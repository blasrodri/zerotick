//! ZeroTick protocol — permissionless market registry, competing
//! multi-market vaults, oracle-priced fixed-duration synthetic exposure.
//!
//! Canonical model lives in [`protocol`]: markets are registered once,
//! each vault independently enables markets and sets pricing/allocation
//! policy, signed oracle bundles are verified at execution, and a router
//! can split orders across vaults. `pricing` / `market` / `functions` are
//! earlier prototypes kept for reference; new code should build on
//! `protocol`.

pub mod functions;
pub mod market;
pub mod oracle;
pub mod pricing;
pub mod protocol;
pub mod types;

pub use functions::*;
pub use market::*;
pub use oracle::*;
pub use pricing::*;
pub use protocol::{
    Formula, FormulaKind, MARKET_CREATION_FEE_USDC_MINOR, MAX_FAIR_JUMP_MULT, MIN_TRADE_USDC_MINOR,
    MarketDefinition, MarketRiskTier, OracleBundle, PositionLot, PositionStatus, PricingOverrides,
    Quote, RoutedExecution, RoutedFill, SETTLEMENT_WINDOW_MS, SharedVault, SpotFill, SpotMarket,
    VaultConfig, ZeroTickProtocol,
};
pub use types::*;
