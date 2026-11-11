//! ZeroTick NEAR contract — shared vault, one formula per market,
//! signed oracle bundles verified on-chain, real NEP-141 custody.
//!
//! CUSTODY MODEL (read this before depositing anything)
//! - Funds enter ONLY via `ft_transfer_call` on the configured USDC token
//!   (`ft_on_transfer` below). The token amount actually transferred is the
//!   amount credited — caller-supplied strings never mint value.
//! - `msg` on the transfer selects the action:
//!
//!   ```text
//!   {"action":"deposit_lp"}
//!   {"action":"create_market", market_id, name, oracle_signer,
//!    oracle_pubkey:[32 bytes], kind, base_usdc_minor, inception,
//!    maturity_unix_secs}              (amount = creation fee, >= $10)
//!   {"action":"buy", market_id, max_price_usdc_minor, spot,
//!    deviation_bps_1m, observed_at_ms, seq, signature:[64 bytes]}
//!                                      (amount = USDC spent)
//!   ```
//! - Funds leave ONLY via `claim()`: sell/redeem/settle/withdraw credit an
//!   internal `claimable` balance, and `claim()` transfers the real token,
//!   restoring the balance if the transfer promise fails.
//! - New markets start at 0% vault allocation. The owner must call
//!   `set_market_allocation` before anyone can buy. Creation alone can never
//!   draft LP collateral.
//!
//! ORACLE SIGNATURE (canonical payload, domain separated)
//!   message = "zerotick.oracle.v2|{contract_account}|{market_id}|{spot}|
//!              {deviation_bps_1m}|{observed_at_ms}|{seq}"   (UTF-8 bytes)
//!   signature = ed25519 sign(message) by the market's registered oracle key
//! Canonical encoding: `spot` must equal Rust's `Display` of the parsed f64
//! ("1450", "0.62"); integers must be plain decimal ("1001", no leading
//! zeros/signs). Non-canonical or non-finite values are rejected.
//! Sequence is monotonic per market; reusing the identical bundle is allowed
//! until superseded, a different bundle with a seen seq is rejected.

// NEAR external methods take flat oracle-bundle arguments by ABI design.
#![allow(clippy::too_many_arguments)]

use near_sdk::json_types::U128;
use near_sdk::{
    AccountId as NearAccountId, Gas, NearToken, Promise, PromiseOrValue, env, ext_contract, near,
};
use std::collections::HashMap;
use zerotick::protocol::{Formula, FormulaKind, OracleBundle, SharedVault};
use zerotick::types::{AccountId as ZAccountId, MarketId};

#[ext_contract(ext_ft)]
pub trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: NearAccountId, amount: U128, memo: Option<String>);
}

const GAS_FT_TRANSFER: Gas = Gas::from_tgas(30);
const GAS_CLAIM_CALLBACK: Gas = Gas::from_tgas(10);
/// Max lots settled per `settle_market` call (gas bound); call again if `more`.
const SETTLE_PAGE: u32 = 50;

#[near(contract_state)]
pub struct ZeroTickContract {
    owner: NearAccountId,
    usdc_token: NearAccountId,
    vault: SharedVault,
    /// market_id -> 32-byte ed25519 oracle public key (registered at creation)
    oracle_pubkeys: HashMap<String, Vec<u8>>,
}

impl Default for ZeroTickContract {
    fn default() -> Self {
        let me = env::current_account_id();
        Self {
            owner: me.clone(),
            usdc_token: me,
            vault: SharedVault::new(),
            oracle_pubkeys: HashMap::new(),
        }
    }
}

pub struct BundleArgs {
    pub spot: String,
    pub deviation_bps_1m: u16,
    pub observed_at_ms: String,
    pub seq: String,
    pub signature: Vec<u8>,
}

