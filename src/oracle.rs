//! Oracle adapter registry — permissionless market creation gate.
//!
//! A market may only reference a registered adapter that satisfies the
//! minimum oracle standard: authenticated source, unambiguous units,
//! freshness bound, manipulation resistance, reproducible settlement value.

use crate::types::MarketId;
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::HashMap;

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct OracleAdapter {
    pub id: String,
    pub description: String,
    pub max_staleness_secs: i64,
    // For non-live indices (CPI, housing) live quoting is disabled;
    // those markets are fixed-maturity settlement only.
    pub supports_live_quoting: bool,
}

impl OracleAdapter {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        max_staleness_secs: i64,
        supports_live_quoting: bool,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            max_staleness_secs,
            supports_live_quoting,
        }
    }
}

#[derive(Default, BorshSerialize, BorshDeserialize)]
pub struct OracleRegistry {
    adapters: HashMap<String, OracleAdapter>,
}

impl OracleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, a: OracleAdapter) {
        self.adapters.insert(a.id.clone(), a);
    }

    pub fn get(&self, id: &str) -> Option<&OracleAdapter> {
        self.adapters.get(id)
    }

    pub fn can_create_market(&self, oracle_id: &str, _market: &MarketId) -> bool {
        self.adapters.contains_key(oracle_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_oracle_cannot_create() {
        let mut r = OracleRegistry::new();
        r.register(OracleAdapter::new(
            "arg30-spot",
            "ARG30 spot, signed",
            5,
            true,
        ));
        assert!(r.can_create_market("arg30-spot", &MarketId::new("ARG30-USDC")));
        assert!(!r.can_create_market("untrusted-dex", &MarketId::new("FOO")));
    }
}
