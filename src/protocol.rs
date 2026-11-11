//! ZeroTick protocol v2 — shared vault, one open formula per market, positions.
//!
//! Model (canonical):
//! - ONE shared USDC vault backs every market (capital efficiency). Each
//!   market has its own exposure cap so no single synthetic can drain it.
//! - ONE formula per market, chosen at creation from an open registry of
//!   audited formula kinds. `IndexCappedV1` ships first; new kinds plug in
//!   behind the same `Formula` interface without touching vault accounting.
//! - Traders BUY / SELL a spot synthetic position (e.g. `sARG30-DEC26`).
//!   Buy = pay USDC, position minted to wallet. Sell = position closed, USDC paid.
//!   At maturity the position redeems for the formula payoff. No long/short,
//!   no leverage, no liquidations.
//! - Every trade carries an `OracleBundle`: spot level, last-1m deviation,
//!   timestamp, monotonic sequence, signer. The contract checks the
//!   SIGNATURE against the market's registered oracle signer, freshness,
//!   sequence and deviation circuit-breaker — then prices and settles.
//! - TIME IS A PRICING INPUT: the older the oracle data, the worse the
//!   price. Effective half-spread grows with age and with 1m deviation:
//!
//!   ```text
//!   half_spread_bps = base + staleness_bps_per_sec * age_secs
//!                         + deviation_bps_1m / vol_divisor
//!   ask = fair * (1 + half_spread + skew * x)
//!   bid = fair * (1 - half_spread + skew * x)
//!   ```
//!   Best price only for data <= ~1s old; hard-reject past `max_age_secs`.

use crate::oracle::{OracleAdapter, OracleRegistry};
use crate::types::{AccountId, ExecError, MarketId, VaultId};
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::HashMap;

// ---------- Oracle bundle ----------

/// Signed oracle payload carried by the trade transaction.
/// In production `signature_valid` is the result of verifying the signer's
/// ed25519 signature over (domain, market, spot, deviation, observed_at, seq).
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct OracleBundle {
    pub market: MarketId,
    /// Index / spot level in the oracle's native units (e.g. Merval points).
    pub spot: f64,
    /// Realized deviation (stdev) of the last 60s, in bps. High = don't trust.
    pub deviation_bps_1m: u16,
    pub observed_at_unix_secs: i64,
    /// Milliseconds component folded in: total timestamp in ms for age maths.
    pub observed_at_ms: i64,
    /// Monotonic round id per market+signer (replay protection).
    pub seq: u64,
    pub signer: String,
    pub signature_valid: bool,
}

impl OracleBundle {
    pub fn age_secs(&self, now_ms: i64) -> f64 {
        (now_ms - self.observed_at_ms) as f64 / 1000.0
    }
}

// ---------- Formula (open registry, one per market) ----------

/// Audited formula kinds. New kinds can be added behind this interface;
/// vault accounting (mint/burn/reserve) never changes.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub enum FormulaKind {
    /// fair = base * (spot / inception), payout capped at cap_mult x base.
    IndexCappedV1,
    /// fair = spot already denominated in USDC minor units per position.
    DirectV1,
    /// Binary event (odds): oracle `spot` is a probability in [0, 1].
    /// fair = payout * p  (implied decimal odds = payout / fair).
    /// At resolution `spot` is 1 (YES) or 0 (NO): position pays `base` or 0.
    BinaryV1,
    /// Synthetic spot FX (fully funded, 1x — no funding, no leverage, no liquidation): fair = base * (spot / inception), uncapped, no maturity.
    /// Exit is always sell-at-bid; `cap_mult` is only the vault's stress
    /// reserve multiplier (reserve as if the rate moved cap x), not a payout cap.
    SyntheticSpotV1,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct Formula {
    pub kind: FormulaKind,
    /// Position base price in USDC minor units at inception (e.g. 10_000_000 = $10).
    pub base_usdc_minor: u64,
    pub inception: f64,
    /// e.g. 2.0 → max payout = 2 * base.
    pub cap_mult_x1000: u32,
    pub base_spread_bps: u16,
    /// Inventory skew at full market capacity, in bps.
    pub skew_bps: u16,
    /// Extra half-spread per second of oracle age, in bps (time decay).
    pub staleness_bps_per_sec: u16,
    /// Extra half-spread per day of position duration, in bps.
    pub tenor_spread_bps_per_day: u16,
    /// half_spread += deviation_bps_1m / vol_divisor.
    pub vol_divisor: u16,
    /// Hard reject if older than this.
    pub max_age_secs: i64,
    /// Hard reject if 1m deviation exceeds this (circuit breaker).
    pub max_deviation_bps: u16,
    pub max_trade_usdc_minor: u64,
    /// Per-market reserved-collateral cap (shared-vault drain protection).
    pub max_market_reserved_usdc_minor: u64,
    pub maturity_unix_secs: i64,
}

impl Formula {
    pub fn index_capped_v1(base: u64, inception: f64, maturity: i64) -> Self {
        Self {
            kind: FormulaKind::IndexCappedV1,
            base_usdc_minor: base,
            inception,
            cap_mult_x1000: 2000,
            base_spread_bps: 50,
            skew_bps: 150,
            staleness_bps_per_sec: 25,
            tenor_spread_bps_per_day: 5,
            vol_divisor: 2,
            max_age_secs: 10,
            max_deviation_bps: 500,
            max_trade_usdc_minor: 5_000_000_000,
            max_market_reserved_usdc_minor: 25_000_000_000,
            maturity_unix_secs: maturity,
        }
    }

    /// Binary event paying `payout` USDC minor units if it resolves YES.
    pub fn binary_v1(payout_usdc_minor: u64, maturity: i64) -> Self {
        Self {
            kind: FormulaKind::BinaryV1,
            base_usdc_minor: payout_usdc_minor,
            inception: 1.0,
            cap_mult_x1000: 1000,
            base_spread_bps: 80,
            skew_bps: 200,
            staleness_bps_per_sec: 25,
            tenor_spread_bps_per_day: 0,
            vol_divisor: 2,
            max_age_secs: 10,
            max_deviation_bps: 500,
            max_trade_usdc_minor: 5_000_000_000,
            max_market_reserved_usdc_minor: 25_000_000_000,
            maturity_unix_secs: maturity,
        }
    }

    /// Synthetic spot FX market: never expires (maturity = i64::MAX).
    pub fn synthetic_spot_v1(base: u64, inception: f64) -> Self {
        Self {
            kind: FormulaKind::SyntheticSpotV1,
            base_usdc_minor: base,
            inception,
            cap_mult_x1000: 2000, // stress-reserve multiplier only
            base_spread_bps: 30,
            skew_bps: 120,
            staleness_bps_per_sec: 25,
            tenor_spread_bps_per_day: 5,
            vol_divisor: 2,
            max_age_secs: 10,
            max_deviation_bps: 500,
            max_trade_usdc_minor: 5_000_000_000,
            max_market_reserved_usdc_minor: 25_000_000_000,
            maturity_unix_secs: i64::MAX,
        }
    }

    /// Implied decimal odds for a binary price (e.g. 2.63 for $3.80 on a $10 payout).
    pub fn implied_odds(&self, price_usdc_minor: u64) -> f64 {
        if price_usdc_minor == 0 {
            f64::INFINITY
        } else {
            self.max_payout() as f64 / price_usdc_minor as f64
        }
    }

    /// Creation-time validation. Every market-creation path must call this
    /// so a hostile or mistyped formula can never reach pricing.
    pub fn validate(&self) -> Result<(), ExecError> {
        if self.base_usdc_minor == 0
            || self.cap_mult_x1000 == 0
            || !self.inception.is_finite()
            || self.inception <= 0.0
            || self.max_age_secs <= 0
            || self.max_trade_usdc_minor == 0
            || self.max_market_reserved_usdc_minor == 0
            || self.maturity_unix_secs == 0
            || self.maturity_unix_secs < 0
                && self.maturity_unix_secs != i64::MIN
                && !matches!(self.kind, FormulaKind::SyntheticSpotV1)
        {
            return Err(ExecError::InvalidFormula);
        }
        // Expiring kinds need a real future maturity sentinel check happens
        // against chain time at the contract boundary; here we only reject
        // non-positive maturities for expiring kinds.
        if !matches!(self.kind, FormulaKind::SyntheticSpotV1) && self.maturity_unix_secs <= 0 {
            return Err(ExecError::InvalidFormula);
        }
        Ok(())
    }

    /// Fair (mid) price per position, USDC minor units, before spread/skew.
    /// Never panics: non-finite or negative spots and invalid inception
    /// price at zero (trading paths reject them earlier in validation).
    pub fn fair(&self, spot: f64) -> u64 {
        if !spot.is_finite() || spot < 0.0 {
            return 0;
        }
        match self.kind {
            FormulaKind::IndexCappedV1 => {
                if !self.inception.is_finite() || self.inception <= 0.0 {
                    return 0;
                }
                let cap = self.cap_mult_x1000 as f64 / 1000.0;
                let ratio = (spot / self.inception).clamp(0.0, cap);
                (self.base_usdc_minor as f64 * ratio).round() as u64
            }
            FormulaKind::DirectV1 => spot.round().max(0.0) as u64,
            FormulaKind::BinaryV1 => {
                (self.base_usdc_minor as f64 * spot.clamp(0.0, 1.0)).round() as u64
            }
            FormulaKind::SyntheticSpotV1 => {
                if !self.inception.is_finite() || self.inception <= 0.0 {
                    return 0;
                }
                (self.base_usdc_minor as f64 * (spot / self.inception).max(0.0)).round() as u64
            }
        }
    }

    /// Payout per position at maturity. For binary markets `final_spot` is the
    /// resolution (1 = YES, 0 = NO), not a trading probability.
    pub fn payoff(&self, final_spot: f64) -> u64 {
        match self.kind {
            FormulaKind::BinaryV1 => {
                if final_spot >= 0.5 {
                    self.base_usdc_minor
                } else {
                    0
                }
            }
            _ => self.fair(final_spot),
        }
    }

    pub fn max_payout(&self) -> u64 {
        (self.base_usdc_minor as u128 * self.cap_mult_x1000 as u128 / 1000) as u64
    }
}

// ---------- Quote with full breakdown ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    pub fair_usdc_minor: u64,
    pub ask_usdc_minor: u64,
    pub bid_usdc_minor: u64,
    pub base_spread_bps: u16,
    pub staleness_bps: f64,
    pub vol_bps: f64,
    pub tenor_bps: f64,
    pub skew_bps: f64,
    pub half_spread_bps: f64,
    pub age_secs: f64,
    pub x: f64,
}

pub fn price(
    formula: &Formula,
    bundle: &OracleBundle,
    market_exposure_usdc_minor: u64,
    market_capacity_usdc_minor: u64,
    now_ms: i64,
) -> Quote {
    price_with_duration(
        formula,
        bundle,
        market_exposure_usdc_minor,
        market_capacity_usdc_minor,
        now_ms,
        0,
    )
}

/// Price a new fixed-duration position. `duration_secs` is the position tenor;
/// longer commitments pay a wider half-spread when the formula specifies a
/// per-day tenor charge. Binary formulas default that charge to zero.
pub fn price_with_duration(
    formula: &Formula,
    bundle: &OracleBundle,
    market_exposure_usdc_minor: u64,
    market_capacity_usdc_minor: u64,
    now_ms: i64,
    duration_secs: i64,
) -> Quote {
    let fair = formula.fair(bundle.spot);
    let age = bundle.age_secs(now_ms).max(0.0);
    let staleness = formula.staleness_bps_per_sec as f64 * age;
    let vol = if formula.vol_divisor == 0 {
        0.0
    } else {
        bundle.deviation_bps_1m as f64 / formula.vol_divisor as f64
    };
    let tenor = formula.tenor_spread_bps_per_day as f64 * (duration_secs.max(0) as f64 / 86_400.0);
    let half = formula.base_spread_bps as f64 + staleness + vol + tenor;
    let x = if market_capacity_usdc_minor == 0 {
        0.0
    } else {
        (market_exposure_usdc_minor as f64 / market_capacity_usdc_minor as f64).clamp(0.0, 1.0)
    };
    let skew_bps = formula.skew_bps as f64 * x;
    let ask = (fair as f64 * (1.0 + half / 10_000.0 + skew_bps / 10_000.0)).round() as u64;
    let bid = (fair as f64 * (1.0 - half / 10_000.0 + skew_bps / 10_000.0))
        .round()
        .max(0.0) as u64;
    Quote {
        fair_usdc_minor: fair,
        ask_usdc_minor: ask,
        bid_usdc_minor: bid,
        base_spread_bps: formula.base_spread_bps,
        staleness_bps: staleness,
        vol_bps: vol,
        tenor_bps: tenor,
        skew_bps,
        half_spread_bps: half,
        age_secs: age,
        x,
    }
}

// ---------- Shared vault ----------

/// Protocol market risk tiers. The percentage is a ceiling on open
/// reference notional for one market relative to vault equity (NAV), not a
/// segregation of collateral and not a guaranteed-loss reserve.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize,
)]
pub enum MarketRiskTier {
    #[default]
    Unrated,
    Tier1,
    Tier2,
    Tier3,
    Tier4,
    Tier5,
}