fn p_u64(s: &str, what: &str) -> u64 {
    let v: u64 = s
        .parse()
        .unwrap_or_else(|_| env::panic_str(&format!("bad {what}")));
    if v.to_string() != s {
        env::panic_str(&format!("non-canonical {what}"));
    }
    v
}
fn p_i64(s: &str, what: &str) -> i64 {
    let v: i64 = s
        .parse()
        .unwrap_or_else(|_| env::panic_str(&format!("bad {what}")));
    if v.to_string() != s {
        env::panic_str(&format!("non-canonical {what}"));
    }
    v
}
fn p_spot(s: &str) -> f64 {
    let v: f64 = s.parse().unwrap_or_else(|_| env::panic_str("bad spot"));
    if !v.is_finite() {
        env::panic_str("spot must be finite");
    }
    if format!("{v}") != s {
        env::panic_str("non-canonical spot encoding");
    }
    v
}
fn p_amount(amount: U128) -> u64 {
    u64::try_from(amount.0).unwrap_or_else(|_| env::panic_str("amount exceeds u64"))
}
fn jstr(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| env::panic_str(&format!("missing {key}")))
        .to_string()
}
fn jbytes(v: &serde_json::Value, key: &str) -> Vec<u8> {
    v.get(key)
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| env::panic_str(&format!("missing {key}")))
        .iter()
        .map(|x| {
            u8::try_from(x.as_u64().unwrap_or_else(|| env::panic_str("bad byte")))
                .unwrap_or_else(|_| env::panic_str("bad byte"))
        })
        .collect()
}
fn bundle_from_json(v: &serde_json::Value) -> BundleArgs {
    BundleArgs {
        spot: jstr(v, "spot"),
        deviation_bps_1m: u16::try_from(
            v.get("deviation_bps_1m")
                .and_then(|x| x.as_u64())
                .unwrap_or_else(|| env::panic_str("missing deviation_bps_1m")),
        )
        .unwrap_or_else(|_| env::panic_str("bad deviation_bps_1m")),
        observed_at_ms: jstr(v, "observed_at_ms"),
        seq: jstr(v, "seq"),
        signature: jbytes(v, "signature"),
    }
}
fn log_event(event: &str, actor: &str, resource: &str, outcome: &str) {
    env::log_str(
        &serde_json::json!({
            "event": event, "actor": actor, "resource": resource, "outcome": outcome,
            "block_timestamp_ms": (env::block_timestamp() / 1_000_000).to_string()
        })
        .to_string(),
    );
}

impl ZeroTickContract {
    fn canonical(&self, market_id: &str, a: &BundleArgs) -> Vec<u8> {
        format!(
            "zerotick.oracle.v2|{}|{}|{}|{}|{}|{}",
            env::current_account_id(),
            market_id,
            a.spot,
            a.deviation_bps_1m,
            a.observed_at_ms,
            a.seq
        )
        .into_bytes()
    }

    fn verify_bundle(&self, market_id: &str, a: BundleArgs) -> OracleBundle {
        let m = self
            .vault
            .markets
            .get(market_id)
            .unwrap_or_else(|| env::panic_str("unknown market"));
        let pubkey = self
            .oracle_pubkeys
            .get(market_id)
            .unwrap_or_else(|| env::panic_str("oracle not registered"));
        let pk: [u8; 32] = pubkey
            .as_slice()
            .try_into()
            .unwrap_or_else(|_| env::panic_str("bad pubkey len"));
        let sig: [u8; 64] = a
            .signature
            .as_slice()
            .try_into()
            .unwrap_or_else(|_| env::panic_str("bad signature len"));
        let spot = p_spot(&a.spot);
        let observed_at_ms = p_i64(&a.observed_at_ms, "observed_at_ms");
        let seq = p_u64(&a.seq, "seq");
        let valid = env::ed25519_verify(&sig, self.canonical(market_id, &a), &pk);
        OracleBundle {
            market: MarketId::new(market_id),
            spot,
            deviation_bps_1m: a.deviation_bps_1m,
            observed_at_unix_secs: observed_at_ms.div_euclid(1000),
            observed_at_ms,
            seq,
            signer: m.oracle_signer.clone(),
            signature_valid: valid,
        }
    }

    fn now_ms() -> i64 {
        (env::block_timestamp() / 1_000_000) as i64
    }
    fn signer_z() -> ZAccountId {
        ZAccountId::new(env::signer_account_id().to_string())
    }
    fn require_owner(&self) {
        if env::predecessor_account_id() != self.owner {
            env::panic_str("owner only");
        }
    }
}

