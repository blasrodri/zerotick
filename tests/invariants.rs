//! Invariant, conservation, permission, and PnL tests for the ZeroTick core.
//! These exercise only the public API and must hold after every operation:
//!   1. collateral >= total reserved (coverage >= 1)
//!   2. cash conservation: collateral == deposits + fees + buys - sells
//!      - settlements - LP withdrawals (tracked externally)
//!   3. per-market units_out == sum of open lot units == sum of positions
//!   4. a sell never pays more than the consumed lots' reserved maximum
//!   5. only owners/operators can administer; only holders can settle theirs

use zerotick::oracle::OracleAdapter;
use zerotick::protocol::*;
use zerotick::types::{AccountId, ExecError, MarketId, VaultId};

const NOW: i64 = 1_700_000_000_000;

fn acct(s: &str) -> AccountId {
    AccountId::new(s)
}

fn bundle(id: &MarketId, spot: f64, seq: u64, at_ms: i64) -> OracleBundle {
    OracleBundle {
        market: id.clone(),
        spot,
        deviation_bps_1m: 40,
        observed_at_unix_secs: at_ms / 1000,
        observed_at_ms: at_ms,
        seq,
        signer: "oracle.near".into(),
        signature_valid: true,
    }
}

fn spot_vault() -> (SharedVault, MarketId) {
    let mut v = SharedVault::new();
    v.deposit_lp(&acct("lp"), 100_000_000_000).unwrap();
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

fn assert_invariants(v: &SharedVault, expected_collateral: u64) {
    assert_eq!(
        v.collateral_usdc_minor, expected_collateral,
        "cash conservation broken"
    );
    assert!(
        v.collateral_usdc_minor >= v.total_reserved(),
        "coverage < 1: {} < {}",
        v.collateral_usdc_minor,
        v.total_reserved()
    );
    if v.total_reserved() > 0 {
        assert!(v.coverage() >= 1.0);
    }
    for (mid, m) in &v.markets {
        let lots: u64 = v
            .position_lots
            .values()
            .filter(|l| l.status == PositionStatus::Open && l.market.0 == *mid)
            .map(|l| l.units_minor)
            .sum();
        assert_eq!(lots, m.units_out_minor, "open lots != units_out for {mid}");
        let held: u64 = v
            .positions
            .iter()
            .filter(|((_, mk), _)| mk == mid)
            .map(|(_, u)| *u)
            .sum();
        assert_eq!(held, m.units_out_minor, "positions != units_out for {mid}");
    }
}

/// Deterministic LCG so the sequence is reproducible without extra deps.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
}

#[test]
fn randomized_ops_preserve_invariants() {
    let (mut v, id) = spot_vault();
    let id2 = MarketId::new("IDX");
    v.create_market(
        id2.clone(),
        "Index".into(),
        "oracle.near".into(),
        Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000),
        MARKET_CREATION_FEE_USDC_MINOR,
    )
    .unwrap();
    v.set_market_allocation(&id2, Some(10_000)).unwrap();

    // External cash ledger: $100k LP + 2x$10 fees.
    let mut cash: u64 = 100_000_000_000 + 2 * MARKET_CREATION_FEE_USDC_MINOR;
    let traders = [acct("alice"), acct("bob"), acct("carol")];
    let mut rng = Lcg(0x5eed1234);
    let mut seq = 0u64;
    let mut now = NOW;
    let (mut n_buys, mut n_sells, mut n_lp) = (0u32, 0u32, 0u32);
    for step in 0..600 {
        seq += 1;
        now += 1_000;
        let who = traders[(rng.next() % 3) as usize].clone();
        let (mid, spot) = if rng.next().is_multiple_of(2) {
            (id.clone(), 700.0 + rng.range(0, 2800) as f64) // 0.5x..2.5x of inception, beyond cap moves included via sells
        } else {
            (id2.clone(), 400.0 + rng.range(0, 2200) as f64)
        };
        let b = bundle(&mid, spot, seq, now - 200);
        match rng.next() % 4 {
            0 | 1 => {
                // buy $10..$2000
                let amt = rng.range(10, 2000) * 1_000_000;
                if let Ok(fill) = v.buy(&who, &b, amt, u64::MAX, now) {
                    cash += fill.usdc_minor;
                    n_buys += 1;
                }
            }
            2 => {
                // sell up to held
                let held = v.position(&who, &mid);
                if held > 0 {
                    let units = rng.range(1, held + 1);
                    if let Ok(fill) = v.sell(&who, &b, units, 0, now) {
                        cash -= fill.usdc_minor;
                        n_sells += 1;
                    }
                }
            }
            _ => {
                // LP deposit / withdraw dust
                if rng.next().is_multiple_of(2) {
                    let amt = rng.range(1, 500) * 1_000_000;
                    if v.deposit_lp(&acct("lp2"), amt).is_ok() {
                        cash += amt;
                        n_lp += 1;
                    }
                } else if let Ok(paid) = v.withdraw_lp(&acct("lp2"), rng.range(1, 1_000_000), now) {
                    cash -= paid;
                    n_lp += 1;
                }
            }
        }
        assert_invariants(&v, cash);
        let _ = step;
    }
    // Non-vacuous: the sequence must actually execute many trades.
    assert!(
        n_buys > 80 && n_sells > 30 && n_lp > 10,
        "vacuous run: buys={n_buys} sells={n_sells} lp={n_lp}"
    );
}