impl MarketRiskTier {
    pub fn default_allocation_bps(&self) -> u16 {
        match self {
            Self::Unrated => 0,
            Self::Tier1 => 4_000,
            Self::Tier2 => 1_500,
            Self::Tier3 => 1_000,
            Self::Tier4 => 500,
            Self::Tier5 => 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum PositionStatus {
    Open,
    Closed,
    Settled,
}

/// One fixed-duration trade. Lots are not transferable tokens.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct PositionLot {
    pub id: u64,
    pub account: AccountId,
    pub market: MarketId,
    pub units_minor: u64,
    pub principal_usdc_minor: u64,
    pub entry_spot: f64,
    pub entry_price_usdc_minor: u64,
    pub opened_at_ms: i64,
    pub expires_at_ms: i64,
    pub max_payout_usdc_minor: u64,
    pub status: PositionStatus,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct SpotMarket {
    pub id: MarketId,
    pub name: String,
    pub oracle_signer: String,
    pub formula: Formula,
    /// Positions outstanding, in position minor units (1 position = 1_000_000).
    pub units_out_minor: u64,
    pub last_seq: u64,
    pub halted: bool,
    pub risk_tier: MarketRiskTier,
    pub allocation_override_bps: Option<u16>,
    pub last_fair_usdc_minor: u64,
}

impl SpotMarket {
    pub fn effective_allocation_bps(&self) -> u16 {
        self.allocation_override_bps
            .unwrap_or_else(|| self.risk_tier.default_allocation_bps())
    }
}

impl SpotMarket {
    /// Mark-to-market liability at a given fair price.
    pub fn liability(&self, fair: u64) -> u64 {
        ((self.units_out_minor as u128 * fair as u128) / 1_000_000) as u64
    }
    /// Worst-case liability (every position pays the cap).
    pub fn max_liability(&self) -> u64 {
        ((self.units_out_minor as u128 * self.formula.max_payout() as u128) / 1_000_000) as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotFill {
    pub is_buy: bool,
    pub quoted_price_usdc_minor: u64,
    pub usdc_minor: u64,
    pub units_minor: u64,
    pub position_id: Option<u64>,
    pub expires_at_ms: Option<i64>,
}

/// Flat market-creation fee: $10 in USDC minor units.
pub const MARKET_CREATION_FEE_USDC_MINOR: u64 = 10_000_000;

/// Minimum buy size ($1). Dust lots bloat state and settlement scans.
pub const MIN_TRADE_USDC_MINOR: u64 = 1_000_000;

/// A settlement fixing must be observed within this window after expiry.
/// Later prints are lookahead, not the expiry fixing.
pub const SETTLEMENT_WINDOW_MS: i64 = 86_400_000; // 24h

/// A buy whose implied fair jumps more than this multiple versus the last
/// accepted fair is treated as a broken/manipulated print (circuit breaker).
/// Sells and settlements are exempt so exits are never trapped; sells are
/// separately capped at each lot's reserved maximum.
pub const MAX_FAIR_JUMP_MULT: u64 = 3;

/// Canonical protocol-level market registration. This is distinct from a
/// vault enabling a market: registering a market never drafts any vault's
/// capital; each vault decides independently whether to support it.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct MarketDefinition {
    pub id: MarketId,
    pub name: String,
    pub oracle_id: String,
    pub oracle_signer: String,
    /// Payoff economics plus default pricing. Vaults clone this template and
    /// may override market-making parameters, but they may not redefine the
    /// payoff or reference inception in an enablement.
    pub formula_template: Formula,
    pub risk_tier: MarketRiskTier,
    pub halted: bool,
}

/// Vault-specific market-making parameters. These deliberately cannot alter
/// payoff terms such as base amount, inception, cap or maturity.
#[derive(Clone, Copy, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct PricingOverrides {
    pub base_spread_bps: Option<u16>,
    pub skew_bps: Option<u16>,
    pub staleness_bps_per_sec: Option<u16>,
    pub tenor_spread_bps_per_day: Option<u16>,
    pub vol_divisor: Option<u16>,
}

impl PricingOverrides {
    pub fn apply(&self, formula: &mut Formula) {
        if let Some(v) = self.base_spread_bps {
            formula.base_spread_bps = v;
        }
        if let Some(v) = self.skew_bps {
            formula.skew_bps = v;
        }
        if let Some(v) = self.staleness_bps_per_sec {
            formula.staleness_bps_per_sec = v;
        }
        if let Some(v) = self.tenor_spread_bps_per_day {
            formula.tenor_spread_bps_per_day = v;
        }
        if let Some(v) = self.vol_divisor {
            formula.vol_divisor = v;
        }
    }
}

/// Risk and liquidity configuration for one vault. It is deliberately
/// separate from market payoff definitions: allocation percentages cap open
/// reference notional, while solvency checks reserve against each lot's
/// explicit maximum payout.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct VaultConfig {
    pub name: String,
    pub operator: Option<AccountId>,
    /// Gross open reference notional ceiling as a share of NAV. Default 500%.
    /// This may exceed 100% because it is not an allocation or reservation.
    pub max_gross_reference_bps: u32,
    pub withdrawal_epoch_secs: i64,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            name: "Default vault".into(),
            operator: None,
            max_gross_reference_bps: 50_000,
            withdrawal_epoch_secs: 0,
        }
    }
}

#[derive(Clone, Default, BorshSerialize, BorshDeserialize)]
pub struct SharedVault {
    pub id: String,
    pub config: VaultConfig,
    /// Total USDC held (LP deposits + sale proceeds - buyback payouts).
    pub collateral_usdc_minor: u64,
    pub lp_shares: HashMap<String, u64>,
    pub markets: HashMap<String, SpotMarket>,
    /// (account, market) -> position balance in position minor units.
    pub positions: HashMap<(String, String), u64>,
    pub position_lots: HashMap<u64, PositionLot>,
    pub last_bundle: HashMap<String, OracleBundle>,
    pub next_position_id: u64,
    pub next_withdrawal_at_ms: i64,
    /// (account, market) -> requested_at_ms. Holders pre-register redemption
    /// so their lots are settled immediately when the market closes.
    pub redemption_requests: HashMap<(String, String), i64>,
    /// account -> USDC minor owed from bulk market settlement, where payout
    /// is due to a holder who did not initiate the settling transaction.
    pub claimable_usdc_minor: HashMap<String, u64>,
}