#[near]
impl ZeroTickContract {
    #[init]
    pub fn new(owner_id: String, usdc_token_id: String) -> Self {
        let owner: NearAccountId = owner_id
            .parse()
            .unwrap_or_else(|_| env::panic_str("bad owner_id"));
        let usdc_token: NearAccountId = usdc_token_id
            .parse()
            .unwrap_or_else(|_| env::panic_str("bad usdc_token_id"));
        Self {
            owner,
            usdc_token,
            vault: SharedVault::new(),
            oracle_pubkeys: HashMap::new(),
        }
    }

    pub fn get_config(&self) -> String {
        serde_json::json!({
            "owner": self.owner.to_string(), "usdc_token": self.usdc_token.to_string(),
            "contract": env::current_account_id().to_string()
        })
        .to_string()
    }

    // ---------- custody in: NEP-141 receiver ----------
    /// Called by the USDC token contract via `ft_transfer_call`. The
    /// transferred `amount` is the only credited value. Any panic here
    /// reverts the token transfer, so failed actions never keep funds.
    pub fn ft_on_transfer(
        &mut self,
        sender_id: NearAccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        if env::predecessor_account_id() != self.usdc_token {
            env::panic_str("ft_on_transfer: only the configured USDC token may call this");
        }
        let amt = p_amount(amount);
        if amt == 0 {
            env::panic_str("zero transfer");
        }
        let who = ZAccountId::new(sender_id.to_string());
        let v: serde_json::Value =
            serde_json::from_str(&msg).unwrap_or_else(|_| env::panic_str("msg must be JSON"));
        match jstr(&v, "action").as_str() {
            "deposit_lp" => {
                let shares = self
                    .vault
                    .deposit_lp(&who, amt)
                    .unwrap_or_else(|e| env::panic_str(&format!("deposit_lp rejected: {e:?}")));
                log_event(
                    "vault.deposit_lp",
                    &who.0,
                    "vault:shared",
                    &format!("shares={shares}"),
                );
            }
            "create_market" => {
                let market_id = jstr(&v, "market_id");
                let oracle_pubkey = jbytes(&v, "oracle_pubkey");
                if oracle_pubkey.len() != 32 {
                    env::panic_str("oracle pubkey must be 32 bytes");
                }
                let base = p_u64(&jstr(&v, "base_usdc_minor"), "base");
                let inc_raw = jstr(&v, "inception");
                let inc = p_spot(&inc_raw);
                let maturity = p_i64(&jstr(&v, "maturity_unix_secs"), "maturity");
                let formula = match jstr(&v, "kind").as_str() {
                    "SyntheticSpotV1" => Formula::synthetic_spot_v1(base, inc),
                    "BinaryV1" => Formula::binary_v1(base, maturity),
                    "IndexCappedV1" => Formula::index_capped_v1(base, inc, maturity),
                    "DirectV1" => {
                        let mut f = Formula::synthetic_spot_v1(base, inc);
                        f.kind = FormulaKind::DirectV1;
                        f
                    }
                    other => env::panic_str(&format!("unknown kind: {other}")),
                };
                // Expiring markets must mature in the future.
                if !matches!(formula.kind, FormulaKind::SyntheticSpotV1)
                    && maturity <= Self::now_ms() / 1000
                {
                    env::panic_str("maturity must be in the future");
                }
                if self.oracle_pubkeys.contains_key(&market_id) {
                    env::panic_str("market exists");
                }
                self.vault
                    .create_market(
                        MarketId::new(market_id.clone()),
                        jstr(&v, "name"),
                        jstr(&v, "oracle_signer"),
                        formula,
                        amt,
                    )
                    .unwrap_or_else(|e| env::panic_str(&format!("create market: {e}")));
                self.oracle_pubkeys.insert(market_id.clone(), oracle_pubkey);
                log_event(
                    "market.create",
                    &who.0,
                    &format!("market:{market_id}"),
                    "created allocation=0",
                );
            }
            "buy" => {
                let market_id = jstr(&v, "market_id");
                let b = self.verify_bundle(&market_id, bundle_from_json(&v));
                let max_p = p_u64(&jstr(&v, "max_price_usdc_minor"), "max price");
                let fill = self
                    .vault
                    .buy(&who, &b, amt, max_p, Self::now_ms())
                    .unwrap_or_else(|e| env::panic_str(&format!("buy rejected: {e:?}")));
                log_event(
                    "trade.buy",
                    &who.0,
                    &format!("market:{market_id}"),
                    &format!("units={}", fill.units_minor),
                );
            }
            other => env::panic_str(&format!("unknown action: {other}")),
        }
        PromiseOrValue::Value(U128(0)) // everything used; nothing refunded
    }