#[test]
fn pnl_roundtrip_at_same_price_loses_only_spread() {
    let (mut v, id) = spot_vault();
    let alice = acct("alice");
    let spent = 250_000_000; // $250
    let fill = v
        .buy(
            &alice,
            &bundle(&id, 1400.0, 1, NOW - 200),
            spent,
            u64::MAX,
            NOW,
        )
        .unwrap();
    // Mark at entry: units * fair($10.00 at par) must be within spread of spend.
    let mark = fill.units_minor as u128 * 10_000_000 / 1_000_000;
    assert!(
        mark <= spent as u128 && mark > (spent as f64 * 0.98) as u128,
        "mark {mark} vs spent {spent}"
    );
    // Sell everything at par 1s later: realize only the bid/ask spread loss (<2%).
    let sell = v
        .sell(
            &alice,
            &bundle(&id, 1400.0, 2, NOW + 800),
            fill.units_minor,
            0,
            NOW + 1000,
        )
        .unwrap();
    assert!(
        sell.usdc_minor < spent,
        "roundtrip at same price must not profit"
    );
    assert!(
        sell.usdc_minor as f64 > spent as f64 * 0.98,
        "spread loss too big: {} vs {}",
        sell.usdc_minor,
        spent
    );
    assert_eq!(v.position(&alice, &id), 0);
}

#[test]
fn pnl_up_move_pays_and_down_move_loses() {
    let (mut v, id) = spot_vault();
    let alice = acct("alice");
    let bob = acct("bob");
    let spent = 250_000_000;
    let fa = v
        .buy(
            &alice,
            &bundle(&id, 1400.0, 1, NOW - 200),
            spent,
            u64::MAX,
            NOW,
        )
        .unwrap();
    let fb = v
        .buy(
            &bob,
            &bundle(&id, 1400.0, 2, NOW - 200),
            spent,
            u64::MAX,
            NOW,
        )
        .unwrap();
    // +30%: 1400 -> 1820. Payout ~ +30% minus spread.
    let win = v
        .sell(
            &alice,
            &bundle(&id, 1820.0, 3, NOW + 800),
            fa.units_minor,
            0,
            NOW + 1000,
        )
        .unwrap();
    let gain = win.usdc_minor as f64 / spent as f64 - 1.0;
    assert!(
        gain > 0.25 && gain <= 0.30,
        "+30% move paid {gain:.3} (must be <= +30%, spread only below)"
    );
    // -30%: 1400 -> 980.
    let lose = v
        .sell(
            &bob,
            &bundle(&id, 980.0, 4, NOW + 800),
            fb.units_minor,
            0,
            NOW + 1000,
        )
        .unwrap();
    let loss = lose.usdc_minor as f64 / spent as f64 - 1.0;
    assert!((-0.35..-0.25).contains(&loss), "-30% move paid {loss:.3}");
}

#[test]
fn partial_sell_cap_is_pro_rata() {
    let (mut v, id) = spot_vault();
    let alice = acct("alice");
    let fill = v
        .buy(
            &alice,
            &bundle(&id, 1400.0, 1, NOW - 200),
            5_000_000_000,
            u64::MAX,
            NOW,
        )
        .unwrap();
    // 20x print: each half pays at most half the lot max (2x base/unit).
    let lot_max = fill.units_minor as u128 * 20_000_000 / 1_000_000;
    let half = fill.units_minor / 2;
    let s1 = v
        .sell(&alice, &bundle(&id, 28000.0, 2, NOW - 100), half, 0, NOW)
        .unwrap();
    assert!(
        (s1.usdc_minor as u128) <= lot_max / 2 + 2,
        "half paid {} > half cap {}",
        s1.usdc_minor,
        lot_max / 2
    );
    let rest = v.position(&alice, &id);
    let s2 = v
        .sell(&alice, &bundle(&id, 28000.0, 3, NOW - 100), rest, 0, NOW)
        .unwrap();
    assert!(
        (s1.usdc_minor as u128 + s2.usdc_minor as u128) <= lot_max + 2,
        "split sells beat the cap"
    );
}