impl SharedVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(id: impl Into<String>, config: VaultConfig) -> Self {
        Self {
            id: id.into(),
            config,
            ..Self::default()
        }
    }

    pub fn total_lp_shares(&self) -> u64 {
        // Sum in u128 so hostile balances cannot wrap the total.
        let total: u128 = self.lp_shares.values().map(|v| *v as u128).sum();
        total.min(u64::MAX as u128) as u64
    }

    /// Net asset value in signed minor units: cash reserves minus the
    /// mark-to-market value of outstanding synthetic liabilities.
    pub fn nav_i128(&self) -> i128 {
        let liabilities: u128 = self
            .markets
            .values()
            .map(|m| m.units_out_minor as u128 * m.last_fair_usdc_minor as u128 / 1_000_000)
            .sum();
        self.collateral_usdc_minor as i128 - liabilities as i128
    }

    pub fn nav(&self) -> u64 {
        self.nav_i128().max(0) as u64
    }

    fn nav_with_market_price(&self, market: &MarketId, fair: u64) -> i128 {
        let liabilities: u128 = self
            .markets
            .values()
            .map(|m| {
                let f = if m.id == *market {
                    fair
                } else {
                    m.last_fair_usdc_minor
                };
                m.units_out_minor as u128 * f as u128 / 1_000_000
            })
            .sum();
        self.collateral_usdc_minor as i128 - liabilities as i128
    }

    /// Open reference notional at current fair values. This is a notional
    /// exposure measure, not the amount reserved for worst-case payouts.
    pub fn total_reference_notional(&self) -> u64 {
        self.markets
            .values()
            .map(|m| m.liability(m.last_fair_usdc_minor))
            .sum()
    }

    pub fn deposit_lp(&mut self, who: &AccountId, usdc_minor: u64) -> Result<u64, ExecError> {
        if usdc_minor == 0 {
            return Ok(0);
        }
        let total = self.total_lp_shares();
        let nav_before = self.nav_i128();
        // With shares outstanding but no positive NAV there is no fair price
        // for new shares; minting 1:1 would dilute any recovery. Reject.
        if total > 0 && nav_before <= 0 {
            return Err(ExecError::InsufficientCollateral {
                available: 0,
                required: usdc_minor,
            });
        }
        let shares = if total == 0 {
            usdc_minor
        } else {
            ((usdc_minor as u128 * total as u128) / nav_before as u128) as u64
        };
        if shares == 0 {
            return Err(ExecError::TradeTooSmall {
                requested: usdc_minor,
                min: MIN_TRADE_USDC_MINOR,
            });
        }
        let held = self.lp_shares.get(&who.0).copied().unwrap_or(0);
        let new_held = held.checked_add(shares).ok_or(ExecError::AmountOverflow)?;
        let new_collateral = self
            .collateral_usdc_minor
            .checked_add(usdc_minor)
            .ok_or(ExecError::AmountOverflow)?;
        self.lp_shares.insert(who.0.clone(), new_held);
        self.collateral_usdc_minor = new_collateral;
        Ok(shares)
    }

    /// Redeem LP shares during an open withdrawal epoch. Expired lots must
    /// be settled first so NAV does not depend on settlement fixings that
    /// are no longer represented by `last_fair`.
    pub fn withdraw_lp(
        &mut self,
        who: &AccountId,
        shares: u64,
        now_ms: i64,
    ) -> Result<u64, ExecError> {
        if shares == 0 {
            return Ok(0);
        }
        if now_ms < self.next_withdrawal_at_ms {
            return Err(ExecError::WithdrawalLocked {
                next_withdrawal_at_ms: self.next_withdrawal_at_ms,
            });
        }
        if self
            .position_lots
            .values()
            .any(|lot| lot.status == PositionStatus::Open && lot.expires_at_ms <= now_ms)
        {
            return Err(ExecError::UnsettledExpiredPositions);
        }
        let held = *self.lp_shares.get(&who.0).unwrap_or(&0);
        if shares > held {
            return Err(ExecError::InsufficientCollateral {
                available: held,
                required: shares,
            });
        }
        let total = self.total_lp_shares();
        let nav = self.nav_i128();
        if total == 0 || nav <= 0 {
            return Err(ExecError::InsufficientCollateral {
                available: 0,
                required: shares,
            });
        }
        let payout = ((shares as u128 * nav as u128) / total as u128) as u64;
        let free = self.free_collateral();
        if payout > free {
            return Err(ExecError::InsufficientCollateral {
                available: free,
                required: payout,
            });
        }
        *self.lp_shares.entry(who.0.clone()).or_insert(0) -= shares;
        self.collateral_usdc_minor -= payout;
        if self.config.withdrawal_epoch_secs > 0 {
            self.next_withdrawal_at_ms = now_ms + self.config.withdrawal_epoch_secs * 1000;
        }
        Ok(payout)
    }

    /// Create a market in this shared vault. Costs a flat $10
    /// (`MARKET_CREATION_FEE_USDC_MINOR`), paid into vault collateral
    /// (it backs all markets and accrues to LPs). Underpayment and
    /// duplicates are rejected before any fee is taken.
    pub fn create_market(
        &mut self,
        id: MarketId,
        name: String,
        oracle_signer: String,
        formula: Formula,
        creation_fee_paid_usdc_minor: u64,
    ) -> Result<(), String> {
        if self.markets.contains_key(&id.0) {
            return Err(format!("market exists: {}", id.0));
        }
        formula
            .validate()
            .map_err(|e| format!("invalid formula: {e:?}"))?;
        if creation_fee_paid_usdc_minor < MARKET_CREATION_FEE_USDC_MINOR {
            return Err(format!(
                "creation fee: paid {} < required {} (USDC minor units)",
                creation_fee_paid_usdc_minor, MARKET_CREATION_FEE_USDC_MINOR
            ));
        }
        self.collateral_usdc_minor = self
            .collateral_usdc_minor
            .checked_add(creation_fee_paid_usdc_minor)
            .ok_or_else(|| "collateral overflow".to_string())?;
        let last_fair_usdc_minor = formula.base_usdc_minor;
        self.markets.insert(
            id.0.clone(),
            SpotMarket {
                id,
                name,
                oracle_signer,
                formula,
                units_out_minor: 0,
                last_seq: 0,
                halted: false,
                // Permissionless creation must never draft the shared vault: new
                // markets start Unrated at 0% until an operator enables risk.
                risk_tier: MarketRiskTier::Unrated,
                allocation_override_bps: None,
                last_fair_usdc_minor,
            },
        );
        Ok(())
    }

    /// Enable a protocol-registered market without paying the global market
    /// creation fee again. The vault uses its own cloned pricing formula so
    /// competing vaults can maintain different spreads and risk limits.
    pub fn enable_market(
        &mut self,
        definition: &MarketDefinition,
        pricing_overrides: Option<&PricingOverrides>,
        allocation_override_bps: Option<u16>,
    ) -> Result<(), ExecError> {
        if self.markets.contains_key(&definition.id.0) {
            return Err(ExecError::AlreadyExists);
        }
        let mut formula = definition.formula_template.clone();
        if let Some(overrides) = pricing_overrides {
            overrides.apply(&mut formula);
        }
        formula.validate()?;
        let last_fair_usdc_minor = formula.base_usdc_minor;
        self.markets.insert(
            definition.id.0.clone(),
            SpotMarket {
                id: definition.id.clone(),
                name: definition.name.clone(),
                oracle_signer: definition.oracle_signer.clone(),
                formula,
                units_out_minor: 0,
                last_seq: 0,
                halted: false,
                risk_tier: definition.risk_tier,
                allocation_override_bps,
                last_fair_usdc_minor,
            },
        );
        Ok(())
    }

    pub fn set_market_allocation(
        &mut self,
        market: &MarketId,
        allocation_override_bps: Option<u16>,
    ) -> Result<(), ExecError> {
        let market = self
            .markets
            .get_mut(&market.0)
            .ok_or(ExecError::MarketNotEnabled)?;
        market.allocation_override_bps = allocation_override_bps;
        Ok(())
    }

    /// Rotate a market's oracle signer identity (contract pairs this with a
    /// new registered pubkey). Sequence resets so the new signer starts fresh.
    pub fn set_oracle_signer(
        &mut self,
        market: &MarketId,
        oracle_signer: String,
    ) -> Result<(), ExecError> {
        let m = self
            .markets
            .get_mut(&market.0)
            .ok_or(ExecError::MarketNotEnabled)?;
        m.oracle_signer = oracle_signer;
        m.last_seq = 0;
        self.last_bundle.remove(&market.0);
        Ok(())
    }

    pub fn sync_market_tier(&mut self, market: &MarketId, tier: MarketRiskTier) {
        if let Some(market) = self.markets.get_mut(&market.0) {
            market.risk_tier = tier;
        }
    }

    /// Worst-case reserved collateral across all open position lots.
    pub fn total_reserved(&self) -> u64 {
        let total: u128 = if self.position_lots.is_empty() {
            self.markets
                .values()
                .map(|m| m.max_liability() as u128)
                .sum()
        } else {
            self.position_lots
                .values()
                .filter(|lot| lot.status == PositionStatus::Open)
                .map(|lot| lot.max_payout_usdc_minor as u128)
                .sum()
        };
        total.min(u64::MAX as u128) as u64
    }
    pub fn free_collateral(&self) -> u64 {
        self.collateral_usdc_minor
            .saturating_sub(self.total_reserved())
    }
    /// collateral / max liability; f64::INFINITY when nothing minted.
    pub fn coverage(&self) -> f64 {
        let r = self.total_reserved();
        if r == 0 {
            f64::INFINITY
        } else {
            self.collateral_usdc_minor as f64 / r as f64
        }
    }
    pub fn position(&self, who: &AccountId, market: &MarketId) -> u64 {
        *self
            .positions
            .get(&(who.0.clone(), market.0.clone()))
            .unwrap_or(&0)
    }

    /// Contract gate: signature, signer, freshness, sequence, deviation breaker.
    pub fn validate_bundle(&self, bundle: &OracleBundle, now_ms: i64) -> Result<(), ExecError> {
        let m = self
            .markets
            .get(&bundle.market.0)
            .ok_or(ExecError::UnknownMarket)?;
        if m.halted {
            return Err(ExecError::Halted);
        }
        if !bundle.signature_valid || bundle.signer != m.oracle_signer {
            return Err(ExecError::BadSignature);
        }
        if !bundle.spot.is_finite() || bundle.spot <= 0.0 {
            return Err(ExecError::InvalidSpot);
        }
        if matches!(m.formula.kind, FormulaKind::BinaryV1) && bundle.spot > 1.0 {
            return Err(ExecError::InvalidSpot);
        }
        // Sequence is monotonic. The identical public bundle may be reused
        // until superseded (many traders share one oracle round), but a
        // *different* bundle carrying an already-seen seq is replay/forgery.
        if bundle.seq < m.last_seq {
            return Err(ExecError::ReplayNonce);
        }
        if bundle.seq == m.last_seq
            && m.last_seq > 0
            && let Some(prev) = self.last_bundle.get(&bundle.market.0)
        {
            let identical = prev.spot.to_bits() == bundle.spot.to_bits()
                && prev.deviation_bps_1m == bundle.deviation_bps_1m
                && prev.observed_at_ms == bundle.observed_at_ms
                && prev.signer == bundle.signer;
            if !identical {
                return Err(ExecError::ReplayNonce);
            }
        }
        let age = bundle.age_secs(now_ms);
        if age < -1.0 || age > m.formula.max_age_secs as f64 {
            return Err(ExecError::StaleObservation {
                age_secs: age as i64,
                max_age: m.formula.max_age_secs,
            });
        }
        if bundle.deviation_bps_1m > m.formula.max_deviation_bps {
            return Err(ExecError::CircuitBreaker);
        }
        let now_secs = now_ms / 1000;
        if now_secs >= m.formula.maturity_unix_secs {
            return Err(ExecError::Halted);
        }
        Ok(())
    }

    pub fn quote(&self, bundle: &OracleBundle, now_ms: i64) -> Result<Quote, ExecError> {
        self.quote_for_duration(bundle, 0, now_ms)
    }

    pub fn quote_for_duration(
        &self,
        bundle: &OracleBundle,
        duration_secs: i64,
        now_ms: i64,
    ) -> Result<Quote, ExecError> {
        self.validate_bundle(bundle, now_ms)?;
        let m = self
            .markets
            .get(&bundle.market.0)
            .ok_or(ExecError::MarketNotEnabled)?;
        let fair = m.formula.fair(bundle.spot);
        Ok(price_with_duration(
            &m.formula,
            bundle,
            m.liability(fair),
            m.formula.max_market_reserved_usdc_minor,
            now_ms,
            duration_secs,
        ))
    }

    /// Accept and store a signed oracle bundle (posted by the oracle service).
    /// It can then drive `buy_latest` / `sell_latest` until it becomes stale.
    pub fn submit_oracle(&mut self, bundle: OracleBundle, now_ms: i64) -> Result<(), ExecError> {
        self.validate_bundle(&bundle, now_ms)?;
        let fair = {
            let market = self.markets.get(&bundle.market.0).unwrap();
            market.formula.fair(bundle.spot)
        };
        let market = self.markets.get_mut(&bundle.market.0).unwrap();
        market.last_seq = bundle.seq;
        market.last_fair_usdc_minor = fair;
        self.last_bundle.insert(bundle.market.0.clone(), bundle);
        Ok(())
    }

    pub fn latest_bundle(&self, market: &MarketId) -> Option<&OracleBundle> {
        self.last_bundle.get(&market.0)
    }

    fn resolve_expiry_ms(
        &self,
        market: &SpotMarket,
        duration_secs: Option<i64>,
        now_ms: i64,
    ) -> Result<(i64, i64), ExecError> {
        let duration = match market.formula.kind {
            FormulaKind::BinaryV1 => {
                let remaining = market.formula.maturity_unix_secs - now_ms / 1000;
                if remaining <= 0 {
                    return Err(ExecError::Halted);
                }
                remaining
            }
            FormulaKind::SyntheticSpotV1 if duration_secs.is_none() => 0,
            _ => duration_secs.unwrap_or(7 * 86_400),
        };
        if duration < 0 {
            return Err(ExecError::InvalidDuration);
        }
        let expires_at_ms = if duration == 0 {
            i64::MAX
        } else if matches!(market.formula.kind, FormulaKind::BinaryV1) {
            // Binary lots expire exactly at the event maturity; adding the
            // whole-second remaining duration to a sub-second `now_ms` would
            // overshoot maturity and reject every trade.
            market
                .formula
                .maturity_unix_secs
                .checked_mul(1000)
                .ok_or(ExecError::AmountOverflow)?
        } else {
            now_ms
                .checked_add(
                    duration
                        .checked_mul(1000)
                        .ok_or(ExecError::AmountOverflow)?,
                )
                .ok_or(ExecError::AmountOverflow)?
        };
        if market.formula.maturity_unix_secs != i64::MAX
            && expires_at_ms > market.formula.maturity_unix_secs.saturating_mul(1000)
        {
            return Err(ExecError::InvalidDuration);
        }
        Ok((expires_at_ms, duration))
    }

    fn open_lot_max_payout(formula: &Formula, units: u64, principal_usdc_minor: u64) -> u64 {
        let by_units = ((units as u128 * formula.max_payout() as u128) / 1_000_000) as u64;
        match formula.kind {
            FormulaKind::BinaryV1 => by_units,
            // Fixed-duration linear products disclose a hard principal-multiple
            // cap; this makes guaranteed settlement possible without hedging.
            FormulaKind::IndexCappedV1 | FormulaKind::DirectV1 => {
                let by_principal =
                    ((principal_usdc_minor as u128 * formula.cap_mult_x1000 as u128) / 1000) as u64;
                by_units.min(by_principal)
            }
            FormulaKind::SyntheticSpotV1 => by_units,
        }
    }

    /// BUY with the product's default tenor (7 days for expiring products,
    /// event maturity for binaries, no expiry for SyntheticSpotV1).
    pub fn buy(
        &mut self,
        buyer: &AccountId,
        bundle: &OracleBundle,
        usdc_minor: u64,
        max_price: u64,
        now_ms: i64,
    ) -> Result<SpotFill, ExecError> {
        self.buy_with_duration(buyer, bundle, usdc_minor, max_price, None, now_ms)
    }

    /// Buy against the latest bundle previously posted with `submit_oracle`.
    pub fn buy_latest(
        &mut self,
        buyer: &AccountId,
        market: &MarketId,
        usdc_minor: u64,
        max_price: u64,
        duration_secs: Option<i64>,
        now_ms: i64,
    ) -> Result<SpotFill, ExecError> {
        let bundle =
            self.last_bundle
                .get(&market.0)
                .cloned()
                .ok_or(ExecError::StaleObservation {
                    age_secs: 0,
                    max_age: 0,
                })?;
        self.buy_with_duration(buyer, &bundle, usdc_minor, max_price, duration_secs, now_ms)
    }

    /// Open a fixed-duration position lot. Payment enters this vault, the
    /// tier allocation is checked against NAV, and the lot's explicit
    /// maximum payout must remain covered by post-trade collateral.
    pub fn buy_with_duration(
        &mut self,
        buyer: &AccountId,
        bundle: &OracleBundle,
        usdc_minor: u64,
        max_price: u64,
        duration_secs: Option<i64>,
        now_ms: i64,
    ) -> Result<SpotFill, ExecError> {
        let (formula, market_reserved_now, current_reference) = {
            let m = self
                .markets
                .get(&bundle.market.0)
                .ok_or(ExecError::MarketNotEnabled)?;
            let fair = m.formula.fair(bundle.spot);
            (m.formula.clone(), m.max_liability(), m.liability(fair))
        };
        let market_template = self.markets.get(&bundle.market.0).unwrap().clone();
        let (expires_at_ms, duration) =
            self.resolve_expiry_ms(&market_template, duration_secs, now_ms)?;
        let q = self.quote_for_duration(bundle, duration, now_ms)?;
        // Spot-jump breaker on new risk only: a >3x fair jump versus the last
        // accepted print is treated as a broken feed. Exits stay open.
        if market_template.last_fair_usdc_minor > 0 && market_template.units_out_minor > 0 {
            let last = market_template.last_fair_usdc_minor as u128;
            let fair = q.fair_usdc_minor as u128;
            if fair > last * (MAX_FAIR_JUMP_MULT as u128)
                || fair * (MAX_FAIR_JUMP_MULT as u128) < last
            {
                return Err(ExecError::CircuitBreaker);
            }
        }
        if usdc_minor < MIN_TRADE_USDC_MINOR {
            return Err(ExecError::TradeTooSmall {
                requested: usdc_minor,
                min: MIN_TRADE_USDC_MINOR,
            });
        }
        if usdc_minor > formula.max_trade_usdc_minor {
            return Err(ExecError::TradeTooLarge {
                requested: usdc_minor,
                max: formula.max_trade_usdc_minor,
            });
        }
        if q.ask_usdc_minor > max_price || q.ask_usdc_minor == 0 {
            return Err(ExecError::SlippageExceeded {
                quoted: q.ask_usdc_minor,
                max_acceptable: max_price,
            });
        }
        let units = ((usdc_minor as u128 * 1_000_000) / q.ask_usdc_minor as u128) as u64;
        if units == 0 {
            return Err(ExecError::TradeTooLarge {
                requested: usdc_minor,
                max: 0,
            });
        }
        let reserve_add = Self::open_lot_max_payout(&formula, units, usdc_minor);
        let market_reserved_after = market_reserved_now
            .checked_add(reserve_add)
            .ok_or(ExecError::AmountOverflow)?;
        if market_reserved_after > formula.max_market_reserved_usdc_minor {
            return Err(ExecError::ExposureExceeded {
                would_be: market_reserved_after,
                max: formula.max_market_reserved_usdc_minor,
            });
        }

        // Tier allocation uses current reference notional (MTM exposure), not
        // reserved collateral. A 0% effective allocation disables new buys.
        let nav_before = self.nav_with_market_price(&bundle.market, q.fair_usdc_minor);
        if nav_before <= 0 {
            return Err(ExecError::InsufficientCollateral {
                available: 0,
                required: usdc_minor,
            });
        }
        let allocation_bps = market_template.effective_allocation_bps() as u128;
        let allocation_cap = ((nav_before as u128 * allocation_bps) / 10_000) as u64;
        let new_reference = ((units as u128 * q.fair_usdc_minor as u128) / 1_000_000) as u64;
        let market_reference_after = current_reference + new_reference;
        if market_reference_after > allocation_cap {
            return Err(ExecError::ExposureExceeded {
                would_be: market_reference_after,
                max: allocation_cap,
            });
        }
        let gross_after = self.total_reference_notional()
            - market_template.liability(market_template.last_fair_usdc_minor)
            + market_reference_after;
        let gross_cap =
            ((nav_before as u128 * self.config.max_gross_reference_bps as u128) / 10_000) as u64;
        if gross_after > gross_cap {
            return Err(ExecError::ExposureExceeded {
                would_be: gross_after,
                max: gross_cap,
            });
        }

        // Solvency: post-trade cash must cover every open lot's explicit cap.
        let post_cash = self.collateral_usdc_minor as u128 + usdc_minor as u128;
        let required = self.total_reserved() as u128 + reserve_add as u128;
        if post_cash < required {
            return Err(ExecError::InsufficientCollateral {
                available: post_cash.min(u64::MAX as u128) as u64,
                required: required.min(u64::MAX as u128) as u64,
            });
        }
        let new_collateral = self
            .collateral_usdc_minor
            .checked_add(usdc_minor)
            .ok_or(ExecError::AmountOverflow)?;

        let position_id = self.next_position_id + 1;
        self.next_position_id = position_id;
        let lot = PositionLot {
            id: position_id,
            account: buyer.clone(),
            market: bundle.market.clone(),
            units_minor: units,
            principal_usdc_minor: usdc_minor,
            entry_spot: bundle.spot,
            entry_price_usdc_minor: q.ask_usdc_minor,
            opened_at_ms: now_ms,
            expires_at_ms,
            max_payout_usdc_minor: reserve_add,
            status: PositionStatus::Open,
        };
        self.position_lots.insert(position_id, lot);

        let held_before = self.position(buyer, &bundle.market);
        let new_held = held_before
            .checked_add(units)
            .ok_or(ExecError::AmountOverflow)?;
        let m = self.markets.get_mut(&bundle.market.0).unwrap();
        m.units_out_minor = m
            .units_out_minor
            .checked_add(units)
            .ok_or(ExecError::AmountOverflow)?;
        m.last_seq = bundle.seq;
        m.last_fair_usdc_minor = q.fair_usdc_minor;
        self.collateral_usdc_minor = new_collateral;
        self.positions
            .insert((buyer.0.clone(), bundle.market.0.clone()), new_held);
        self.last_bundle
            .insert(bundle.market.0.clone(), bundle.clone());
        Ok(SpotFill {
            is_buy: true,
            quoted_price_usdc_minor: q.ask_usdc_minor,
            usdc_minor,
            units_minor: units,
            position_id: Some(position_id),
            expires_at_ms: Some(expires_at_ms),
        })
    }

    /// Sell against the latest bundle previously posted with `submit_oracle`.
    pub fn sell_latest(
        &mut self,
        seller: &AccountId,
        market: &MarketId,
        units_minor: u64,
        min_price: u64,
        now_ms: i64,
    ) -> Result<SpotFill, ExecError> {
        let bundle =
            self.last_bundle
                .get(&market.0)
                .cloned()
                .ok_or(ExecError::StaleObservation {
                    age_secs: 0,
                    max_age: 0,
                })?;
        self.sell(seller, &bundle, units_minor, min_price, now_ms)
    }

    /// SELL unexpired position units back to the vault at the current bid.
    /// Lots are consumed FIFO by expiry. Expired lots must be settled, not sold.
    pub fn sell(
        &mut self,
        seller: &AccountId,
        bundle: &OracleBundle,
        units_minor: u64,
        min_price: u64,
        now_ms: i64,
    ) -> Result<SpotFill, ExecError> {
        let q = self.quote(bundle, now_ms)?;
        if q.bid_usdc_minor < min_price {
            return Err(ExecError::SlippageExceeded {
                quoted: q.bid_usdc_minor,
                max_acceptable: min_price,
            });
        }
        let mut lot_ids: Vec<u64> = self
            .position_lots
            .values()
            .filter(|lot| {
                lot.status == PositionStatus::Open
                    && lot.account == *seller
                    && lot.market == bundle.market
                    && lot.expires_at_ms > now_ms
            })
            .map(|lot| lot.id)
            .collect();
        lot_ids.sort_by_key(|id| (self.position_lots[id].expires_at_ms, *id));
        let eligible128: u128 = lot_ids
            .iter()
            .map(|id| self.position_lots[id].units_minor as u128)
            .sum();
        let eligible = eligible128.min(u64::MAX as u128) as u64;
        if units_minor > eligible {
            return Err(ExecError::InsufficientCollateral {
                available: eligible,
                required: units_minor,
            });
        }
        // Solvency cap (user-approved): each consumed lot pays at most its
        // pro-rata reserved maximum, exactly as at settlement. Fair may be
        // uncapped for display, but cash out can never exceed what was
        // reserved, so coverage >= 1 survives any spot move.
        let mut payout = 0u64;
        let mut left = units_minor;
        for lot_id in &lot_ids {
            if left == 0 {
                break;
            }
            let lot = &self.position_lots[lot_id];
            let take = left.min(lot.units_minor);
            let at_bid = ((take as u128 * q.bid_usdc_minor as u128) / 1_000_000) as u64;
            let cap_share = if lot.units_minor == 0 {
                0
            } else {
                ((lot.max_payout_usdc_minor as u128 * take as u128) / lot.units_minor as u128)
                    as u64
            };
            payout = payout
                .checked_add(at_bid.min(cap_share))
                .ok_or(ExecError::AmountOverflow)?;
            left -= take;
        }
        if payout > self.collateral_usdc_minor {
            return Err(ExecError::InsufficientCollateral {
                available: self.collateral_usdc_minor,
                required: payout,
            });
        }

        let mut remaining = units_minor;
        let mut first_lot = None;
        let mut single_lot = lot_ids.len() == 1;
        for lot_id in lot_ids {
            if remaining == 0 {
                break;
            }
            if first_lot.is_none() {
                first_lot = Some(lot_id);
            } else {
                single_lot = false;
            }
            let lot = self.position_lots.get_mut(&lot_id).unwrap();
            let take = remaining.min(lot.units_minor);
            let remaining_after = lot.units_minor - take;
            if remaining_after == 0 {
                lot.status = PositionStatus::Closed;
                lot.principal_usdc_minor = 0;
                lot.max_payout_usdc_minor = 0;
            } else {
                lot.principal_usdc_minor = ((lot.principal_usdc_minor as u128
                    * remaining_after as u128)
                    / lot.units_minor as u128) as u64;
                lot.max_payout_usdc_minor = ((lot.max_payout_usdc_minor as u128
                    * remaining_after as u128)
                    / lot.units_minor as u128) as u64;
            }
            lot.units_minor = remaining_after;
            remaining -= take;
        }

        let m = self.markets.get_mut(&bundle.market.0).unwrap();
        m.units_out_minor -= units_minor;
        m.last_seq = bundle.seq;
        m.last_fair_usdc_minor = q.fair_usdc_minor;
        self.collateral_usdc_minor -= payout;
        *self
            .positions
            .entry((seller.0.clone(), bundle.market.0.clone()))
            .or_insert(0) -= units_minor;
        self.last_bundle
            .insert(bundle.market.0.clone(), bundle.clone());
        Ok(SpotFill {
            is_buy: false,
            quoted_price_usdc_minor: q.bid_usdc_minor,
            usdc_minor: payout,
            units_minor,
            position_id: if single_lot { first_lot } else { None },
            expires_at_ms: None,
        })
    }

    fn validate_settlement_bundle(
        &self,
        bundle: &OracleBundle,
        expires_at_ms: i64,
        now_ms: i64,
    ) -> Result<(), ExecError> {
        let m = self
            .markets
            .get(&bundle.market.0)
            .ok_or(ExecError::UnknownMarket)?;
        if !bundle.signature_valid || bundle.signer != m.oracle_signer {
            return Err(ExecError::BadSignature);
        }
        if !bundle.spot.is_finite() || bundle.spot < 0.0 {
            return Err(ExecError::InvalidSpot);
        }
        // A binary fixing is a resolution, not a probability: exactly 0 or 1.
        if matches!(m.formula.kind, FormulaKind::BinaryV1)
            && bundle.spot != 0.0
            && bundle.spot != 1.0
        {
            return Err(ExecError::InvalidSpot);
        }
        if bundle.seq < m.last_seq {
            return Err(ExecError::ReplayNonce);
        }
        if bundle.observed_at_ms < expires_at_ms {
            return Err(ExecError::PositionNotExpired);
        }
        // Expiry-pinned window: prints long after expiry are lookahead.
        if bundle.observed_at_ms > expires_at_ms.saturating_add(SETTLEMENT_WINDOW_MS) {
            return Err(ExecError::StaleObservation {
                age_secs: (bundle.observed_at_ms - expires_at_ms) / 1000,
                max_age: SETTLEMENT_WINDOW_MS / 1000,
            });
        }
        if bundle.observed_at_ms > now_ms + 1000 {
            return Err(ExecError::StaleObservation {
                age_secs: -1,
                max_age: m.formula.max_age_secs,
            });
        }
        Ok(())
    }

    /// Settle one expired lot from an authenticated fixing bundle. The
    /// bundle may be submitted after expiry; trading max-age does not apply.
    pub fn settle_position(
        &mut self,
        holder: &AccountId,
        position_id: u64,
        bundle: &OracleBundle,
        now_ms: i64,
    ) -> Result<u64, ExecError> {
        let lot = self
            .position_lots
            .get(&position_id)
            .cloned()
            .ok_or(ExecError::PositionNotFound)?;
        if lot.account != *holder {
            return Err(ExecError::Unauthorized);
        }
        if lot.market != bundle.market {
            return Err(ExecError::UnknownMarket);
        }
        if lot.status != PositionStatus::Open {
            return Err(ExecError::PositionNotFound);
        }
        if now_ms < lot.expires_at_ms {
            return Err(ExecError::PositionNotExpired);
        }
        self.validate_settlement_bundle(bundle, lot.expires_at_ms, now_ms)?;
        let (pay_per, last_fair) = {
            let market = self.markets.get(&lot.market.0).unwrap();
            (
                market.formula.payoff(bundle.spot),
                market.formula.payoff(bundle.spot),
            )
        };
        let uncapped = ((lot.units_minor as u128 * pay_per as u128) / 1_000_000) as u64;
        let payout = uncapped.min(lot.max_payout_usdc_minor);
        if payout > self.collateral_usdc_minor {
            return Err(ExecError::InsufficientCollateral {
                available: self.collateral_usdc_minor,
                required: payout,
            });
        }
        let units = lot.units_minor;
        if let Some(stored) = self.position_lots.get_mut(&position_id) {
            stored.status = PositionStatus::Settled;
        }
        let market = self.markets.get_mut(&lot.market.0).unwrap();
        market.units_out_minor -= units;
        market.last_seq = bundle.seq;
        market.last_fair_usdc_minor = last_fair;
        self.collateral_usdc_minor -= payout;
        *self
            .positions
            .entry((holder.0.clone(), lot.market.0.clone()))
            .or_insert(0) -= units;
        self.last_bundle
            .insert(lot.market.0.clone(), bundle.clone());
        Ok(payout)
    }

    /// Settle every expired lot held by an account in one market.
    pub fn settle_expired(
        &mut self,
        holder: &AccountId,
        bundle: &OracleBundle,
        now_ms: i64,
    ) -> Result<u64, ExecError> {
        let ids: Vec<u64> = self
            .position_lots
            .values()
            .filter(|lot| {
                lot.status == PositionStatus::Open
                    && lot.account == *holder
                    && lot.market == bundle.market
                    && lot.expires_at_ms <= now_ms
            })
            .map(|lot| lot.id)
            .collect();
        if ids.is_empty() {
            if self.position(holder, &bundle.market) > 0 {
                return Err(ExecError::PositionNotExpired);
            }
            return Ok(0);
        }
        let mut total = 0u64;
        for id in ids {
            total += self.settle_position(holder, id, bundle, now_ms)?;
        }
        Ok(total)
    }

    pub fn halt(&mut self, market: &MarketId) {
        if let Some(m) = self.markets.get_mut(&market.0) {
            m.halted = true;
        }
    }

    pub fn unhalt(&mut self, market: &MarketId) {
        if let Some(m) = self.markets.get_mut(&market.0) {
            m.halted = false;
        }
    }

    /// Credit an amount already deducted from collateral as withdrawable.
    pub fn credit_claimable(
        &mut self,
        who: &AccountId,
        amount_usdc_minor: u64,
    ) -> Result<(), ExecError> {
        if amount_usdc_minor == 0 {
            return Ok(());
        }
        let cur = self.claimable(who);
        let next = cur
            .checked_add(amount_usdc_minor)
            .ok_or(ExecError::AmountOverflow)?;
        self.claimable_usdc_minor.insert(who.0.clone(), next);
        Ok(())
    }

    /// Take the full claimable balance (zeroes it). Collateral was already
    /// deducted when the payout was credited; the caller (contract) is
    /// responsible for the token transfer and must restore on failure via
    /// `credit_claimable`.
    pub fn claim(&mut self, who: &AccountId) -> u64 {
        self.claimable_usdc_minor.remove(&who.0).unwrap_or(0)
    }

    /// Pre-register redemption for a market that will close. The request is
    /// recorded while the holder still has an open position and is settled
    /// automatically in the market close-out. Idempotent.
    pub fn request_redemption(
        &mut self,
        holder: &AccountId,
        market: &MarketId,
        now_ms: i64,
    ) -> Result<(), ExecError> {
        let m = self
            .markets
            .get(&market.0)
            .ok_or(ExecError::UnknownMarket)?;
        // Synthetic spot never closes; its exit is sell-at-bid, not redemption.
        if m.formula.maturity_unix_secs == i64::MAX {
            return Err(ExecError::PositionNotExpired);
        }
        if self.position(holder, market) == 0 {
            return Err(ExecError::PositionNotFound);
        }
        self.redemption_requests
            .insert((holder.0.clone(), market.0.clone()), now_ms);
        Ok(())
    }

    pub fn redemption_requested(&self, holder: &AccountId, market: &MarketId) -> bool {
        self.redemption_requests
            .contains_key(&(holder.0.clone(), market.0.clone()))
    }

    pub fn claimable(&self, holder: &AccountId) -> u64 {
        *self.claimable_usdc_minor.get(&holder.0).unwrap_or(&0)
    }

    /// Market close-out: once the market reaches maturity, anyone can submit
    /// the signed fixing bundle and settle the whole market in one call —
    /// every open lot (pre-requested redemptions included) is closed at the
    /// fixing payoff, payouts are credited per holder as claimable, pending
    /// requests are cleared and the market halts. Returns (total payout,
    /// lots settled). Lots held with a later individual tenor are left open.
    pub fn settle_market(
        &mut self,
        bundle: &OracleBundle,
        now_ms: i64,
    ) -> Result<(u64, u32), ExecError> {
        let (total, lots, _more) = self.settle_market_paged(bundle, now_ms, u32::MAX)?;
        // Full close-out path: drain any remainder pages defensively.
        let mut total = total;
        let mut lots = lots;
        let mut more = _more;
        while more {
            let (t, n, m2) = self.settle_market_paged(bundle, now_ms, u32::MAX)?;
            total = total.checked_add(t).ok_or(ExecError::AmountOverflow)?;
            lots = lots.checked_add(n).ok_or(ExecError::AmountOverflow)?;
            more = m2;
            if n == 0 {
                break;
            }
        }
        Ok((total, lots))
    }

    /// Paged market close-out: settles at most `max_lots` lots so one call
    /// stays within gas limits. Returns (total payout, lots settled, more
    /// remain). The market is halted and redemption requests cleared only
    /// on the final page.
    pub fn settle_market_paged(
        &mut self,
        bundle: &OracleBundle,
        now_ms: i64,
        max_lots: u32,
    ) -> Result<(u64, u32, bool), ExecError> {
        let maturity_ms = {
            let m = self
                .markets
                .get(&bundle.market.0)
                .ok_or(ExecError::UnknownMarket)?;
            if m.formula.maturity_unix_secs == i64::MAX {
                return Err(ExecError::PositionNotExpired);
            }
            m.formula
                .maturity_unix_secs
                .checked_mul(1000)
                .ok_or(ExecError::AmountOverflow)?
        };
        if now_ms < maturity_ms {
            return Err(ExecError::PositionNotExpired);
        }
        // Validate the fixing once up front so a bad bundle closes nothing.
        self.validate_settlement_bundle(bundle, maturity_ms, now_ms)?;
        let mut lots: Vec<(u64, AccountId)> = self
            .position_lots
            .values()
            .filter(|lot| {
                lot.status == PositionStatus::Open
                    && lot.market == bundle.market
                    && lot.expires_at_ms <= now_ms
            })
            .map(|lot| (lot.id, lot.account.clone()))
            .collect();
        lots.sort_by_key(|(id, _)| *id);
        let more = (lots.len() as u32) > max_lots;
        lots.truncate(max_lots as usize);
        let mut total = 0u64;
        for (id, holder) in &lots {
            let payout = self.settle_position(holder, *id, bundle, now_ms)?;
            self.credit_claimable(holder, payout)?;
            total = total.checked_add(payout).ok_or(ExecError::AmountOverflow)?;
        }
        if !more {
            self.redemption_requests
                .retain(|(_, market), _| *market != bundle.market.0);
            if let Some(m) = self.markets.get_mut(&bundle.market.0) {
                m.halted = true;
            }
        }
        Ok((total, lots.len() as u32, more))
    }
}