    // ---------- custody out: claim ----------
    /// Transfer the caller's full claimable balance in real USDC. Uses a
    /// callback to restore the balance if the token transfer fails.
    pub fn claim(&mut self) -> Promise {
        let who = Self::signer_z();
        let amount = self.vault.claim(&who);
        if amount == 0 {
            env::panic_str("nothing claimable");
        }
        let receiver: NearAccountId = who
            .0
            .parse()
            .unwrap_or_else(|_| env::panic_str("bad receiver"));
        log_event(
            "vault.claim",
            &who.0,
            "vault:shared",
            &format!("amount={amount}"),
        );
        ext_ft::ext(self.usdc_token.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(GAS_FT_TRANSFER)
            .ft_transfer(
                receiver,
                U128(amount as u128),
                Some("ZeroTick claim".into()),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CLAIM_CALLBACK)
                    .on_claim_result(who.0.clone(), amount.to_string()),
            )
    }

    #[private]
    pub fn on_claim_result(&mut self, account: String, amount_usdc_minor: String) -> bool {
        let ok = env::promise_result_checked(0, 1024).is_ok();
        if !ok {
            let amount: u64 = amount_usdc_minor.parse().unwrap_or(0);
            let _ = self
                .vault
                .credit_claimable(&ZAccountId::new(account.clone()), amount);
            log_event(
                "vault.claim",
                &account,
                "vault:shared",
                "transfer_failed_restored",
            );
        }
        ok
    }

    /// Redeem LP shares; payout is credited as claimable (call `claim()`).
    pub fn withdraw_lp(&mut self, shares: String) -> String {
        let who = Self::signer_z();
        let payout = self
            .vault
            .withdraw_lp(&who, p_u64(&shares, "shares"), Self::now_ms())
            .unwrap_or_else(|e| env::panic_str(&format!("withdraw rejected: {e:?}")));
        self.vault
            .credit_claimable(&who, payout)
            .unwrap_or_else(|e| env::panic_str(&format!("credit claimable: {e:?}")));
        log_event(
            "vault.withdraw_lp",
            &who.0,
            "vault:shared",
            &format!("payout={payout}"),
        );
        serde_json::json!({"payout_usdc_minor": payout.to_string(), "claimable": self.vault.claimable(&who).to_string()}).to_string()
    }

    pub fn lp_shares_of(&self, account: String) -> String {
        self.vault
            .lp_shares
            .get(&account)
            .copied()
            .unwrap_or(0)
            .to_string()
    }

    // ---------- views ----------
    pub fn vault_state(&self) -> String {
        serde_json::json!({
            "collateral": self.vault.collateral_usdc_minor.to_string(),
            "reserved_max": self.vault.total_reserved().to_string(),
            "free": self.vault.free_collateral().to_string(),
            "coverage": format!("{}", self.vault.coverage()),
            "markets": self.vault.markets.len(),
            "usdc_token": self.usdc_token.to_string()
        })
        .to_string()
    }

    pub fn position_of(&self, account: String, market_id: String) -> String {
        self.vault
            .position(&ZAccountId::new(account), &MarketId::new(market_id))
            .to_string()
    }

    pub fn claimable_of(&self, account: String) -> String {
        self.vault.claimable(&ZAccountId::new(account)).to_string()
    }

