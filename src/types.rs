//! Shared types. Fixed-point USDC uses 6 decimals (u64 minor units); prices
//! use i64 fixed-point with 1e6 scale unless stated otherwise.
use borsh::{BorshDeserialize, BorshSerialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize)]
pub struct MarketId(pub String);

impl MarketId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize)]
pub struct VaultId(pub String);

impl VaultId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct AccountId(pub String);

impl AccountId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Synthetic position payoff (internal unit, not a token) — fully collateralized capped note.
/// One unit = one internal position unit, funded by capped max payout per unit.
/// Payout at settlement given I_T / I_0:
///   payout = base_usdc_minor * min(2.0, I_T / I_0)
/// Example base $10: -30%→$7, 0%→$10, +15%→$11.5, +100%→$20 (capped at 2x), +150%→$20.
/// Collateral locked per unit = max payout = base*2 (e.g. $20 for $10 base). Internal ledger position, not a withdrawable token.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct PairPayoff {
    pub notional_usdc_minor: u64, // e.g. 10_000_000 = $10 base
    pub inception_index: f64,     // I0, must be > 0
    pub maturity_unix_secs: i64,
}

impl PairPayoff {
    pub fn new(notional_usdc_minor: u64, inception_index: f64, maturity_unix_secs: i64) -> Self {
        Self {
            notional_usdc_minor,
            inception_index,
            maturity_unix_secs,
        }
    }
    pub fn max_payout(&self) -> u64 {
        self.notional_usdc_minor.saturating_mul(2)
    }

    /// Position payout in USDC minor units for a given final index.
    pub fn settlement(&self, final_index: f64) -> (u64, u64) {
        // Keep tuple for compat: (spot_payout, 0) — single payout, no pair.
        let p = self.spot_payout(final_index);
        (p, 0)
    }
    pub fn spot_payout(&self, final_index: f64) -> u64 {
        // Never panic on hostile input: invalid levels pay zero.
        if !self.inception_index.is_finite()
            || self.inception_index <= 0.0
            || !final_index.is_finite()
            || final_index < 0.0
        {
            return 0;
        }
        let ratio = (final_index / self.inception_index).clamp(0.0, 2.0);
        (self.notional_usdc_minor as f64 * ratio).round() as u64
    }
}

/// Spot trading only: buy opens/increases the internal position, sell closes/decreases it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ExecError {
    PolicyExpired,
    StaleObservation {
        age_secs: i64,
        max_age: i64,
    },
    BadSignature,
    TradeTooLarge {
        requested: u64,
        max: u64,
    },
    ExposureExceeded {
        would_be: u64,
        max: u64,
    },
    SlippageExceeded {
        quoted: u64,
        max_acceptable: u64,
    },
    InsufficientCollateral {
        available: u64,
        required: u64,
    },
    Halted,
    UnknownMarket,
    ReplayNonce,
    AmountOverflow,
    /// 1m deviation beyond the formula limit — trading paused for this bundle.
    CircuitBreaker,
    UnknownVault,
    MarketNotEnabled,
    InvalidDuration,
    PositionNotFound,
    PositionNotExpired,
    WithdrawalLocked {
        next_withdrawal_at_ms: i64,
    },
    UnsettledExpiredPositions,
    AlreadyExists,
    Unauthorized,
    /// Spot was non-finite, out of range for the formula kind, or (for a
    /// binary fixing) not exactly 0 or 1.
    InvalidSpot,
    /// Formula parameters failed validation at market creation.
    InvalidFormula,
    TradeTooSmall {
        requested: u64,
        min: u64,
    },
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for ExecError {}
