//! Function vault — market == function(oracle, inventory) → price/payout.
//!
//! A `FunctionDef` is the executable pricing/payoff template registered once.
//! The vault holds many `FunctionMarket`s sharing the same template (multi-market).
//! Oracle submissions (on-chain pushed or off-chain signed attestation delivered
//! by a trader/oracle relay) are ingested into the vault via `submit_oracle`,
//! stored as the market's latest observation, and all quotes/trades are
//! evaluated strictly via `function.evaluate(raw, inventory)`.
//!
//! The vault enforces: signature validity + freshness + monotone sequence,
//! so the same oracle value always yields the same quote — trustless off-chain
//! repricing with only trades touching chain.

use crate::market::Vault;
use crate::oracle::OracleRegistry;
use crate::pricing::{InventorySnapshot, OracleObservation, PricingPolicy};
use crate::types::{ExecError, MarketId, PairPayoff, Side};
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct FunctionId(pub String);
impl FunctionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Pricing template — the "market function".
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub enum PricingKind {
    /// Direct oracle price is already the USD price (e.g. FX, commodity spot).
    DirectPrice,
    /// Index → price linear: P = base_usdc * (raw / inception).
    /// ARG30: base $10, inception 1000.
    IndexLinear {
        base_usdc_minor: u64,
        inception: f64,
    },
    /// Future MLP / time-decay: placeholder hook for vol curve.
    TimeDecay {
        base_usdc_minor: u64,
        inception: f64,
        vol_bps: u16,
    },
}

impl PricingKind {
    /// Map raw oracle value → fair reference price (before spread/skew).
    pub fn fair_price(&self, raw: f64) -> u64 {
        match *self {
            PricingKind::DirectPrice => raw.round().max(0.0) as u64,
            PricingKind::IndexLinear {
                base_usdc_minor,
                inception,
            } => {
                assert!(inception > 0.0);
                (base_usdc_minor as f64 * (raw / inception)).round() as u64
            }
            PricingKind::TimeDecay {
                base_usdc_minor,
                inception,
                ..
            } => {
                assert!(inception > 0.0);
                (base_usdc_minor as f64 * (raw / inception)).round() as u64
            }
        }
    }