    pub fn get_formula(&self, market_id: String) -> String {
        let m = self
            .vault
            .markets
            .get(&market_id)
            .unwrap_or_else(|| env::panic_str("unknown market"));
        let f = &m.formula;
        let kind = match f.kind {
            FormulaKind::IndexCappedV1 => "IndexCappedV1",
            FormulaKind::DirectV1 => "DirectV1",
            FormulaKind::BinaryV1 => "BinaryV1",
            FormulaKind::SyntheticSpotV1 => "SyntheticSpotV1",
        };
        serde_json::json!({
            "market": m.id.0, "name": m.name, "kind": kind,
            "base_usdc_minor": f.base_usdc_minor.to_string(), "inception": f.inception,
            "cap_mult_x1000": f.cap_mult_x1000, "base_spread_bps": f.base_spread_bps,
            "skew_bps": f.skew_bps, "staleness_bps_per_sec": f.staleness_bps_per_sec,
            "tenor_spread_bps_per_day": f.tenor_spread_bps_per_day, "vol_divisor": f.vol_divisor,
            "max_age_secs": f.max_age_secs, "max_deviation_bps": f.max_deviation_bps,
            "max_trade_usdc_minor": f.max_trade_usdc_minor.to_string(),
            "max_market_reserved_usdc_minor": f.max_market_reserved_usdc_minor.to_string(),
            "maturity_unix_secs": f.maturity_unix_secs, "oracle_signer": m.oracle_signer,
            "units_out": m.units_out_minor.to_string(), "halted": m.halted,
            "allocation_bps": m.effective_allocation_bps()
        })
        .to_string()
    }

    // ---------- trading (exits credit claimable) ----------
    pub fn quote(
        &self,
        market_id: String,
        spot: String,
        deviation_bps_1m: u16,
        observed_at_ms: String,
        seq: String,
        signature: Vec<u8>,
    ) -> String {
        let b = self.verify_bundle(
            &market_id,
            BundleArgs {
                spot,
                deviation_bps_1m,
                observed_at_ms,
                seq,
                signature,
            },
        );
        let q = self
            .vault
            .quote(&b, Self::now_ms())
            .unwrap_or_else(|e| env::panic_str(&format!("quote rejected: {e:?}")));
        serde_json::json!({
            "fair": q.fair_usdc_minor.to_string(), "ask": q.ask_usdc_minor.to_string(), "bid": q.bid_usdc_minor.to_string(),
            "half_spread_bps": q.half_spread_bps, "age_secs": q.age_secs,
            "staleness_bps": q.staleness_bps, "vol_bps": q.vol_bps, "skew_bps": q.skew_bps
        }).to_string()
    }

    /// Sell units back at bid. Payout is capped per lot at its reserved
    /// maximum and credited as claimable (call `claim()` for the transfer).
    pub fn sell(
        &mut self,
        market_id: String,
        units_minor: String,
        min_price_usdc_minor: String,
        spot: String,
        deviation_bps_1m: u16,
        observed_at_ms: String,
        seq: String,
        signature: Vec<u8>,
    ) -> String {
        let b = self.verify_bundle(
            &market_id,
            BundleArgs {
                spot,
                deviation_bps_1m,
                observed_at_ms,
                seq,
                signature,
            },
        );
        let who = Self::signer_z();
        let fill = self
            .vault
            .sell(
                &who,
                &b,
                p_u64(&units_minor, "units"),
                p_u64(&min_price_usdc_minor, "min price"),
                Self::now_ms(),
            )
            .unwrap_or_else(|e| env::panic_str(&format!("sell rejected: {e:?}")));
        self.vault
            .credit_claimable(&who, fill.usdc_minor)
            .unwrap_or_else(|e| env::panic_str(&format!("credit claimable: {e:?}")));
        log_event(
            "trade.sell",
            &who.0,
            &format!("market:{market_id}"),
            &format!("units={} payout={}", fill.units_minor, fill.usdc_minor),
        );
        serde_json::json!({"units": fill.units_minor.to_string(), "usdc_credited": fill.usdc_minor.to_string(), "price": fill.quoted_price_usdc_minor.to_string()}).to_string()
    }

    pub fn request_redemption(&mut self, market_id: String) {
        let who = Self::signer_z();
        self.vault
            .request_redemption(&who, &MarketId::new(market_id), Self::now_ms())
            .unwrap_or_else(|e| env::panic_str(&format!("request redemption rejected: {e:?}")));
    }

    pub fn redemption_requested(&self, account: String, market_id: String) -> String {
        self.vault
            .redemption_requested(&ZAccountId::new(account), &MarketId::new(market_id))
            .to_string()
    }