/// A fill executed by one vault as part of a routed order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutedFill {
    pub vault_id: VaultId,
    pub fill: SpotFill,
}

/// Result of routing an order across competing vaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutedExecution {
    pub fills: Vec<RoutedFill>,
    pub unfilled_usdc_minor: u64,
}

/// Protocol-level registry plus competing vaults. The market registry is the
/// only place a market is created; creating it does not draft any vault's
/// capital. Each vault independently decides whether to enable it.
#[derive(BorshSerialize, BorshDeserialize)]
pub struct ZeroTickProtocol {
    pub owner: AccountId,
    pub oracles: OracleRegistry,
    pub markets: HashMap<String, MarketDefinition>,
    pub vaults: HashMap<String, SharedVault>,
    pub protocol_fees_usdc_minor: u64,
}

impl ZeroTickProtocol {
    pub fn new(owner: AccountId) -> Self {
        Self {
            owner,
            oracles: OracleRegistry::new(),
            markets: HashMap::new(),
            vaults: HashMap::new(),
            protocol_fees_usdc_minor: 0,
        }
    }

    fn require_owner(&self, caller: &AccountId) -> Result<(), ExecError> {
        if *caller == self.owner {
            Ok(())
        } else {
            Err(ExecError::Unauthorized)
        }
    }

