//! Executable pricing policy — the core ZeroTick primitive.
//!
//! A maker signs a `PricingPolicy` once; the contract re-derives the live
//! quote for every trade as `P_ref * (1 ± s + λ·x)` where `x` is the maker's
//! normalized net short exposure in `[-1, 1]`. No quote updates hit chain.

use crate::types::{ExecError, MarketId};
use borsh::{BorshDeserialize, BorshSerialize};

/// Signed, short-lived pricing policy. The contract checks domain + nonce
/// for replay protection (caller supplies domain/nonce validation).
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct PricingPolicy {
    pub market: MarketId,
    pub pricing_model: String,   // e.g. "inventory_v1"
    pub base_spread_bps: u16,    // e.g. 50 = 0.5%
    pub inventory_skew_bps: u16, // λ in bps at full short, e.g. 150
    pub max_trade_usdc_minor: u64,
    pub max_exposure_usdc_minor: u64,
    pub oracle_max_age_secs: i64,
    pub policy_expiry_unix_secs: i64,
    pub nonce: u64,
    /// Contract domain string bound into the signature (e.g. "zerotick.neardao/v1").
    pub domain: String,
}

impl PricingPolicy {
    pub fn validate_time(&self, now_unix_secs: i64) -> Result<(), ExecError> {
        if now_unix_secs > self.policy_expiry_unix_secs {
            return Err(ExecError::PolicyExpired);
        }
        Ok(())
    }
}

/// Fresh, independently signed reference observation.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct OracleObservation {
    pub market: MarketId,
    pub ref_price_usdc_minor: u64, // reference price per unit exposure (minor units)
    pub observed_at_unix_secs: i64,
    pub source_id: String,
    pub signature_valid: bool,
}

impl OracleObservation {
    pub fn age(&self, now_unix_secs: i64) -> i64 {
        now_unix_secs - self.observed_at_unix_secs
    }
    pub fn check_fresh(&self, now_unix_secs: i64, max_age_secs: i64) -> Result<(), ExecError> {
        let a = self.age(now_unix_secs);
        if a < 0 || a > max_age_secs {
            return Err(ExecError::StaleObservation {
                age_secs: a,
                max_age: max_age_secs,
            });
        }
        if !self.signature_valid {
            return Err(ExecError::BadSignature);
        }
        Ok(())
    }
}

/// Maker inventory snapshot used for skew.
#[derive(Clone, Copy, Debug, BorshSerialize, BorshDeserialize)]
pub struct InventorySnapshot {
    /// Positive = maker is net long USDC exposure (worried), negative = net short.
    /// `x = -net_long / capacity` is the "how short" measure from the paper.
    pub net_long_usdc_minor: i64,
    pub capacity_usdc_minor: u64,
}

impl InventorySnapshot {
    /// Normalized shortness in [-1, 1]. Positive means maker is short.
    pub fn shortness_x(&self) -> f64 {
        if self.capacity_usdc_minor == 0 {
            return 0.0;
        }
        let x = -(self.net_long_usdc_minor as f64) / (self.capacity_usdc_minor as f64);
        x.clamp(-1.0, 1.0)
    }
}

/// Executable pricing engine. Pure function — same inputs always yield the
/// same quote, which is what makes off-chain repricing trustless.
pub struct PricingEngine;

impl PricingEngine {
    /// Ask price (maker sells to taker who buys): P_ref * (1 + s + λ·x)
    pub fn ask(policy: &PricingPolicy, p_ref: u64, inv: InventorySnapshot) -> u64 {
        let x = inv.shortness_x();
        let s = policy.base_spread_bps as f64 / 10_000.0;
        let lambda = policy.inventory_skew_bps as f64 / 10_000.0;
        (p_ref as f64 * (1.0 + s + lambda * x)).round() as u64
    }

    /// Bid price: P_ref * (1 - s + λ·x)
    pub fn bid(policy: &PricingPolicy, p_ref: u64, inv: InventorySnapshot) -> u64 {
        let x = inv.shortness_x();
        let s = policy.base_spread_bps as f64 / 10_000.0;
        let lambda = policy.inventory_skew_bps as f64 / 10_000.0;
        let v = p_ref as f64 * (1.0 - s + lambda * x);
        v.round().max(0.0) as u64
    }

    /// Spread in minor units at current inventory.
    pub fn spread(policy: &PricingPolicy, p_ref: u64, inv: InventorySnapshot) -> u64 {
        Self::ask(policy, p_ref, inv).saturating_sub(Self::bid(policy, p_ref, inv))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MarketId;

    fn policy() -> PricingPolicy {
        PricingPolicy {
            market: MarketId::new("ARG30-USDC"),
            pricing_model: "inventory_v1".into(),
            base_spread_bps: 50,
            inventory_skew_bps: 150,
            max_trade_usdc_minor: 5_000_000_000,
            max_exposure_usdc_minor: 100_000_000_000,
            oracle_max_age_secs: 5,
            policy_expiry_unix_secs: 1_700_000_000,
            nonce: 42,
            domain: "zerotick.neardao/v1".into(),
        }
    }

    #[test]
    fn flat_inventory_matches_base_spread() {
        let p = policy();
        let inv = InventorySnapshot {
            net_long_usdc_minor: 0,
            capacity_usdc_minor: 100_000_000_000,
        };
        let r = 10_000_000u64; // $10
        assert_eq!(PricingEngine::ask(&p, r, inv), 10_050_000); // 10 * 1.005
        assert_eq!(PricingEngine::bid(&p, r, inv), 9_950_000); // 10 * 0.995
    }

    #[test]
    fn short_maker_raises_both_quotes() {
        let p = policy();
        let r = 10_000_000u64;
        // Fully short: x = +1
        let short = InventorySnapshot {
            net_long_usdc_minor: -100_000_000_000,
            capacity_usdc_minor: 100_000_000_000,
        };
        let ask_s = PricingEngine::ask(&p, r, short);
        let bid_s = PricingEngine::bid(&p, r, short);
        // Flat baseline
        let flat = InventorySnapshot {
            net_long_usdc_minor: 0,
            capacity_usdc_minor: 100_000_000_000,
        };
        let ask_f = PricingEngine::ask(&p, r, flat);
        // Short maker must quote higher than flat on both sides.
        assert!(ask_s > ask_f);
        assert!(bid_s > PricingEngine::bid(&p, r, flat));
        assert_eq!(ask_s, 10_200_000); // 1 + 0.005 + 0.015
        assert_eq!(bid_s, 10_100_000);
    }

    #[test]
    fn long_maker_lowers_both_quotes() {
        let p = policy();
        let r = 10_000_000u64;
        let long = InventorySnapshot {
            net_long_usdc_minor: 100_000_000_000,
            capacity_usdc_minor: 100_000_000_000,
        };
        assert_eq!(PricingEngine::ask(&p, r, long), 9_900_000);
        assert_eq!(PricingEngine::bid(&p, r, long), 9_800_000);
    }

    #[test]
    fn skew_clamps_at_capacity() {
        let p = policy();
        let r = 10_000_000u64;
        // 10x over capacity still clamps to x=1.
        let over = InventorySnapshot {
            net_long_usdc_minor: -1_000_000_000_000,
            capacity_usdc_minor: 100_000_000_000,
        };
        let at = InventorySnapshot {
            net_long_usdc_minor: -100_000_000_000,
            capacity_usdc_minor: 100_000_000_000,
        };
        assert_eq!(
            PricingEngine::ask(&p, r, over),
            PricingEngine::ask(&p, r, at)
        );
    }
}