    /// Paged market close-out (50 lots/call). Returns `more: true` when
    /// another call is needed. Payouts land in claimable balances.
    pub fn settle_market(
        &mut self,
        market_id: String,
        spot: String,
        deviation_bps_1m: u16,
        observed_at_ms: String,
        seq: String,
        signature: Vec<u8>,
    ) -> String {
        let b = self.verify_bundle(
            &market_id,
            BundleArgs {
                spot,
                deviation_bps_1m,
                observed_at_ms,
                seq,
                signature,
            },
        );
        let (total, lots, more) = self
            .vault
            .settle_market_paged(&b, Self::now_ms(), SETTLE_PAGE)
            .unwrap_or_else(|e| env::panic_str(&format!("settle market rejected: {e:?}")));
        log_event(
            "market.settle",
            env::signer_account_id().as_ref(),
            &format!("market:{market_id}"),
            &format!("lots={lots} total={total} more={more}"),
        );
        serde_json::json!({"total_payout": total.to_string(), "lots_settled": lots, "more": more})
            .to_string()
    }

    /// Settle the caller's expired lots; payout credited as claimable.
    pub fn redeem(
        &mut self,
        market_id: String,
        spot: String,
        deviation_bps_1m: u16,
        observed_at_ms: String,
        seq: String,
        signature: Vec<u8>,
    ) -> String {
        let b = self.verify_bundle(
            &market_id,
            BundleArgs {
                spot,
                deviation_bps_1m,
                observed_at_ms,
                seq,
                signature,
            },
        );
        let who = Self::signer_z();
        let payout = self
            .vault
            .settle_expired(&who, &b, Self::now_ms())
            .unwrap_or_else(|e| env::panic_str(&format!("redeem rejected: {e:?}")));
        self.vault
            .credit_claimable(&who, payout)
            .unwrap_or_else(|e| env::panic_str(&format!("credit claimable: {e:?}")));
        payout.to_string()
    }

    // ---------- owner administration ----------
    pub fn halt(&mut self, market_id: String) {
        self.require_owner();
        self.vault.halt(&MarketId::new(market_id.clone()));
        log_event(
            "market.halt",
            self.owner.as_ref(),
            &format!("market:{market_id}"),
            "halted",
        );
    }

    pub fn unhalt(&mut self, market_id: String) {
        self.require_owner();
        self.vault.unhalt(&MarketId::new(market_id.clone()));
        log_event(
            "market.unhalt",
            self.owner.as_ref(),
            &format!("market:{market_id}"),
            "unhalted",
        );
    }

    /// Enable risk for a market: allocation in bps of vault NAV (0 disables buys).
    pub fn set_market_allocation(&mut self, market_id: String, allocation_bps: u16) {
        self.require_owner();
        if allocation_bps > 10_000 {
            env::panic_str("allocation_bps > 10000");
        }
        self.vault
            .set_market_allocation(&MarketId::new(market_id.clone()), Some(allocation_bps))
            .unwrap_or_else(|e| env::panic_str(&format!("set allocation: {e:?}")));
        log_event(
            "market.allocation",
            self.owner.as_ref(),
            &format!("market:{market_id}"),
            &format!("bps={allocation_bps}"),
        );
    }

    /// Rotate the oracle signer + pubkey for a market (sequence resets).
    pub fn rotate_oracle(
        &mut self,
        market_id: String,
        oracle_signer: String,
        oracle_pubkey: Vec<u8>,
    ) {
        self.require_owner();
        if oracle_pubkey.len() != 32 {
            env::panic_str("oracle pubkey must be 32 bytes");
        }
        self.vault
            .set_oracle_signer(&MarketId::new(market_id.clone()), oracle_signer)
            .unwrap_or_else(|e| env::panic_str(&format!("rotate oracle: {e:?}")));
        self.oracle_pubkeys.insert(market_id.clone(), oracle_pubkey);
        log_event(
            "market.rotate_oracle",
            self.owner.as_ref(),
            &format!("market:{market_id}"),
            "rotated",
        );
    }

    pub fn transfer_ownership(&mut self, new_owner: String) {
        self.require_owner();
        self.owner = new_owner
            .parse()
            .unwrap_or_else(|_| env::panic_str("bad new_owner"));
        log_event(
            "contract.transfer_ownership",
            self.owner.as_ref(),
            "contract:self",
            "transferred",
        );
    }
}