    pub fn register_oracle(
        &mut self,
        caller: &AccountId,
        adapter: OracleAdapter,
    ) -> Result<(), ExecError> {
        self.require_owner(caller)?;
        self.oracles.register(adapter);
        Ok(())
    }

    /// Register a new market for a flat $10 fee. New markets start Unrated,
    /// so vaults allocate 0% unless an operator explicitly overrides it or
    /// protocol governance assigns a tier.
    pub fn create_market(
        &mut self,
        mut definition: MarketDefinition,
        fee_paid_usdc_minor: u64,
    ) -> Result<(), ExecError> {
        if self.markets.contains_key(&definition.id.0) {
            return Err(ExecError::AlreadyExists);
        }
        definition.formula_template.validate()?;
        if !self
            .oracles
            .can_create_market(&definition.oracle_id, &definition.id)
        {
            return Err(ExecError::UnknownMarket);
        }
        if fee_paid_usdc_minor < MARKET_CREATION_FEE_USDC_MINOR {
            return Err(ExecError::InsufficientCollateral {
                available: fee_paid_usdc_minor,
                required: MARKET_CREATION_FEE_USDC_MINOR,
            });
        }
        definition.risk_tier = MarketRiskTier::Unrated;
        definition.halted = false;
        self.protocol_fees_usdc_minor = self
            .protocol_fees_usdc_minor
            .checked_add(fee_paid_usdc_minor)
            .ok_or(ExecError::AmountOverflow)?;
        self.markets.insert(definition.id.0.clone(), definition);
        Ok(())
    }

    pub fn assign_market_tier(
        &mut self,
        caller: &AccountId,
        market: &MarketId,
        tier: MarketRiskTier,
    ) -> Result<(), ExecError> {
        self.require_owner(caller)?;
        let definition = self
            .markets
            .get_mut(&market.0)
            .ok_or(ExecError::UnknownMarket)?;
        definition.risk_tier = tier;
        for vault in self.vaults.values_mut() {
            vault.sync_market_tier(market, tier);
        }
        Ok(())
    }

    pub fn create_vault(
        &mut self,
        creator: &AccountId,
        vault_id: VaultId,
        mut config: VaultConfig,
        now_ms: i64,
    ) -> Result<(), ExecError> {
        if self.vaults.contains_key(&vault_id.0) {
            return Err(ExecError::AlreadyExists);
        }
        config.operator = Some(creator.clone());
        let mut vault = SharedVault::with_config(vault_id.0.clone(), config);
        if vault.config.withdrawal_epoch_secs > 0 {
            vault.next_withdrawal_at_ms = now_ms + vault.config.withdrawal_epoch_secs * 1000;
        }
        self.vaults.insert(vault_id.0, vault);
        Ok(())
    }

    pub fn vault(&self, vault_id: &VaultId) -> Option<&SharedVault> {
        self.vaults.get(&vault_id.0)
    }

    pub fn vault_mut(&mut self, vault_id: &VaultId) -> Option<&mut SharedVault> {
        self.vaults.get_mut(&vault_id.0)
    }

    pub fn enable_market(
        &mut self,
        caller: &AccountId,
        vault_id: &VaultId,
        market: &MarketId,
        pricing_overrides: Option<&PricingOverrides>,
        allocation_override_bps: Option<u16>,
    ) -> Result<(), ExecError> {
        let definition = self
            .markets
            .get(&market.0)
            .cloned()
            .ok_or(ExecError::UnknownMarket)?;
        if definition.halted {
            return Err(ExecError::Halted);
        }
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        Self::require_operator_static(caller, vault)?;
        vault.enable_market(&definition, pricing_overrides, allocation_override_bps)
    }

    fn require_operator_static(caller: &AccountId, vault: &SharedVault) -> Result<(), ExecError> {
        match &vault.config.operator {
            Some(operator) if operator != caller => Err(ExecError::Unauthorized),
            _ => Ok(()),
        }
    }

    pub fn set_market_allocation(
        &mut self,
        caller: &AccountId,
        vault_id: &VaultId,
        market: &MarketId,
        allocation_override_bps: Option<u16>,
    ) -> Result<(), ExecError> {
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        Self::require_operator_static(caller, vault)?;
        vault.set_market_allocation(market, allocation_override_bps)
    }

    pub fn deposit_lp(
        &mut self,
        vault_id: &VaultId,
        who: &AccountId,
        usdc_minor: u64,
    ) -> Result<u64, ExecError> {
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        vault.deposit_lp(who, usdc_minor)
    }

    pub fn withdraw_lp(
        &mut self,
        vault_id: &VaultId,
        who: &AccountId,
        shares: u64,
        now_ms: i64,
    ) -> Result<u64, ExecError> {
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        vault.withdraw_lp(who, shares, now_ms)
    }

    /// Quotes from every vault that has enabled this market. Rejected
    /// quotes (stale bundle, circuit breaker, disabled allocation still has
    /// a theoretical quote but cannot execute) are omitted if validation fails.
    pub fn quote_all(
        &self,
        bundle: &OracleBundle,
        duration_secs: Option<i64>,
        now_ms: i64,
    ) -> Vec<(VaultId, Quote)> {
        let duration = duration_secs.unwrap_or(7 * 86_400);
        let mut out: Vec<(VaultId, Quote)> = self
            .vaults
            .iter()
            .filter(|(_, vault)| vault.markets.contains_key(&bundle.market.0))
            .filter_map(|(id, vault)| {
                vault
                    .quote_for_duration(bundle, duration, now_ms)
                    .ok()
                    .map(|q| (VaultId::new(id.clone()), q))
            })
            .collect();
        out.sort_by_key(|(_, q)| q.ask_usdc_minor);
        out
    }

    /// Execute a buy in one named vault.
    #[allow(clippy::too_many_arguments)] // explicit trade parameters beat a params struct at this API boundary
    pub fn buy_in_vault(
        &mut self,
        vault_id: &VaultId,
        buyer: &AccountId,
        bundle: &OracleBundle,
        usdc_minor: u64,
        max_price: u64,
        duration_secs: Option<i64>,
        now_ms: i64,
    ) -> Result<SpotFill, ExecError> {
        if self
            .markets
            .get(&bundle.market.0)
            .map(|m| m.halted)
            .unwrap_or(true)
        {
            return Err(ExecError::Halted);
        }
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        if !vault.markets.contains_key(&bundle.market.0) {
            return Err(ExecError::MarketNotEnabled);
        }
        vault.buy_with_duration(buyer, bundle, usdc_minor, max_price, duration_secs, now_ms)
    }

    /// Route a buy from cheapest ask upward, splitting across vaults when a
    /// single vault cannot fill the entire amount under its limits.
    pub fn buy_routed(
        &mut self,
        buyer: &AccountId,
        bundle: &OracleBundle,
        mut remaining_usdc_minor: u64,
        max_price: u64,
        duration_secs: Option<i64>,
        now_ms: i64,
    ) -> RoutedExecution {
        let ranked: Vec<VaultId> = self
            .quote_all(bundle, duration_secs, now_ms)
            .into_iter()
            .filter(|(_, q)| q.ask_usdc_minor <= max_price)
            .map(|(id, _)| id)
            .collect();
        let mut fills = Vec::new();
        for vault_id in ranked {
            while remaining_usdc_minor > 0 {
                let max_trade = self
                    .vaults
                    .get(&vault_id.0)
                    .and_then(|vault| vault.markets.get(&bundle.market.0))
                    .map(|market| market.formula.max_trade_usdc_minor)
                    .unwrap_or(0);
                if max_trade == 0 {
                    break;
                }
                let mut attempt = remaining_usdc_minor.min(max_trade);
                let mut filled = false;
                while attempt > 0 {
                    match self.buy_in_vault(
                        &vault_id,
                        buyer,
                        bundle,
                        attempt,
                        max_price,
                        duration_secs,
                        now_ms,
                    ) {
                        Ok(fill) => {
                            remaining_usdc_minor -= fill.usdc_minor;
                            fills.push(RoutedFill {
                                vault_id: vault_id.clone(),
                                fill,
                            });
                            filled = true;
                            break;
                        }
                        Err(_) if attempt > 1_000_000 => attempt /= 2,
                        Err(_) => break,
                    }
                }
                if !filled {
                    break;
                }
                if attempt < max_trade {
                    break;
                }
            }
            if remaining_usdc_minor == 0 {
                break;
            }
        }
        RoutedExecution {
            fills,
            unfilled_usdc_minor: remaining_usdc_minor,
        }
    }

    /// Sell held units to the highest bidding vault that holds the seller's
    /// lots. A lot can only close against its issuing vault.
    /// Broadcast a verified oracle bundle to every vault that enabled its
    /// market. Returns the number of vaults accepting it.
    pub fn submit_oracle(&mut self, bundle: OracleBundle, now_ms: i64) -> usize {
        let mut accepted = 0;
        for vault in self.vaults.values_mut() {
            if vault.markets.contains_key(&bundle.market.0)
                && vault.submit_oracle(bundle.clone(), now_ms).is_ok()
            {
                accepted += 1;
            }
        }
        accepted
    }

    pub fn settle_position(
        &mut self,
        vault_id: &VaultId,
        holder: &AccountId,
        position_id: u64,
        bundle: &OracleBundle,
        now_ms: i64,
    ) -> Result<u64, ExecError> {
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        vault.settle_position(holder, position_id, bundle, now_ms)
    }