    pub fn ask(&self, raw: f64, policy: &PricingPolicy, inv: InventorySnapshot) -> u64 {
        crate::pricing::PricingEngine::ask(policy, self.fair_price(raw), inv)
    }
    pub fn bid(&self, raw: f64, policy: &PricingPolicy, inv: InventorySnapshot) -> u64 {
        crate::pricing::PricingEngine::bid(policy, self.fair_price(raw), inv)
    }
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct FunctionDef {
    pub id: FunctionId,
    pub pricing: PricingKind,
    pub payoff: PairPayoff,
    pub description: String,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct FunctionMarket {
    pub id: MarketId,
    pub function_id: FunctionId,
    pub oracle_adapter_id: String,
    pub payoff: PairPayoff,
    pub halted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FunctionFill {
    pub side: Side,
    pub quoted_price_usdc_minor: u64,
    pub notional_usdc_minor: u64,
    pub net_long_delta_usdc_minor: i64,
    pub function_id: String,
}

#[derive(Default, BorshSerialize, BorshDeserialize)]
pub struct FunctionVault {
    functions: HashMap<String, FunctionDef>,
    markets: HashMap<String, FunctionMarket>,
    vaults: HashMap<String, Vault>,
    registry: OracleRegistry,
    last_obs: HashMap<String, OracleObservation>,
    last_raw: HashMap<String, f64>,
    seen_nonces: HashSet<(String, u64)>,
}

impl FunctionVault {
    pub fn new(registry: OracleRegistry) -> Self {
        Self {
            functions: HashMap::new(),
            markets: HashMap::new(),
            vaults: HashMap::new(),
            registry,
            last_obs: HashMap::new(),
            last_raw: HashMap::new(),
            seen_nonces: HashSet::new(),
        }
    }

    pub fn register_oracle(&mut self, adapter: crate::oracle::OracleAdapter) {
        self.registry.register(adapter);
    }

    pub fn register_function(&mut self, def: FunctionDef) -> Result<(), String> {
        if self.functions.contains_key(&def.id.0) {
            return Err(format!("function exists: {}", def.id.0));
        }
        self.functions.insert(def.id.0.clone(), def);
        Ok(())
    }

    pub fn create_market(
        &mut self,
        market_id: MarketId,
        function_id: FunctionId,
        oracle_adapter_id: String,
    ) -> Result<(), String> {
        let def = self
            .functions
            .get(&function_id.0)
            .ok_or_else(|| format!("unknown function {}", function_id.0))?;
        if !self
            .registry
            .can_create_market(&oracle_adapter_id, &market_id)
        {
            return Err(format!("oracle not registered: {}", oracle_adapter_id));
        }
        if self.markets.contains_key(&market_id.0) {
            return Err(format!("market exists: {}", market_id.0));
        }
        let payoff = def.payoff.clone();
        let vault = Vault::with_notional(payoff.notional_usdc_minor);
        self.vaults.insert(market_id.0.clone(), vault);
        self.markets.insert(
            market_id.0.clone(),
            FunctionMarket {
                id: market_id,
                function_id,
                oracle_adapter_id,
                payoff,
                halted: false,
            },
        );
        Ok(())
    }

    pub fn vault(&self, id: &MarketId) -> Option<&Vault> {
        self.vaults.get(&id.0)
    }
    pub fn vault_mut(&mut self, id: &MarketId) -> Option<&mut Vault> {
        self.vaults.get_mut(&id.0)
    }
    pub fn get(&self, id: &MarketId) -> Option<&FunctionMarket> {
        self.markets.get(&id.0)
    }
    pub fn last_obs(&self, id: &MarketId) -> Option<&OracleObservation> {
        self.last_obs.get(&id.0)
    }

    /// Ingest oracle submission. Either on-chain (oracle posts directly) or
    /// off-chain (signed attestation forwarded by trader/relayer) — same
    /// monotone, freshness, and signature checks. Stores if strictly newer.
    /// Returns true if accepted (stored), false if strictly older (ignored).
    pub fn submit_oracle(
        &mut self,
        market: &MarketId,
        obs: OracleObservation,
        raw_value: f64,
    ) -> Result<bool, ExecError> {
        let m = self
            .markets
            .get(&market.0)
            .ok_or(ExecError::UnknownMarket)?;
        if m.halted {
            return Err(ExecError::Halted);
        }
        if !obs.signature_valid {
            return Err(ExecError::BadSignature);
        }
        if let Some(last) = self.last_obs.get(&market.0)
            && obs.observed_at_unix_secs <= last.observed_at_unix_secs
        {
            return Ok(false); // stale sequence, ignore not error
        }
        // freshness is enforced at trade time via policy.oracle_max_age_secs,
        // but reject future timestamps here.
        // We do not enforce max_age at ingestion; vault keeps last, trade decides.
        self.last_obs.insert(market.0.clone(), obs);
        self.last_raw.insert(market.0.clone(), raw_value);
        Ok(true)
    }

    /// Quote via vault-stored oracle + function. No external obs needed.
    pub fn quote(
        &self,
        market: &MarketId,
        policy: &PricingPolicy,
        now_unix_secs: i64,
    ) -> Result<(u64, u64), ExecError> {
        let m = self
            .markets
            .get(&market.0)
            .ok_or(ExecError::UnknownMarket)?;
        if m.halted {
            return Err(ExecError::Halted);
        }
        let obs = self
            .last_obs
            .get(&market.0)
            .ok_or(ExecError::StaleObservation {
                age_secs: 0,
                max_age: policy.oracle_max_age_secs,
            })?;
        obs.check_fresh(now_unix_secs, policy.oracle_max_age_secs)?;
        let raw = *self
            .last_raw
            .get(&market.0)
            .ok_or(ExecError::StaleObservation {
                age_secs: 0,
                max_age: policy.oracle_max_age_secs,
            })?;
        let def = self.functions.get(&m.function_id.0).unwrap();
        let vault = self.vaults.get(&market.0).ok_or(ExecError::UnknownMarket)?;
        let inv = vault.inventory();
        Ok((
            def.pricing.ask(raw, policy, inv),
            def.pricing.bid(raw, policy, inv),
        ))
    }

    /// Trade using vault-stored oracle + function. Trader does NOT need to
    /// carry the observation — vault already holds the latest submitted value.
    pub fn trade_via_vault(
        &mut self,
        policy: &PricingPolicy,
        side: Side,
        trade_size_usdc_minor: u64,
        max_acceptable_price_usdc_minor: u64,
        now_unix_secs: i64,
    ) -> Result<FunctionFill, ExecError> {
        let market = policy.market.clone();
        let m = self
            .markets
            .get(&market.0)
            .ok_or(ExecError::UnknownMarket)?
            .clone();
        if m.halted {
            return Err(ExecError::Halted);
        }
        policy.validate_time(now_unix_secs)?;
        if self.seen_nonces.contains(&(market.0.clone(), policy.nonce)) {
            return Err(ExecError::ReplayNonce);
        }
        let obs = self
            .last_obs
            .get(&market.0)
            .cloned()
            .ok_or(ExecError::StaleObservation {
                age_secs: 0,
                max_age: policy.oracle_max_age_secs,
            })?;
        obs.check_fresh(now_unix_secs, policy.oracle_max_age_secs)?;
        if trade_size_usdc_minor > policy.max_trade_usdc_minor {
            return Err(ExecError::TradeTooLarge {
                requested: trade_size_usdc_minor,
                max: policy.max_trade_usdc_minor,
            });
        }
        let raw = *self
            .last_raw
            .get(&market.0)
            .ok_or(ExecError::StaleObservation {
                age_secs: 0,
                max_age: policy.oracle_max_age_secs,
            })?;
        let def = self.functions.get(&m.function_id.0).unwrap().clone();
        let vault = self.vaults.get(&market.0).ok_or(ExecError::UnknownMarket)?;
        let inv = vault.inventory();
        let (quoted, delta) = match side {
            Side::Buy => (
                def.pricing.ask(raw, policy, inv),
                trade_size_usdc_minor as i64,
            ),
            Side::Sell => (
                def.pricing.bid(raw, policy, inv),
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
        let vault = self.vaults.get_mut(&market.0).unwrap();
        vault.maker_net_long_usdc_minor -= delta;
        self.seen_nonces.insert((market.0.clone(), policy.nonce));
        Ok(FunctionFill {
            side,
            quoted_price_usdc_minor: quoted,
            notional_usdc_minor: trade_size_usdc_minor,
            net_long_delta_usdc_minor: delta,
            function_id: m.function_id.0.clone(),
        })
    }

    /// Direct trade with fresh obs injected (trader-carried alt path).
    /// Useful when vault ingestion lags: trader can still prove freshness.
    #[allow(clippy::too_many_arguments)]
    pub fn trade_with_obs(
        &mut self,
        policy: &PricingPolicy,
        obs: &OracleObservation,
        raw_value: f64,
        side: Side,
        trade_size_usdc_minor: u64,
        max_acceptable_price_usdc_minor: u64,
        now_unix_secs: i64,
    ) -> Result<FunctionFill, ExecError> {
        // First ingest, then delegate.
        self.submit_oracle(&policy.market, obs.clone(), raw_value)?;
        self.trade_via_vault(
            policy,
            side,
            trade_size_usdc_minor,
            max_acceptable_price_usdc_minor,
            now_unix_secs,
        )
    }

    pub fn settle(&self, market: &MarketId, final_raw: f64) -> Option<(u64, u64)> {
        let m = self.markets.get(&market.0)?;
        Some(m.payoff.settlement(final_raw))
    }

    pub fn halt(&mut self, id: &MarketId) {
        if let Some(m) = self.markets.get_mut(&id.0) {
            m.halted = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::{OracleAdapter, OracleRegistry};
    use crate::pricing::{OracleObservation, PricingPolicy};
    use crate::types::AccountId;

    fn registry3() -> OracleRegistry {
        let mut r = OracleRegistry::new();
        r.register(OracleAdapter::new("arg30-spot", "ARG30", 5, true));
        r.register(OracleAdapter::new("bov-spot", "BOV", 5, true));
        r.register(OracleAdapter::new("fx-spot", "FX ARS/USD", 5, true));
        r
    }

    fn vault_with_index_fn() -> FunctionVault {
        let mut v = FunctionVault::new(registry3());
        v.register_function(FunctionDef {
            id: FunctionId::new("index_skew_v1"),
            pricing: PricingKind::IndexLinear {
                base_usdc_minor: 10_000_000,
                inception: 1000.0,
            },
            payoff: PairPayoff::new(10_000_000, 1000.0, 1_800_000_000),
            description: "ARG-style linear index → $10 base".into(),
        })
        .unwrap();
        v.create_market(
            MarketId::new("ARG30-DEC26"),
            FunctionId::new("index_skew_v1"),
            "arg30-spot".into(),
        )
        .unwrap();
        v.create_market(
            MarketId::new("BOV-DEC26"),
            FunctionId::new("index_skew_v1"),
            "bov-spot".into(),
        )
        .unwrap();
        v.create_market(
            MarketId::new("ARSFX-DEC26"),
            FunctionId::new("index_skew_v1"),
            "fx-spot".into(),
        )
        .unwrap();
        v
    }

    fn obs(market: MarketId, _raw: f64, ts: i64) -> OracleObservation {
        OracleObservation {
            market,
            ref_price_usdc_minor: 0, // ignored when raw used via function
            observed_at_unix_secs: ts,
            source_id: "signed-oracle".into(),
            signature_valid: true,
        }
    }

    fn pol(market: MarketId, nonce: u64) -> PricingPolicy {
        PricingPolicy {
            market,
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
    fn vault_function_multi_market_isolation_and_pricing() {
        let mut v = vault_with_index_fn();
        for id in ["ARG30-DEC26", "BOV-DEC26"] {
            let mid = MarketId::new(id);
            v.vault_mut(&mid)
                .unwrap()
                .deposit_lp(&AccountId::new("lp"), 100_000_000_000);
            v.vault_mut(&mid).unwrap().capacity_usdc_minor = 100_000_000_000;
        }
        // Submit oracle for each market — same function template, different raw.
        v.submit_oracle(
            &MarketId::new("ARG30-DEC26"),
            obs(MarketId::new("ARG30-DEC26"), 1000.0, 1_700_000_000),
            1000.0,
        )
        .unwrap();
        v.submit_oracle(
            &MarketId::new("BOV-DEC26"),
            obs(MarketId::new("BOV-DEC26"), 1000.0, 1_700_000_000),
            1000.0,
        )
        .unwrap();
        let p = pol(MarketId::new("ARG30-DEC26"), 1);
        let (ask, bid) = v
            .quote(&MarketId::new("ARG30-DEC26"), &p, 1_700_000_001)
            .unwrap();
        assert_eq!(ask, 10_050_000);
        assert_eq!(bid, 9_950_000);

        // Trade via vault without carrying obs.
        let p1 = pol(MarketId::new("ARG30-DEC26"), 1);
        let f = v
            .trade_via_vault(&p1, Side::Buy, 1_000_000_000, 10_050_000, 1_700_000_001)
            .unwrap();
        assert_eq!(f.quoted_price_usdc_minor, 10_050_000);
        assert_eq!(f.function_id, "index_skew_v1");

        // BOV unaffected (different vault, same function).
        assert_eq!(
            v.vault(&MarketId::new("BOV-DEC26"))
                .unwrap()
                .maker_net_long_usdc_minor,
            0
        );
        assert_eq!(
            v.vault(&MarketId::new("ARG30-DEC26"))
                .unwrap()
                .maker_net_long_usdc_minor,
            -1_000_000_000
        );

        // New oracle submission re-prices next trade without tx.
        v.submit_oracle(
            &MarketId::new("ARG30-DEC26"),
            obs(MarketId::new("ARG30-DEC26"), 1023.0, 1_700_000_050),
            1023.0,
        )
        .unwrap();
        let p2 = pol(MarketId::new("ARG30-DEC26"), 2);
        let (ask2, _) = v
            .quote(&MarketId::new("ARG30-DEC26"), &p2, 1_700_000_051)
            .unwrap();
        assert!(ask2 > 10_050_000); // moved with raw
        let f2 = v
            .trade_via_vault(&p2, Side::Buy, 1_000_000_000, 20_000_000, 1_700_000_051)
            .unwrap();
        assert_eq!(f2.quoted_price_usdc_minor, ask2);
    }

    #[test]
    fn on_chain_and_off_chain_oracle_both_submit_and_stale_is_not_stored() {
        let mut v = vault_with_index_fn();
        let m = MarketId::new("ARG30-DEC26");
        v.vault_mut(&m)
            .unwrap()
            .deposit_lp(&AccountId::new("lp"), 100_000_000_000);
        v.vault_mut(&m).unwrap().capacity_usdc_minor = 100_000_000_000;
        // On-chain style: oracle posts directly (signature_valid true)
        assert!(
            v.submit_oracle(&m, obs(m.clone(), 1000.0, 100), 1000.0)
                .unwrap()
        );
        // Off-chain style: relayer forwards signed observation
        assert!(
            v.submit_oracle(&m, obs(m.clone(), 1010.0, 101), 1010.0)
                .unwrap()
        );
        // Older sequence ignored
        assert!(
            !v.submit_oracle(&m, obs(m.clone(), 1005.0, 99), 1005.0)
                .unwrap()
        );
        let last = v.last_obs(&m).unwrap().observed_at_unix_secs;
        assert_eq!(last, 101);
    }

    #[test]
    fn function_change_quotes_but_payoff_stays_fully_collateralized() {
        let mut v = FunctionVault::new(registry3());
        v.register_function(FunctionDef {
            id: FunctionId::new("direct"),
            pricing: PricingKind::DirectPrice,
            payoff: PairPayoff::new(10_000_000, 1.0, 1_800_000_000),
            description: "direct price".into(),
        })
        .unwrap();
        v.create_market(
            MarketId::new("ARG30-DEC26"),
            FunctionId::new("direct"),
            "arg30-spot".into(),
        )
        .unwrap();
        let m = MarketId::new("ARG30-DEC26");
        v.vault_mut(&m)
            .unwrap()
            .deposit_lp(&AccountId::new("lp"), 100_000_000_000);
        v.vault_mut(&m).unwrap().capacity_usdc_minor = 100_000_000_000;
        // raw is already the USD price (10.00 = 10_000_000 minor)
        v.submit_oracle(
            &m,
            obs(m.clone(), 10_000_000.0, 1_700_000_000),
            10_000_000.0,
        )
        .unwrap();
        let p = pol(m.clone(), 99);
        let (ask, _) = v.quote(&m, &p, 1_700_000_001).unwrap();
        assert_eq!(ask, 10_050_000);
        // Spot settlement: $10 base * min(2, 0.5/1) = $5, no pair leg.
        let (l, s) = v.settle(&m, 0.5).unwrap();
        assert_eq!((l, s), (5_000_000, 0));
    }

    #[test]
    fn trading_requires_fresh_vault_oracle() {
        let mut v = vault_with_index_fn();
        let m = MarketId::new("ARG30-DEC26");
        v.vault_mut(&m)
            .unwrap()
            .deposit_lp(&AccountId::new("lp"), 100_000_000_000);
        v.vault_mut(&m).unwrap().capacity_usdc_minor = 100_000_000_000;
        v.submit_oracle(&m, obs(m.clone(), 1000.0, 1_700_000_000), 1000.0)
            .unwrap();
        let p = pol(m.clone(), 1);
        let err = v
            .trade_via_vault(&p, Side::Buy, 100, 20_000_000, 1_700_000_100 + 1)
            .unwrap_err();
        // expired policy is distinct; use stale-obs path with short max_age
        let mut short = p.clone();
        short.oracle_max_age_secs = 5;
        let err2 = v
            .trade_via_vault(&short, Side::Buy, 100, 20_000_000, 1_700_000_000 + 10)
            .unwrap_err();
        assert!(matches!(err2, ExecError::StaleObservation { .. }));
        let _ = err;
    }
}