#[test]
fn lp_roundtrip_no_free_money() {
    let (mut v, id) = spot_vault();
    // Trader roundtrip pays spread into the pool; LP2 joins after, leaves later.
    let alice = acct("alice");
    let f = v
        .buy(
            &alice,
            &bundle(&id, 1400.0, 1, NOW - 200),
            1_000_000_000,
            u64::MAX,
            NOW,
        )
        .unwrap();
    v.sell(
        &alice,
        &bundle(&id, 1400.0, 2, NOW + 800),
        f.units_minor,
        0,
        NOW + 1000,
    )
    .unwrap();
    let shares = v.deposit_lp(&acct("lp2"), 10_000_000_000).unwrap();
    let paid = v.withdraw_lp(&acct("lp2"), shares, NOW + 2000).unwrap();
    // LP2 deposited after the spread was earned: roundtrip returns ~principal, never more.
    assert!(paid <= 10_000_000_000, "late LP extracted {paid} > deposit");
    assert!(
        paid as f64 > 10_000_000_000f64 * 0.999,
        "late LP haircut without losses: {paid}"
    );
    // Withdrawing more shares than held fails; double-withdraw fails.
    assert!(v.withdraw_lp(&acct("lp2"), 1, NOW + 3000).is_err());
    assert!(v.withdraw_lp(&acct("nobody"), 1, NOW + 3000).is_err());
}

#[test]
fn protocol_permissions() {
    let owner = acct("gov");
    let mut p = ZeroTickProtocol::new(owner.clone());
    let adapter = || OracleAdapter::new("o1", "desc", 5, true);
    assert_eq!(
        p.register_oracle(&acct("mallory"), adapter()),
        Err(ExecError::Unauthorized)
    );
    p.register_oracle(&owner, adapter()).unwrap();

    let id = MarketId::new("M1");
    p.create_market(
        MarketDefinition {
            id: id.clone(),
            name: "M1".into(),
            oracle_id: "o1".into(),
            oracle_signer: "oracle.near".into(),
            formula_template: Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000),
            risk_tier: MarketRiskTier::Tier1,
            halted: false,
        },
        MARKET_CREATION_FEE_USDC_MINOR,
    )
    .unwrap();
    // Tier assignment is governance-only and new markets start Unrated.
    assert_eq!(p.markets[&id.0].risk_tier, MarketRiskTier::Unrated);
    assert_eq!(
        p.assign_market_tier(&acct("mallory"), &id, MarketRiskTier::Tier1),
        Err(ExecError::Unauthorized)
    );

    let vid = VaultId::new("v1");
    p.create_vault(&acct("op"), vid.clone(), VaultConfig::default(), NOW)
        .unwrap();
    p.deposit_lp(&vid, &acct("lp"), 100_000_000_000).unwrap();
    // Only the vault operator may enable markets / set allocations.
    assert_eq!(
        p.enable_market(&acct("mallory"), &vid, &id, None, Some(10_000)),
        Err(ExecError::Unauthorized)
    );
    p.enable_market(&acct("op"), &vid, &id, None, None).unwrap();
    assert_eq!(
        p.set_market_allocation(&acct("mallory"), &vid, &id, Some(10_000)),
        Err(ExecError::Unauthorized)
    );
    // Unrated => buys route nowhere even with a valid bundle.
    let routed = p.buy_routed(
        &acct("alice"),
        &bundle(&id, 1000.0, 1, NOW - 100),
        100_000_000,
        u64::MAX,
        None,
        NOW,
    );
    assert!(routed.fills.is_empty() && routed.unfilled_usdc_minor == 100_000_000);
}

#[test]
fn claimable_conservation_across_settlement() {
    let mut v = SharedVault::new();
    v.deposit_lp(&acct("lp"), 50_000_000_000).unwrap();
    let maturity = NOW / 1000 + 90;
    let id = MarketId::new("BIN");
    v.create_market(
        id.clone(),
        "Bin".into(),
        "oracle.near".into(),
        Formula::binary_v1(10_000_000, maturity),
        MARKET_CREATION_FEE_USDC_MINOR,
    )
    .unwrap();
    v.set_market_allocation(&id, Some(10_000)).unwrap();
    let expiry_ms = maturity * 1000;
    let mut cash = 50_000_000_000 + MARKET_CREATION_FEE_USDC_MINOR;
    for (who, seq) in [("a", 1u64), ("b", 2)] {
        let f = v
            .buy(
                &acct(who),
                &bundle(&id, 0.62, seq, NOW - 100),
                100_000_000,
                u64::MAX,
                NOW,
            )
            .unwrap();
        cash += f.usdc_minor;
    }
    let fix = bundle(&id, 1.0, 3, expiry_ms);
    let (total, lots) = v.settle_market(&fix, expiry_ms).unwrap();
    assert_eq!(lots, 2);
    cash -= total;
    assert_eq!(v.collateral_usdc_minor, cash);
    let claimed = v.claim(&acct("a")) + v.claim(&acct("b"));
    assert_eq!(claimed, total, "claimable must equal settled total exactly");
    assert_eq!(v.claim(&acct("a")), 0, "double claim");
    // Restoring a failed transfer (contract callback path) re-credits exactly.
    v.credit_claimable(&acct("a"), 500).unwrap();
    assert_eq!(v.claim(&acct("a")), 500);
}