    pub fn settle_expired(
        &mut self,
        vault_id: &VaultId,
        holder: &AccountId,
        bundle: &OracleBundle,
        now_ms: i64,
    ) -> Result<u64, ExecError> {
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        vault.settle_expired(holder, bundle, now_ms)
    }

    pub fn request_redemption(
        &mut self,
        vault_id: &VaultId,
        holder: &AccountId,
        market: &MarketId,
        now_ms: i64,
    ) -> Result<(), ExecError> {
        let vault = self
            .vaults
            .get_mut(&vault_id.0)
            .ok_or(ExecError::UnknownVault)?;
        vault.request_redemption(holder, market, now_ms)
    }

    /// Close out a matured market in every vault that enabled it: each vault
    /// settles all of its open lots against the same fixing bundle. Returns
    /// (total payout, lots settled) summed across vaults.
    pub fn settle_market(
        &mut self,
        bundle: &OracleBundle,
        now_ms: i64,
    ) -> Result<(u64, u32), ExecError> {
        let mut total = 0u64;
        let mut lots = 0u32;
        let mut any = false;
        for vault in self.vaults.values_mut() {
            if !vault.markets.contains_key(&bundle.market.0) {
                continue;
            }
            any = true;
            let (paid, settled) = vault.settle_market(bundle, now_ms)?;
            total = total.checked_add(paid).ok_or(ExecError::AmountOverflow)?;
            lots = lots.checked_add(settled).ok_or(ExecError::AmountOverflow)?;
        }
        if !any {
            return Err(ExecError::UnknownMarket);
        }
        Ok((total, lots))
    }

    pub fn sell_routed(
        &mut self,
        seller: &AccountId,
        bundle: &OracleBundle,
        mut remaining_units_minor: u64,
        min_price: u64,
        now_ms: i64,
    ) -> RoutedExecution {
        let mut candidates: Vec<(VaultId, Quote)> = self
            .vaults
            .iter()
            .filter(|(_, vault)| vault.position(seller, &bundle.market) > 0)
            .filter_map(|(id, vault)| {
                vault
                    .quote(bundle, now_ms)
                    .ok()
                    .map(|q| (VaultId::new(id.clone()), q))
            })
            .filter(|(_, q)| q.bid_usdc_minor >= min_price)
            .collect();
        candidates.sort_by_key(|c| std::cmp::Reverse(c.1.bid_usdc_minor));
        let mut fills = Vec::new();
        for (vault_id, _) in candidates {
            if remaining_units_minor == 0 {
                break;
            }
            let held = self
                .vaults
                .get(&vault_id.0)
                .map(|vault| vault.position(seller, &bundle.market))
                .unwrap_or(0);
            let attempt = remaining_units_minor.min(held);
            if attempt == 0 {
                continue;
            }
            let vault = self.vaults.get_mut(&vault_id.0).unwrap();
            if let Ok(fill) = vault.sell(seller, bundle, attempt, min_price, now_ms) {
                remaining_units_minor -= fill.units_minor;
                fills.push(RoutedFill { vault_id, fill });
            }
        }
        RoutedExecution {
            fills,
            unfilled_usdc_minor: remaining_units_minor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: i64 = 1_700_000_000_000;

    fn vault() -> (SharedVault, MarketId) {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 100_000_000_000)
            .unwrap(); // $100k shared
        let id = MarketId::new("ARG30-DEC26");
        v.create_market(
            id.clone(),
            "Merval 30 (synthetic)".into(),
            "oracle.near".into(),
            Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap(); // operator enables risk (creation defaults to 0%)
        (v, id)
    }

    fn bundle(id: &MarketId, spot: f64, age_ms: i64, seq: u64) -> OracleBundle {
        OracleBundle {
            market: id.clone(),
            spot,
            deviation_bps_1m: 40,
            observed_at_unix_secs: (NOW_MS - age_ms) / 1000,
            observed_at_ms: NOW_MS - age_ms,
            seq,
            signer: "oracle.near".into(),
            signature_valid: true,
        }
    }

    #[test]
    fn spot_payoff_matches_capped_note() {
        let f = Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000);
        assert_eq!(f.payoff(700.0), 7_000_000);
        assert_eq!(f.payoff(1000.0), 10_000_000);
        assert_eq!(f.payoff(1150.0), 11_500_000);
        assert_eq!(f.payoff(2000.0), 20_000_000);
        assert_eq!(f.payoff(2500.0), 20_000_000);
        assert_eq!(f.max_payout(), 20_000_000);
    }

    #[test]
    fn older_oracle_means_worse_price() {
        let (v, id) = vault();
        let fresh = v.quote(&bundle(&id, 1000.0, 200, 1), NOW_MS).unwrap();
        let one_sec = v.quote(&bundle(&id, 1000.0, 1000, 1), NOW_MS).unwrap();
        let four_sec = v.quote(&bundle(&id, 1000.0, 4000, 1), NOW_MS).unwrap();
        // Same fair, but staleness widens the spread monotonically.
        assert_eq!(fresh.fair_usdc_minor, four_sec.fair_usdc_minor);
        assert!(fresh.ask_usdc_minor < one_sec.ask_usdc_minor);
        assert!(one_sec.ask_usdc_minor < four_sec.ask_usdc_minor);
        assert!(fresh.bid_usdc_minor > four_sec.bid_usdc_minor);
        assert!(four_sec.half_spread_bps > fresh.half_spread_bps + 90.0);
    }

    #[test]
    fn high_deviation_widens_then_breaks() {
        let (v, id) = vault();
        let mut calm = bundle(&id, 1000.0, 200, 1);
        calm.deviation_bps_1m = 10;
        let mut wild = bundle(&id, 1000.0, 200, 1);
        wild.deviation_bps_1m = 300;
        let mut broken = bundle(&id, 1000.0, 200, 1);
        broken.deviation_bps_1m = 900;
        assert!(
            v.quote(&wild, NOW_MS).unwrap().half_spread_bps
                > v.quote(&calm, NOW_MS).unwrap().half_spread_bps
        );
        assert_eq!(v.quote(&broken, NOW_MS), Err(ExecError::CircuitBreaker));
    }

    #[test]
    fn validity_gates_signature_signer_seq_stale() {
        let (mut v, id) = vault();
        let alice = AccountId::new("alice");
        let mut bad_sig = bundle(&id, 1000.0, 100, 1);
        bad_sig.signature_valid = false;
        assert_eq!(
            v.buy(&alice, &bad_sig, 1_000_000, u64::MAX, NOW_MS),
            Err(ExecError::BadSignature)
        );
        let mut wrong = bundle(&id, 1000.0, 100, 1);
        wrong.signer = "eve.near".into();
        assert_eq!(
            v.buy(&alice, &wrong, 1_000_000, u64::MAX, NOW_MS),
            Err(ExecError::BadSignature)
        );
        let stale = bundle(&id, 1000.0, 20_000, 1);
        assert!(matches!(
            v.buy(&alice, &stale, 1_000_000, u64::MAX, NOW_MS),
            Err(ExecError::StaleObservation { .. })
        ));
        let ok = bundle(&id, 1000.0, 100, 5);
        v.buy(&alice, &ok, 1_000_000, u64::MAX, NOW_MS).unwrap();
        // A fresh public oracle bundle may be reused until superseded.
        v.buy(&alice, &ok, 1_000_000, u64::MAX, NOW_MS).unwrap();
        let old = bundle(&id, 1000.0, 100, 4);
        assert_eq!(
            v.buy(&alice, &old, 1_000_000, u64::MAX, NOW_MS),
            Err(ExecError::ReplayNonce)
        );
    }

    #[test]
    fn buy_mints_spot_position_and_shared_vault_stays_solvent() {
        let (mut v, id) = vault();
        let alice = AccountId::new("alice");
        let fill = v
            .buy(
                &alice,
                &bundle(&id, 1000.0, 200, 1),
                1_000_000_000,
                u64::MAX,
                NOW_MS,
            )
            .unwrap(); // $1,000
        assert!(fill.is_buy);
        assert!(fill.units_minor > 90_000_000 && fill.units_minor < 100_000_000); // ~99 units at ~$10.10
        assert_eq!(v.position(&alice, &id), fill.units_minor);
        assert!(v.coverage() >= 1.0);
        assert_eq!(v.collateral_usdc_minor, 101_010_000_000); // 00k LP + 0 creation fee + k buy
        // Sell half back: units burned, USDC returned at bid.
        let half = fill.units_minor / 2;
        let sell = v
            .sell(&alice, &bundle(&id, 1000.0, 200, 2), half, 0, NOW_MS)
            .unwrap();
        assert!(!sell.is_buy && sell.usdc_minor > 0);
        assert_eq!(v.position(&alice, &id), fill.units_minor - half);
    }

    #[test]
    fn cannot_sell_what_you_do_not_hold() {
        let (mut v, id) = vault();
        let err = v.sell(
            &AccountId::new("nobody"),
            &bundle(&id, 1000.0, 100, 1),
            1_000_000,
            0,
            NOW_MS,
        );
        assert!(matches!(err, Err(ExecError::InsufficientCollateral { .. })));
    }

    #[test]
    fn shared_vault_per_market_cap_blocks_drain() {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 60_000_000_000).unwrap();
        let a = MarketId::new("ARG30-DEC26");
        let b = MarketId::new("BOV-DEC26");
        let mut f = Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000);
        f.max_market_reserved_usdc_minor = 2_000_000_000; // $2k reserved cap each
        v.create_market(
            a.clone(),
            "A".into(),
            "oracle.near".into(),
            f.clone(),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.create_market(
            b.clone(),
            "B".into(),
            "oracle.near".into(),
            f,
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&a, Some(10_000)).unwrap();
        v.set_market_allocation(&b, Some(10_000)).unwrap();
        let alice = AccountId::new("alice");
        // Big buy on A would reserve ~2x notional → blocked by per-market cap first.
        let err = v.buy(
            &alice,
            &bundle(&a, 1000.0, 100, 1),
            5_000_000_000,
            u64::MAX,
            NOW_MS,
        );
        assert!(matches!(err, Err(ExecError::ExposureExceeded { .. })));
        // Same vault, market B still tradeable.
        let bb = OracleBundle {
            market: b.clone(),
            ..bundle(&b, 1000.0, 100, 1)
        };
        // B inception also 1000 in this fixture, so spot 1000 is par.
        v.buy(&alice, &bb, 100_000_000, u64::MAX, NOW_MS).unwrap();
    }

    #[test]
    fn redeem_at_maturity_pays_capped_payoff() {
        let (mut v, id) = vault();
        let alice = AccountId::new("alice");
        let fill = v
            .buy(
                &alice,
                &bundle(&id, 1000.0, 100, 1),
                100_000_000,
                u64::MAX,
                NOW_MS,
            )
            .unwrap();
        let expiry = fill.expires_at_ms.unwrap();
        let mut fixing = bundle(&id, 1500.0, 0, 2);
        fixing.observed_at_ms = expiry;
        fixing.observed_at_unix_secs = expiry / 1000;
        assert!(v.settle_expired(&alice, &fixing, NOW_MS).is_err()); // too early
        let payout = v.settle_expired(&alice, &fixing, expiry).unwrap();
        assert!(payout > 100_000_000); // bought ~$10, index +50% → ~$15/position
        assert_eq!(v.position(&alice, &id), 0);
    }

    #[test]
    fn binary_odds_pricing_and_resolution() {
        // $10 payout event, oracle probability 38% -> fair $3.80, odds ~2.63.
        let f = Formula::binary_v1(10_000_000, 1_800_000_000);
        assert_eq!(f.fair(0.38), 3_800_000);
        assert!((f.implied_odds(f.fair(0.38)) - 2.631).abs() < 0.01);
        assert_eq!(f.payoff(1.0), 10_000_000); // YES
        assert_eq!(f.payoff(0.0), 0); // NO
        assert_eq!(f.max_payout(), 10_000_000);

        // Same shared vault, same buy/sell path as index synthetics.
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 50_000_000_000).unwrap();
        let id = MarketId::new("FED-CUT-DEC26");
        v.create_market(
            id.clone(),
            "Fed cuts in Dec 2026?".into(),
            "oracle.near".into(),
            f,
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        let alice = AccountId::new("alice");
        let mut b = bundle(&id, 0.38, 200, 1);
        // bundle() uses arg30 signer convention from the shared fixture
        b.signer = "oracle.near".into();
        let fill = v.buy(&alice, &b, 380_000_000, u64::MAX, NOW_MS).unwrap(); // $380
        assert!(fill.units_minor > 90_000_000); // ~97 units at ~$3.90 ask
        assert!(v.coverage() >= 1.0);
        // Resolve YES after maturity: each position redeems the full $10.
        let mut fixing = bundle(&id, 1.0, 0, 2);
        fixing.observed_at_ms = 1_800_000_000_000;
        fixing.observed_at_unix_secs = 1_800_000_000;
        let payout = v
            .settle_expired(&alice, &fixing, 1_800_000_001_000)
            .unwrap();
        assert!(payout > 900_000_000); // >$900 back on $380 at ~2.6x odds
        assert_eq!(v.position(&alice, &id), 0);
    }

    #[test]
    fn binary_buy_at_subsecond_now_expires_exactly_at_maturity() {
        // Regression: expiry was now_ms + whole-second remaining, overshooting
        // maturity by the sub-second part and rejecting every binary buy.
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 50_000_000_000).unwrap();
        let id = MarketId::new("FED-CUT-SUBSEC");
        let f = Formula::binary_v1(10_000_000, NOW_MS / 1000 + 90);
        v.create_market(
            id.clone(),
            "Fed cuts?".into(),
            "oracle.near".into(),
            f,
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        let alice = AccountId::new("alice");
        let mut b = bundle(&id, 0.62, 40, 1);
        b.signer = "oracle.near".into();
        let fill = v
            .buy(&alice, &b, 380_000_000, u64::MAX, NOW_MS + 500)
            .unwrap();
        assert_eq!(fill.expires_at_ms, Some((NOW_MS / 1000 + 90) * 1000));
    }

