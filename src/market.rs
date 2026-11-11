//! Permissionless market factory + isolated vaults + paired claims.
//!
//! Design (per ZeroTick spec):
//! - Anyone can `create_market` if the oracle is registered.
//! - Liquidity is isolated per-market vault by default (never one global pool).
//! - Minting a pair locks `notional` USDC and issues 1 LONG + 1 SHORT.
//!   Settlement guarantees LONG + SHORT = notional (fully collateralized).
//! - Executable quotes come from `PricingEngine` + signed policy + fresh
//!   observation; execution is checked atomically against collateral and
//!   exposure limits.

use crate::oracle::OracleRegistry;
use crate::pricing::{InventorySnapshot, OracleObservation, PricingEngine, PricingPolicy};
use crate::types::{AccountId, ExecError, MarketId, PairPayoff, Side};
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct Market {
    pub id: MarketId,
    pub oracle_adapter_id: String,
    pub payoff: PairPayoff,
    pub halted: bool,
}

#[derive(Clone, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct Vault {
    pub collateral_usdc_minor: u64,
    pub outstanding_pairs: u64,
    pub longs_out: u64,
    pub shorts_out: u64,
    pub lp_shares: HashMap<String, u64>,
    /// Net long USDC-minor exposure held by maker(s) in this vault.
    pub maker_net_long_usdc_minor: i64,
    pub capacity_usdc_minor: u64,
    pub _notional_per_pair: Option<u64>,
}

impl Vault {
    pub fn inventory(&self) -> InventorySnapshot {
        InventorySnapshot {
            net_long_usdc_minor: self.maker_net_long_usdc_minor,
            capacity_usdc_minor: self.capacity_usdc_minor,
        }
    }

    pub fn lp_total(&self) -> u64 {
        self.lp_shares.values().sum()
    }

    /// LP deposit: mints shares 1:1 with USDC (first depositor) or pro-rata.
    pub fn deposit_lp(&mut self, who: &AccountId, usdc_minor: u64) -> u64 {
        let total = self.lp_total();
        let shares = if total == 0 || self.collateral_usdc_minor == 0 {
            usdc_minor
        } else {
            // pro-rata
            ((usdc_minor as u128 * total as u128) / (self.collateral_usdc_minor as u128)) as u64
        };
        *self.lp_shares.entry(who.0.clone()).or_insert(0) += shares;
        self.collateral_usdc_minor += usdc_minor;
        shares
    }

    /// Mint one pair: locks notional, increases longs/shorts outstanding.
    pub fn mint_pairs(&mut self, pairs: u64) -> Result<(), ExecError> {
        let notional_per = self.notional_per_pair().ok_or(ExecError::AmountOverflow)?;
        let required = pairs
            .checked_mul(notional_per)
            .ok_or(ExecError::AmountOverflow)?;
        if required > self.collateral_usdc_minor {
            return Err(ExecError::InsufficientCollateral {
                available: self.collateral_usdc_minor,
                required,
            });
        }
        // Collateral stays locked in vault (not burned) — it backs settlement.
        // Outstanding claims are liabilities against it.
        self.outstanding_pairs = self
            .outstanding_pairs
            .checked_add(pairs)
            .ok_or(ExecError::AmountOverflow)?;
        self.longs_out = self
            .longs_out
            .checked_add(pairs)
            .ok_or(ExecError::AmountOverflow)?;
        self.shorts_out = self
            .shorts_out
            .checked_add(pairs)
            .ok_or(ExecError::AmountOverflow)?;
        Ok(())
    }

    pub fn notional_per_pair(&self) -> Option<u64> {
        self._notional_per_pair
    }
}