#[test]
fn perpetual_index_lot_never_expires_but_settles_at_market_maturity() {
    let mut v = SharedVault::new();
    v.deposit_lp(&acct("lp"), 100_000_000_000).unwrap();
    let maturity = NOW / 1000 + 90;
    let id = MarketId::new("IDX90");
    v.create_market(
        id.clone(),
        "Index".into(),
        "oracle.near".into(),
        Formula::index_capped_v1(10_000_000, 1000.0, maturity),
        MARKET_CREATION_FEE_USDC_MINOR,
    )
    .unwrap();
    v.set_market_allocation(&id, Some(10_000)).unwrap();
    let alice = acct("alice");
    // Default buy = perpetual lot bounded only by market maturity.
    let fill = v
        .buy(
            &alice,
            &bundle(&id, 1000.0, 1, NOW - 100),
            100_000_000,
            u64::MAX,
            NOW,
        )
        .unwrap();
    assert_eq!(fill.expires_at_ms, Some(maturity * 1000));
    // Cannot settle before maturity; settles for everyone at maturity fixing.
    let early = bundle(&id, 1500.0, 2, NOW);
    assert!(v.settle_expired(&alice, &early, NOW).is_err());
    let fix = bundle(&id, 1500.0, 3, maturity * 1000);
    let (total, lots) = v.settle_market(&fix, maturity * 1000).unwrap();
    assert_eq!(lots, 1);
    assert!(
        total > 100_000_000,
        "+50% index pays out at maturity: {total}"
    );
    assert_eq!(v.position(&alice, &id), 0);
}

#[test]
fn fx_lot_is_perpetual_and_sellable_far_future() {
    let (mut v, id) = spot_vault();
    let alice = acct("alice");
    let fill = v
        .buy(
            &alice,
            &bundle(&id, 1400.0, 1, NOW - 100),
            100_000_000,
            u64::MAX,
            NOW,
        )
        .unwrap();
    assert_eq!(fill.expires_at_ms, Some(i64::MAX));
    let y2100: i64 = 4_102_444_800_000;
    let sell = v
        .sell(
            &alice,
            &bundle(&id, 1400.0, 2, y2100 - 100),
            fill.units_minor,
            0,
            y2100,
        )
        .unwrap();
    assert!(sell.usdc_minor > 0);
}

#[test]
fn pool_wide_liquidity_skew_protects_empty_market() {
    // Fresh vault: quote market B (empty) -> baseline ask.
    let (mut v, a) = spot_vault();
    let b_id = MarketId::new("USDBRL");
    v.create_market(
        b_id.clone(),
        "USD/BRL".into(),
        "oracle.near".into(),
        Formula::synthetic_spot_v1(10_000_000, 5.2),
        MARKET_CREATION_FEE_USDC_MINOR,
    )
    .unwrap();
    v.set_market_allocation(&b_id, Some(10_000)).unwrap();
    let bb = bundle(&b_id, 5.2, 1, NOW - 100);
    let baseline = v.quote(&bb, NOW).unwrap();
    assert_eq!(
        baseline.skew_bps, 0.0,
        "healthy pool: pure formula, no liquidity charge"
    );
    // Fill market A until a large share of the pool is reserved.
    let whale = acct("whale");
    let mut seq = 10u64;
    loop {
        seq += 1;
        match v.buy(
            &whale,
            &bundle(&a, 1400.0, seq, NOW - 100),
            5_000_000_000,
            u64::MAX,
            NOW,
        ) {
            Ok(_) => {
                if v.total_reserved() as f64 / v.collateral_usdc_minor as f64 > 0.35 {
                    break;
                }
            }
            Err(_) => break,
        }
        assert!(seq < 200, "setup loop runaway");
    }
    // Market B is still empty, but its price now carries a liquidity charge.
    let stressed = v.quote(&bb, NOW).unwrap();
    assert!(
        stressed.skew_bps > 0.0,
        "pool-wide skew must engage: {}",
        stressed.skew_bps
    );
    assert!(
        stressed.ask_usdc_minor > baseline.ask_usdc_minor,
        "buys get dearer as pool fills"
    );
    assert!(
        stressed.bid_usdc_minor >= baseline.bid_usdc_minor,
        "sells get better (pool pays up to de-risk)"
    );
    assert!(v.coverage() >= 1.0);
}