    fn binary_vault() -> (SharedVault, MarketId, i64) {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 50_000_000_000).unwrap();
        let id = MarketId::new("FED-CUT-ENDING");
        let maturity = NOW_MS / 1000 + 90;
        v.create_market(
            id.clone(),
            "Fed cuts?".into(),
            "oracle.near".into(),
            Formula::binary_v1(10_000_000, maturity),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        (v, id, maturity * 1000)
    }

    fn fixing(id: &MarketId, spot: f64, observed_at_ms: i64, seq: u64) -> OracleBundle {
        OracleBundle {
            market: id.clone(),
            spot,
            deviation_bps_1m: 0,
            observed_at_unix_secs: observed_at_ms / 1000,
            observed_at_ms,
            seq,
            signer: "oracle.near".into(),
            signature_valid: true,
        }
    }

    #[test]
    fn trading_halts_at_maturity() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        let at_maturity = fixing(&id, 0.62, expiry_ms - 200, 1);
        assert!(matches!(
            v.buy(&alice, &at_maturity, 380_000_000, u64::MAX, expiry_ms),
            Err(ExecError::Halted)
        ));
        let after = fixing(&id, 0.62, expiry_ms + 800, 2);
        assert!(matches!(
            v.buy(&alice, &after, 380_000_000, u64::MAX, expiry_ms + 1000),
            Err(ExecError::Halted)
        ));
    }

    #[test]
    fn binary_resolving_no_pays_zero_and_clears_position() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        assert!(v.position(&alice, &id) > 0);
        let payout = v
            .settle_expired(&alice, &fixing(&id, 0.0, expiry_ms, 2), expiry_ms)
            .unwrap();
        assert_eq!(payout, 0); // NO: premium stays with the vault
        assert_eq!(v.position(&alice, &id), 0);
    }

    #[test]
    fn settlement_rejects_bad_signature_and_early_fixing() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        let mut forged = fixing(&id, 1.0, expiry_ms, 2);
        forged.signature_valid = false;
        assert!(matches!(
            v.settle_expired(&alice, &forged, expiry_ms),
            Err(ExecError::BadSignature)
        ));
        let mut wrong_signer = fixing(&id, 1.0, expiry_ms, 2);
        wrong_signer.signer = "mallory.near".into();
        assert!(matches!(
            v.settle_expired(&alice, &wrong_signer, expiry_ms),
            Err(ExecError::BadSignature)
        ));
        // Fixing observed before the lot expired, even if submitted after:
        let early = fixing(&id, 1.0, expiry_ms - 1000, 2);
        assert!(matches!(
            v.settle_expired(&alice, &early, expiry_ms),
            Err(ExecError::PositionNotExpired)
        ));
        // Rejected attempts leave the position settleable:
        assert!(
            v.settle_expired(&alice, &fixing(&id, 1.0, expiry_ms, 2), expiry_ms)
                .unwrap()
                > 0
        );
    }

    #[test]
    fn settle_position_rejects_wrong_holder_and_double_settle() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        let fill = v
            .buy(
                &alice,
                &bundle(&id, 0.62, 100, 1),
                380_000_000,
                u64::MAX,
                NOW_MS + 500,
            )
            .unwrap();
        let lot = fill.position_id.unwrap();
        let fix = fixing(&id, 1.0, expiry_ms, 2);
        assert!(matches!(
            v.settle_position(&AccountId::new("bob"), lot, &fix, expiry_ms),
            Err(ExecError::Unauthorized)
        ));
        assert!(v.settle_position(&alice, lot, &fix, expiry_ms).unwrap() > 0);
        assert!(matches!(
            v.settle_position(&alice, lot, &fix, expiry_ms),
            Err(ExecError::PositionNotFound)
        ));
    }

    #[test]
    fn redemption_request_then_market_close_settles_everyone() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        let bob = AccountId::new("bob");
        // No position yet: nothing to redeem.
        assert!(matches!(
            v.request_redemption(&alice, &id, NOW_MS),
            Err(ExecError::PositionNotFound)
        ));
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        v.buy(
            &bob,
            &bundle(&id, 0.62, 100, 2),
            190_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        // Alice pre-registers redemption before the close (idempotent); Bob does not.
        v.request_redemption(&alice, &id, NOW_MS + 600).unwrap();
        v.request_redemption(&alice, &id, NOW_MS + 700).unwrap();
        assert!(v.redemption_requested(&alice, &id));
        assert!(!v.redemption_requested(&bob, &id));
        // Close-out before maturity and with a forged fixing are both rejected,
        // and neither closes anything.
        assert!(matches!(
            v.settle_market(&fixing(&id, 1.0, expiry_ms, 3), expiry_ms - 1),
            Err(ExecError::PositionNotExpired)
        ));
        let mut forged = fixing(&id, 1.0, expiry_ms, 3);
        forged.signature_valid = false;
        assert!(matches!(
            v.settle_market(&forged, expiry_ms),
            Err(ExecError::BadSignature)
        ));
        assert!(v.position(&alice, &id) > 0 && v.position(&bob, &id) > 0);
        // Market closes: one call settles right away, all positions closed.
        let (total, lots) = v
            .settle_market(&fixing(&id, 1.0, expiry_ms, 3), expiry_ms)
            .unwrap();
        assert_eq!(lots, 2);
        assert!(total > 0);
        assert_eq!(v.position(&alice, &id), 0);
        assert_eq!(v.position(&bob, &id), 0);
        // Payouts are credited to holders (neither initiated the close-out).
        assert!(v.claimable(&alice) > 380_000_000); // bought at ~62%, paid $10/unit
        assert!(v.claimable(&bob) > 0);
        assert_eq!(v.claimable(&alice) + v.claimable(&bob), total);
        // Requests cleared, market halted, LP free to withdraw afterwards.
        assert!(!v.redemption_requested(&alice, &id));
        assert!(v.markets[&id.0].halted);
        assert!(
            v.withdraw_lp(&AccountId::new("lp"), 1_000_000, expiry_ms)
                .is_ok()
        );
        // Re-running the close-out is a harmless no-op.
        assert_eq!(
            v.settle_market(&fixing(&id, 1.0, expiry_ms, 3), expiry_ms)
                .unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn market_close_on_no_closes_all_positions_for_zero() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        v.request_redemption(&alice, &id, NOW_MS + 600).unwrap();
        let (total, lots) = v
            .settle_market(&fixing(&id, 0.0, expiry_ms, 2), expiry_ms)
            .unwrap();
        assert_eq!((total, lots), (0, 1));
        assert_eq!(v.position(&alice, &id), 0);
        assert_eq!(v.claimable(&alice), 0);
    }

    #[test]
    fn redemption_rejected_for_never_closing_spot_market() {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 100_000_000_000)
            .unwrap();
        let id = MarketId::new("USDARS-SPOT");
        v.create_market(
            id.clone(),
            "USD/ARS".into(),
            "oracle.near".into(),
            Formula::synthetic_spot_v1(10_000_000, 1400.0),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 1450.0, 100, 1),
            100_000_000,
            u64::MAX,
            NOW_MS,
        )
        .unwrap();
        assert!(matches!(
            v.request_redemption(&alice, &id, NOW_MS),
            Err(ExecError::PositionNotExpired)
        ));
        assert!(matches!(
            v.settle_market(&fixing(&id, 1450.0, NOW_MS, 2), NOW_MS),
            Err(ExecError::PositionNotExpired)
        ));
    }

    #[test]
    fn lp_withdrawal_blocked_until_expired_lots_settle() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        let lp = AccountId::new("lp");
        assert!(matches!(
            v.withdraw_lp(&lp, 1_000_000, expiry_ms),
            Err(ExecError::UnsettledExpiredPositions)
        ));
        v.settle_expired(&alice, &fixing(&id, 1.0, expiry_ms, 2), expiry_ms)
            .unwrap();
        assert!(v.withdraw_lp(&lp, 1_000_000, expiry_ms).is_ok());
    }

    #[test]
    fn synthetic_spot_fx_has_no_expiry_and_no_payout_cap() {
        // FX never expires: uncapped mark, exit is sell-at-bid at any time.
        let f = Formula::synthetic_spot_v1(10_000_000, 1400.0);
        assert_eq!(f.maturity_unix_secs, i64::MAX);
        assert_eq!(f.fair(1400.0), 10_000_000); // par
        assert_eq!(f.fair(4200.0), 30_000_000); // 3x rate move -> $30, NOT capped at $20
        assert_eq!(f.max_payout(), 20_000_000); // reserve stress only (2x), not a payout cap

        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 100_000_000_000)
            .unwrap();
        let id = MarketId::new("USDARS");
        v.create_market(
            id.clone(),
            "USD/ARS".into(),
            "oracle.near".into(),
            f,
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        let alice = AccountId::new("alice");
        let mut b = bundle(&id, 1000.0, 100, 1);
        b.spot = 1400.0; // inception of this fixture differs; quote at par
        v.buy(&alice, &b, 100_000_000, u64::MAX, NOW_MS).unwrap();
        // Far future: an expiring note would be halted; synthetic spot still trades.
        let later_ms: i64 = 4_102_444_800_000; // year 2100
        let mut b2 = bundle(&id, 1000.0, 100, 2);
        b2.spot = 2100.0;
        b2.observed_at_ms = later_ms - 100;
        b2.observed_at_unix_secs = (later_ms - 100) / 1000;
        let held = v.position(&alice, &id);
        let sell = v.sell(&alice, &b2, held, 0, later_ms).unwrap();
        assert!(sell.usdc_minor > 100_000_000); // rate +50% -> profitable close, no expiry involved
    }

    #[test]
    fn market_creation_costs_ten_dollars_into_shared_vault() {
        let mut v = SharedVault::new(); // empty vault: fee alone must land as collateral
        let id = MarketId::new("USDARS");
        let f = Formula::synthetic_spot_v1(10_000_000, 1400.0);
        // $9.99 -> rejected, nothing created, nothing charged.
        assert!(
            v.create_market(
                id.clone(),
                "USD/ARS".into(),
                "oracle.near".into(),
                f.clone(),
                9_990_000
            )
            .is_err()
        );
        assert_eq!(v.collateral_usdc_minor, 0);
        assert!(v.markets.is_empty());
        // exactly $10 -> created, fee is now vault collateral backing every market.
        v.create_market(
            id.clone(),
            "USD/ARS".into(),
            "oracle.near".into(),
            f.clone(),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        assert_eq!(v.collateral_usdc_minor, 10_000_000);
        // duplicate creation does not charge again.
        assert!(
            v.create_market(
                id,
                "USD/ARS".into(),
                "oracle.near".into(),
                f,
                MARKET_CREATION_FEE_USDC_MINOR
            )
            .is_err()
        );
        assert_eq!(v.collateral_usdc_minor, 10_000_000);
    }

    fn protocol_with_two_vaults() -> (ZeroTickProtocol, MarketId, VaultId, VaultId) {
        let owner = AccountId::new("gov");
        let operator_a = AccountId::new("vault-a");
        let operator_b = AccountId::new("vault-b");
        let mut protocol = ZeroTickProtocol::new(owner.clone());
        protocol
            .register_oracle(
                &owner,
                crate::oracle::OracleAdapter::new("arg30-spot", "ARG30 spot, signed", 5, true),
            )
            .unwrap();
        let id = MarketId::new("ARG30");
        protocol
            .create_market(
                MarketDefinition {
                    id: id.clone(),
                    name: "Merval 30".into(),
                    oracle_id: "arg30-spot".into(),
                    oracle_signer: "oracle.near".into(),
                    formula_template: Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000),
                    risk_tier: MarketRiskTier::Tier5, // create_market must reset this to Unrated
                    halted: false,
                },
                MARKET_CREATION_FEE_USDC_MINOR,
            )
            .unwrap();

        let vault_a = VaultId::new("conservative");
        let vault_b = VaultId::new("aggressive");
        let config_a = VaultConfig {
            name: "Conservative".into(),
            withdrawal_epoch_secs: 7 * 86_400,
            ..Default::default()
        };
        protocol
            .create_vault(&operator_a, vault_a.clone(), config_a, NOW_MS)
            .unwrap();
        let config_b = VaultConfig {
            name: "Aggressive".into(),
            ..Default::default()
        };
        protocol
            .create_vault(&operator_b, vault_b.clone(), config_b, NOW_MS)
            .unwrap();
        protocol
            .deposit_lp(&vault_a, &AccountId::new("lp-a"), 100_000_000_000)
            .unwrap();
        protocol
            .deposit_lp(&vault_b, &AccountId::new("lp-b"), 100_000_000_000)
            .unwrap();
        (protocol, id, vault_a, vault_b)
    }

    #[test]
    fn risk_tier_defaults_match_specification() {
        assert_eq!(MarketRiskTier::Unrated.default_allocation_bps(), 0);
        assert_eq!(MarketRiskTier::Tier1.default_allocation_bps(), 4_000);
        assert_eq!(MarketRiskTier::Tier2.default_allocation_bps(), 1_500);
        assert_eq!(MarketRiskTier::Tier3.default_allocation_bps(), 1_000);
        assert_eq!(MarketRiskTier::Tier4.default_allocation_bps(), 500);
        assert_eq!(MarketRiskTier::Tier5.default_allocation_bps(), 200);
    }

    #[test]
    fn unrated_market_is_zero_until_vault_overrides() {
        let (mut protocol, id, vault_a, _) = protocol_with_two_vaults();
        let operator_a = AccountId::new("vault-a");
        assert_eq!(protocol.markets[&id.0].risk_tier, MarketRiskTier::Unrated);
        protocol
            .enable_market(&operator_a, &vault_a, &id, None, None)
            .unwrap();
        let trade = bundle(&id, 1000.0, 100, 1);
        let routed = protocol.buy_routed(
            &AccountId::new("alice"),
            &trade,
            100_000_000,
            u64::MAX,
            Some(7 * 86_400),
            NOW_MS,
        );
        assert!(routed.fills.is_empty());
        assert_eq!(routed.unfilled_usdc_minor, 100_000_000);

        protocol
            .set_market_allocation(&operator_a, &vault_a, &id, Some(200))
            .unwrap();
        let routed = protocol.buy_routed(
            &AccountId::new("alice"),
            &trade,
            100_000_000,
            u64::MAX,
            Some(7 * 86_400),
            NOW_MS,
        );
        assert_eq!(routed.fills.len(), 1);
        assert_eq!(routed.unfilled_usdc_minor, 0);
        let fill = &routed.fills[0].fill;
        assert_eq!(fill.expires_at_ms, Some(NOW_MS + 7 * 86_400 * 1000));
        let lot = &protocol.vault(&vault_a).unwrap().position_lots[&fill.position_id.unwrap()];
        assert_eq!(lot.principal_usdc_minor, 100_000_000);
        assert_eq!(lot.entry_spot, 1000.0);
    }

    #[test]
    fn tier_allocation_is_reference_notional_and_override_wins() {
        let (mut protocol, id, vault_a, _) = protocol_with_two_vaults();
        let owner = AccountId::new("gov");
        let operator_a = AccountId::new("vault-a");
        protocol
            .assign_market_tier(&owner, &id, MarketRiskTier::Tier5)
            .unwrap();
        protocol
            .enable_market(&operator_a, &vault_a, &id, None, None)
            .unwrap();
        // $100k NAV x Tier 5's 2% = $2k reference cap; a $2.5k order fails.
        let err = protocol.buy_in_vault(
            &vault_a,
            &AccountId::new("alice"),
            &bundle(&id, 1000.0, 100, 1),
            2_500_000_000,
            u64::MAX,
            Some(7 * 86_400),
            NOW_MS,
        );
        assert!(matches!(err, Err(ExecError::ExposureExceeded { .. })));
        protocol
            .buy_in_vault(
                &vault_a,
                &AccountId::new("alice"),
                &bundle(&id, 1000.0, 100, 1),
                1_000_000_000,
                u64::MAX,
                Some(7 * 86_400),
                NOW_MS,
            )
            .unwrap();
    }

    #[test]
    fn router_splits_from_cheapest_vault_to_next() {
        let (mut protocol, id, vault_a, vault_b) = protocol_with_two_vaults();
        let owner = AccountId::new("gov");
        protocol
            .assign_market_tier(&owner, &id, MarketRiskTier::Tier1)
            .unwrap();
        let cheap = PricingOverrides {
            base_spread_bps: Some(20),
            ..Default::default()
        };
        let expensive = PricingOverrides {
            base_spread_bps: Some(120),
            ..Default::default()
        };
        protocol
            .enable_market(
                &AccountId::new("vault-a"),
                &vault_a,
                &id,
                Some(&cheap),
                Some(100),
            )
            .unwrap();
        protocol
            .enable_market(
                &AccountId::new("vault-b"),
                &vault_b,
                &id,
                Some(&expensive),
                Some(4_000),
            )
            .unwrap();
        let quotes = protocol.quote_all(&bundle(&id, 1000.0, 100, 1), Some(7 * 86_400), NOW_MS);
        assert_eq!(quotes[0].0, vault_a);
        assert!(quotes[0].1.ask_usdc_minor < quotes[1].1.ask_usdc_minor);

        let routed = protocol.buy_routed(
            &AccountId::new("alice"),
            &bundle(&id, 1000.0, 100, 1),
            2_500_000_000,
            u64::MAX,
            Some(7 * 86_400),
            NOW_MS,
        );
        assert_eq!(routed.unfilled_usdc_minor, 0);
        assert_eq!(routed.fills.len(), 2);
        assert_eq!(routed.fills[0].vault_id, vault_a);
        assert_eq!(routed.fills[1].vault_id, vault_b);
        assert_eq!(
            routed.fills.iter().map(|f| f.fill.usdc_minor).sum::<u64>(),
            2_500_000_000
        );
    }

    #[test]
    fn allocation_limits_do_not_need_to_sum_to_one_hundred_percent() {
        let vault = SharedVault::new();
        // Each market's Tier 1 ceiling is independently 40% of NAV; five such
        // markets may exist simultaneously. The reference caps sum to 200%,
        // while capital remains shared and global solvency still applies.
        let total_policy_capacity: u32 = (0..5)
            .map(|_| MarketRiskTier::Tier1.default_allocation_bps() as u32)
            .sum();
        assert_eq!(total_policy_capacity, 20_000);
        assert_eq!(vault.config.max_gross_reference_bps, 50_000);
    }

    #[test]
    fn lp_withdrawal_uses_nav_and_waits_for_epoch() {
        let (mut protocol, _id, vault_a, _) = protocol_with_two_vaults();
        let lp = AccountId::new("lp-a");
        let err = protocol.withdraw_lp(&vault_a, &lp, 10_000_000, NOW_MS);
        assert!(matches!(err, Err(ExecError::WithdrawalLocked { .. })));
        let paid = protocol
            .withdraw_lp(&vault_a, &lp, 5_000_000_000, NOW_MS + 7 * 86_400 * 1000)
            .unwrap();
        assert_eq!(paid, 5_000_000_000); // no trades: shares redeem at par
        assert_eq!(protocol.vault(&vault_a).unwrap().nav(), 95_000_000_000);
    }

    #[test]
    fn direct_formula_also_works_in_same_vault() {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 50_000_000_000).unwrap();
        let id = MarketId::new("CU-DEC26");
        let mut f = Formula::index_capped_v1(10_000_000, 1.0, 1_800_000_000);
        f.kind = FormulaKind::DirectV1;
        v.create_market(
            id.clone(),
            "Copper".into(),
            "oracle.near".into(),
            f,
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        let q = v.quote(&bundle(&id, 10_000_000.0, 100, 1), NOW_MS).unwrap();
        assert_eq!(q.fair_usdc_minor, 10_000_000);
        assert!(q.ask_usdc_minor > q.fair_usdc_minor && q.bid_usdc_minor < q.fair_usdc_minor);
    }

    // ---- Regression tests for the product review (P0) ----

    fn spot_vault() -> (SharedVault, MarketId) {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 100_000_000_000)
            .unwrap();
        let id = MarketId::new("USDARS");
        v.create_market(
            id.clone(),
            "USD/ARS".into(),
            "oracle.near".into(),
            Formula::synthetic_spot_v1(10_000_000, 1400.0),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&id, Some(10_000)).unwrap();
        (v, id)
    }

    #[test]
    fn regression_sell_cannot_drain_beyond_reserved_cap() {
        let (mut v, id) = spot_vault();
        let alice = AccountId::new("alice");
        let fill = v
            .buy(
                &alice,
                &bundle(&id, 1400.0, 100, 1),
                5_000_000_000,
                u64::MAX,
                NOW_MS,
            )
            .unwrap();
        let coll_before = v.collateral_usdc_minor;
        // Oracle prints 20x: uncapped fair would pay ~$100k on a $5k trade.
        // Sell must be capped at the lot's reserved max (2x stress reserve),
        // and the vault must stay able to cover what remains reserved.
        let sell = v
            .sell(
                &alice,
                &bundle(&id, 28000.0, 100, 2),
                fill.units_minor,
                0,
                NOW_MS,
            )
            .unwrap();
        let lot_max = ((fill.units_minor as u128 * 20_000_000u128) / 1_000_000) as u64;
        assert!(
            sell.usdc_minor <= lot_max,
            "paid {} > lot max {}",
            sell.usdc_minor,
            lot_max
        );
        assert!(v.collateral_usdc_minor >= coll_before - lot_max);
        assert!(v.coverage() >= 1.0);
    }

    #[test]
    fn regression_non_finite_and_bad_spots_rejected() {
        let (v, id) = spot_vault();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -5.0] {
            assert!(
                v.quote(&bundle(&id, bad, 100, 1), NOW_MS).is_err(),
                "spot {bad} accepted"
            );
        }
        // fair() itself must never panic on hostile input.
        let f = Formula::synthetic_spot_v1(10_000_000, 1400.0);
        assert_eq!(f.fair(f64::NAN), 0);
        assert_eq!(f.fair(f64::INFINITY), 0);
        assert_eq!(f.fair(-1.0), 0);
    }

    #[test]
    fn regression_create_market_rejects_bad_formula() {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 10_000_000_000).unwrap();
        for (name, f) in [
            (
                "zero-inception",
                Formula::synthetic_spot_v1(10_000_000, 0.0),
            ),
            (
                "nan-inception",
                Formula::synthetic_spot_v1(10_000_000, f64::NAN),
            ),
            ("zero-base", Formula::synthetic_spot_v1(0, 1400.0)),
        ] {
            let id = MarketId::new(name);
            assert!(
                v.create_market(
                    id,
                    name.into(),
                    "oracle.near".into(),
                    f,
                    MARKET_CREATION_FEE_USDC_MINOR
                )
                .is_err(),
                "{name} accepted"
            );
        }
    }

    #[test]
    fn regression_new_market_starts_unrated_zero_allocation() {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), 100_000_000_000)
            .unwrap();
        let id = MarketId::new("NEW-MKT");
        v.create_market(
            id.clone(),
            "New".into(),
            "oracle.near".into(),
            Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000),
            MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        assert_eq!(v.markets[&id.0].effective_allocation_bps(), 0);
        let err = v.buy(
            &AccountId::new("alice"),
            &bundle(&id, 1000.0, 100, 1),
            100_000_000,
            u64::MAX,
            NOW_MS,
        );
        assert!(
            matches!(err, Err(ExecError::ExposureExceeded { .. })),
            "got {err:?}"
        );
    }

    #[test]
    fn regression_same_seq_different_bundle_rejected() {
        let (mut v, id) = vault();
        let alice = AccountId::new("alice");
        let ok = bundle(&id, 1000.0, 100, 5);
        v.buy(&alice, &ok, 1_000_000, u64::MAX, NOW_MS).unwrap();
        // Same seq, different spot: an alternate bundle must not pass as seq 5.
        let evil = bundle(&id, 1500.0, 100, 5);
        assert_eq!(
            v.buy(&alice, &evil, 1_000_000, u64::MAX, NOW_MS),
            Err(ExecError::ReplayNonce)
        );
    }

    #[test]
    fn regression_binary_fixing_must_be_zero_or_one_within_window() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        // A trading probability (0.62) is not a resolution.
        assert!(
            v.settle_expired(&alice, &fixing(&id, 0.62, expiry_ms, 2), expiry_ms)
                .is_err()
        );
        // A fixing observed a week after expiry is lookahead, not settlement.
        let late = expiry_ms + 7 * 86_400 * 1000;
        assert!(
            v.settle_expired(&alice, &fixing(&id, 1.0, late, 2), late)
                .is_err()
        );
        // A clean 1.0 at expiry still settles.
        assert!(
            v.settle_expired(&alice, &fixing(&id, 1.0, expiry_ms, 2), expiry_ms)
                .unwrap()
                > 0
        );
    }

    #[test]
    fn regression_deposit_overflow_rejected() {
        let mut v = SharedVault::new();
        v.deposit_lp(&AccountId::new("lp"), u64::MAX - 10).unwrap();
        assert!(v.deposit_lp(&AccountId::new("lp"), 100).is_err());
        assert_eq!(v.collateral_usdc_minor, u64::MAX - 10);
    }

    #[test]
    fn regression_dust_trade_rejected() {
        let (mut v, id) = vault();
        let err = v.buy(
            &AccountId::new("alice"),
            &bundle(&id, 1000.0, 100, 1),
            1,
            u64::MAX,
            NOW_MS,
        );
        assert!(err.is_err(), "1-minor-unit dust buy accepted: {err:?}");
    }

    #[test]
    fn regression_claim_pays_out_claimable_once() {
        let (mut v, id, expiry_ms) = binary_vault();
        let alice = AccountId::new("alice");
        v.buy(
            &alice,
            &bundle(&id, 0.62, 100, 1),
            380_000_000,
            u64::MAX,
            NOW_MS + 500,
        )
        .unwrap();
        v.settle_market(&fixing(&id, 1.0, expiry_ms, 2), expiry_ms)
            .unwrap();
        let owed = v.claimable(&alice);
        assert!(owed > 0);
        assert_eq!(v.claim(&alice), owed);
        assert_eq!(v.claimable(&alice), 0);
        assert_eq!(v.claim(&alice), 0);
    }

    #[test]
    fn regression_settle_market_paged() {
        let (mut v, id, expiry_ms) = binary_vault();
        for (who, seq) in [("a", 1u64), ("b", 2), ("c", 3)] {
            v.buy(
                &AccountId::new(who),
                &bundle(&id, 0.62, 100, seq),
                100_000_000,
                u64::MAX,
                NOW_MS + 500,
            )
            .unwrap();
        }
        let fix = fixing(&id, 1.0, expiry_ms, 4);
        let (_t, n1, rem1) = v.settle_market_paged(&fix, expiry_ms, 2).unwrap();
        assert_eq!((n1, rem1), (2, true));
        let (_t, n2, rem2) = v.settle_market_paged(&fix, expiry_ms, 2).unwrap();
        assert_eq!((n2, rem2), (1, false));
        assert!(v.markets[&id.0].halted);
    }
}