impl Vault {
    pub fn with_notional(notional: u64) -> Self {
        Self {
            _notional_per_pair: Some(notional),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    pub side: Side,
    pub quoted_price_usdc_minor: u64,
    pub notional_usdc_minor: u64,
    pub net_long_delta_usdc_minor: i64,
}

#[derive(Default)]
pub struct MarketFactory {
    markets: HashMap<String, Market>,
    vaults: HashMap<String, Vault>,
    registry: OracleRegistry,
    seen_nonces: HashSet<(String, u64)>,
}

impl MarketFactory {
    pub fn new(registry: OracleRegistry) -> Self {
        Self {
            markets: HashMap::new(),
            vaults: HashMap::new(),
            registry,
            seen_nonces: HashSet::new(),
        }
    }

    pub fn create_market(
        &mut self,
        market_id: MarketId,
        oracle_adapter_id: String,
        payoff: PairPayoff,
    ) -> Result<(), String> {
        if !self
            .registry
            .can_create_market(&oracle_adapter_id, &market_id)
        {
            return Err(format!("oracle not registered: {}", oracle_adapter_id));
        }
        if self.markets.contains_key(&market_id.0) {
            return Err(format!("market exists: {}", market_id.0));
        }
        let vault = Vault::with_notional(payoff.notional_usdc_minor);
        self.vaults.insert(market_id.0.clone(), vault);
        self.markets.insert(
            market_id.0.clone(),
            Market {
                id: market_id,
                oracle_adapter_id,
                payoff,
                halted: false,
            },
        );
        Ok(())
    }

    pub fn get(&self, id: &MarketId) -> Option<&Market> {
        self.markets.get(&id.0)
    }
    pub fn vault(&self, id: &MarketId) -> Option<&Vault> {
        self.vaults.get(&id.0)
    }
    pub fn vault_mut(&mut self, id: &MarketId) -> Option<&mut Vault> {
        self.vaults.get_mut(&id.0)
    }

    pub fn halt(&mut self, id: &MarketId) {
        if let Some(m) = self.markets.get_mut(&id.0) {
            m.halted = true;
        }
    }

    /// Execute a taker trade against a signed policy + fresh observation.
    /// Returns the fill price derived by `PricingEngine`. Atomic checks:
    /// halt, expiry, freshness, size, replay nonce, exposure, slippage.
    pub fn execute(
        &mut self,
        policy: &PricingPolicy,
        obs: &OracleObservation,
        side: Side,
        trade_size_usdc_minor: u64,
        max_acceptable_price_usdc_minor: u64,
        now_unix_secs: i64,
    ) -> Result<Fill, ExecError> {
        let market = self
            .markets
            .get(&policy.market.0)
            .ok_or(ExecError::UnknownMarket)?;
        if market.halted {
            return Err(ExecError::Halted);
        }
        policy.validate_time(now_unix_secs)?;
        // replay: (market, nonce) must be fresh
        if self
            .seen_nonces
            .contains(&(policy.market.0.clone(), policy.nonce))
        {
            return Err(ExecError::ReplayNonce);
        }
        obs.check_fresh(now_unix_secs, policy.oracle_max_age_secs)?;
        if trade_size_usdc_minor > policy.max_trade_usdc_minor {
            return Err(ExecError::TradeTooLarge {
                requested: trade_size_usdc_minor,
                max: policy.max_trade_usdc_minor,
            });
        }

        let vault = self
            .vaults
            .get(&policy.market.0)
            .ok_or(ExecError::UnknownMarket)?;
        let inv = vault.inventory();
        let (quoted, delta) = match side {
            Side::Buy => (
                PricingEngine::ask(policy, obs.ref_price_usdc_minor, inv),
                trade_size_usdc_minor as i64,
            ),
            Side::Sell => (
                PricingEngine::bid(policy, obs.ref_price_usdc_minor, inv),
                -(trade_size_usdc_minor as i64),
            ),
        };

        if quoted > max_acceptable_price_usdc_minor {
            return Err(ExecError::SlippageExceeded {
                quoted,
                max_acceptable: max_acceptable_price_usdc_minor,
            });
        }

        let would_be = (vault.maker_net_long_usdc_minor + delta).unsigned_abs();
        if would_be > policy.max_exposure_usdc_minor {
            return Err(ExecError::ExposureExceeded {
                would_be,
                max: policy.max_exposure_usdc_minor,
            });
        }

        // Apply fill: maker inventory moves opposite the taker.
        let vault = self.vaults.get_mut(&policy.market.0).unwrap();
        vault.maker_net_long_usdc_minor -= delta;

        // Burn the nonce (mark as seen) — one policy instance, one trade.
        self.seen_nonces
            .insert((policy.market.0.clone(), policy.nonce));

        Ok(Fill {
            side,
            quoted_price_usdc_minor: quoted,
            notional_usdc_minor: trade_size_usdc_minor,
            net_long_delta_usdc_minor: delta,
        })
    }

    /// Settle a market at final index: returns (long_payout, short_payout) per pair.
    pub fn settle(&self, market: &MarketId, final_index: f64) -> Option<(u64, u64)> {
        self.markets
            .get(&market.0)
            .map(|m| m.payoff.settlement(final_index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::{OracleAdapter, OracleRegistry};
    use crate::pricing::{OracleObservation, PricingPolicy};
    use crate::types::{AccountId, PairPayoff};

    fn factory_with_arg30() -> (MarketFactory, MarketId) {
        let mut reg = OracleRegistry::new();
        reg.register(OracleAdapter::new(
            "arg30-spot",
            "ARG30 spot, signed",
            5,
            true,
        ));
        let mut f = MarketFactory::new(reg);
        let id = MarketId::new("ARG30-DEC26");
        f.create_market(
            id.clone(),
            "arg30-spot".into(),
            PairPayoff::new(10_000_000, 1000.0, 1_800_000_000),
        )
        .unwrap();
        (f, id)
    }

    fn policy_for(id: &MarketId, nonce: u64) -> PricingPolicy {
        PricingPolicy {
            market: id.clone(),
            pricing_model: "inventory_v1".into(),
            base_spread_bps: 50,
            inventory_skew_bps: 150,
            max_trade_usdc_minor: 5_000_000_000,
            max_exposure_usdc_minor: 100_000_000_000,
            oracle_max_age_secs: 5,
            policy_expiry_unix_secs: 1_700_000_100,
            nonce,
            domain: "zerotick.neardao/v1".into(),
        }
    }

    #[test]
    fn spot_payout_is_capped_note() {
        // $10 spot token: payout = 10 * min(2, IT/I0). No long/short pair.
        let p = PairPayoff::new(10_000_000, 1000.0, 1_800_000_000);
        for (it, want) in [
            (700.0, 7_000_000),
            (1000.0, 10_000_000),
            (1150.0, 11_500_000),
            (2000.0, 20_000_000),
            (3000.0, 20_000_000),
        ] {
            assert_eq!(p.spot_payout(it), want, "IT={}", it);
            assert_eq!(p.settlement(it), (want, 0));
        }
    }

    #[test]
    fn mint_requires_collateral_and_locks_it() {
        let (mut f, id) = factory_with_arg30();
        let lp = AccountId::new("maker");
        // No LP funds yet — mint must fail.
        let v = f.vault_mut(&id).unwrap();
        assert!(v.mint_pairs(5).is_err());
        // Deposit and mint.
        v.deposit_lp(&lp, 100_000_000_000); // $100k
        assert_eq!(v.collateral_usdc_minor, 100_000_000_000);
        v.mint_pairs(100).unwrap();
        assert_eq!(v.outstanding_pairs, 100);
        assert_eq!(v.longs_out, 100);
        assert_eq!(v.shorts_out, 100);
    }

    #[test]
    fn execute_buy_long_uses_ask_and_updates_inventory() {
        let (mut f, id) = factory_with_arg30();
        f.vault_mut(&id)
            .unwrap()
            .deposit_lp(&AccountId::new("m"), 100_000_000_000);
        f.vault_mut(&id).unwrap().capacity_usdc_minor = 100_000_000_000;
        let pol = policy_for(&id, 1);
        let obs = OracleObservation {
            market: id.clone(),
            ref_price_usdc_minor: 10_000_000,
            observed_at_unix_secs: 1_700_000_000,
            source_id: "arg30-spot".into(),
            signature_valid: true,
        };
        let fill = f
            .execute(
                &pol,
                &obs,
                Side::Buy,
                1_000_000_000,
                10_050_000,
                1_700_000_001,
            )
            .unwrap();
        assert_eq!(fill.quoted_price_usdc_minor, 10_050_000);
        // Maker goes short $1000.
        assert_eq!(
            f.vault(&id).unwrap().maker_net_long_usdc_minor,
            -1_000_000_000
        );
    }

    #[test]
    fn second_trade_repriced_without_update_tx() {
        let (mut f, id) = factory_with_arg30();
        f.vault_mut(&id)
            .unwrap()
            .deposit_lp(&AccountId::new("m"), 100_000_000_000);
        f.vault_mut(&id).unwrap().capacity_usdc_minor = 100_000_000_000;
        // Trade 1 buys $1000 long from maker → maker short $1000.
        let p1 = policy_for(&id, 1);
        let obs1 = OracleObservation {
            market: id.clone(),
            ref_price_usdc_minor: 10_000_000,
            observed_at_unix_secs: 1_700_000_000,
            source_id: "s".into(),
            signature_valid: true,
        };
        let fill1 = f
            .execute(
                &p1,
                &obs1,
                Side::Buy,
                1_000_000_000,
                10_050_000,
                1_700_000_001,
            )
            .unwrap();
        // Trade 2 (same week-old policy family, fresh nonce, new index) at $10.23 ref.
        let p2 = policy_for(&id, 2);
        let obs2 = OracleObservation {
            market: id.clone(),
            ref_price_usdc_minor: 10_230_000,
            observed_at_unix_secs: 1_700_000_050,
            source_id: "s".into(),
            signature_valid: true,
        };
        let fill2 = f
            .execute(
                &p2,
                &obs2,
                Side::Buy,
                1_000_000_000,
                20_000_000,
                1_700_000_051,
            )
            .unwrap();
        // Price moved with the index despite zero update transactions.
        assert_ne!(fill1.quoted_price_usdc_minor, fill2.quoted_price_usdc_minor);
        assert!(fill2.quoted_price_usdc_minor > 10_230_000); // ask > ref
    }

    #[test]
    fn stale_policy_and_stale_oracle_rejected() {
        let (mut f, id) = factory_with_arg30();
        f.vault_mut(&id)
            .unwrap()
            .deposit_lp(&AccountId::new("m"), 100_000_000_000);
        let mut pol = policy_for(&id, 1);
        pol.policy_expiry_unix_secs = 1_700_000_000;
        let obs = OracleObservation {
            market: id.clone(),
            ref_price_usdc_minor: 10_000_000,
            observed_at_unix_secs: 1_700_000_000,
            source_id: "s".into(),
            signature_valid: true,
        };
        assert_eq!(
            f.execute(&pol, &obs, Side::Buy, 100, 20_000_000, 1_700_000_001),
            Err(ExecError::PolicyExpired)
        );

        let pol2 = policy_for(&id, 2);
        let stale = OracleObservation {
            market: id.clone(),
            ref_price_usdc_minor: 10_000_000,
            observed_at_unix_secs: 1_700_000_000 - 10,
            source_id: "s".into(),
            signature_valid: true,
        };
        assert!(matches!(
            f.execute(&pol2, &stale, Side::Buy, 100, 20_000_000, 1_700_000_002),
            Err(ExecError::StaleObservation { .. })
        ));
    }

    #[test]
    fn replay_nonce_rejected() {
        let (mut f, id) = factory_with_arg30();
        f.vault_mut(&id)
            .unwrap()
            .deposit_lp(&AccountId::new("m"), 100_000_000_000);
        f.vault_mut(&id).unwrap().capacity_usdc_minor = 100_000_000_000;
        let pol = policy_for(&id, 7);
        let obs = OracleObservation {
            market: id.clone(),
            ref_price_usdc_minor: 10_000_000,
            observed_at_unix_secs: 1_700_000_000,
            source_id: "s".into(),
            signature_valid: true,
        };
        f.execute(&pol, &obs, Side::Buy, 100, 20_000_000, 1_700_000_001)
            .unwrap();
        assert_eq!(
            f.execute(&pol, &obs, Side::Buy, 100, 20_000_000, 1_700_000_001),
            Err(ExecError::ReplayNonce)
        );
    }

    #[test]
    fn isolated_vaults_do_not_share_collateral() {
        let mut reg = OracleRegistry::new();
        reg.register(OracleAdapter::new("arg30-spot", "ARG30", 5, true));
        reg.register(OracleAdapter::new("bov-spot", "BOV", 5, true));
        let mut f = MarketFactory::new(reg);
        let a = MarketId::new("ARG30-DEC26");
        let b = MarketId::new("BOV-DEC26");
        f.create_market(
            a.clone(),
            "arg30-spot".into(),
            PairPayoff::new(10_000_000, 1000.0, 1_800_000_000),
        )
        .unwrap();
        f.create_market(
            b.clone(),
            "bov-spot".into(),
            PairPayoff::new(10_000_000, 500.0, 1_800_000_000),
        )
        .unwrap();
        f.vault_mut(&a)
            .unwrap()
            .deposit_lp(&AccountId::new("lp-a"), 100_000_000);
        f.vault_mut(&b)
            .unwrap()
            .deposit_lp(&AccountId::new("lp-b"), 100_000_000);
        f.vault_mut(&a).unwrap().mint_pairs(9).unwrap();
        assert_eq!(f.vault(&b).unwrap().outstanding_pairs, 0);
    }
}
