//! ZeroTick v2 demo — shared vault, one formula per market, internal FX positions.
//! Run: `cargo run -p zerotick`

use zerotick::protocol::{Formula, OracleBundle, SharedVault};
use zerotick::types::{AccountId, MarketId};

const NOW_MS: i64 = 1_700_000_000_000;

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

fn main() {
    let mut v = SharedVault::new();
    v.deposit_lp(&AccountId::new("lp"), 100_000_000_000)
        .unwrap(); // one shared $100k vault

    // One formula template, many markets.
    let f = Formula::index_capped_v1(10_000_000, 1000.0, 1_800_000_000);
    for (id, name) in [
        ("ARG30-DEC26", "Merval 30"),
        ("BOV-DEC26", "Bovespa"),
        ("CU-DEC26", "Copper"),
    ] {
        let mid = MarketId::new(id);
        v.create_market(
            mid.clone(),
            name.into(),
            "oracle.near".into(),
            f.clone(),
            zerotick::protocol::MARKET_CREATION_FEE_USDC_MINOR,
        )
        .unwrap();
        v.set_market_allocation(&mid, Some(10_000)).unwrap(); // operator enables risk; creation alone starts at 0%
    }
    println!("Shared vault $100,000 backs 3 synthetic markets (per-market reserve caps).");

    // Time is a pricing input: same spot, older data = worse price.
    let id = MarketId::new("ARG30-DEC26");
    for (label, age) in [("200ms fresh", 200), ("1s old", 1000), ("4s old", 4000)] {
        let q = v.quote(&bundle(&id, 1000.0, age, 1), NOW_MS).unwrap();
        println!(
            "  oracle {:>11}: fair ${:.2}  ask ${:.4}  bid ${:.4}  half-spread {:.0}bps (base {} + stale {:.0} + vol {:.0})",
            label,
            q.fair_usdc_minor as f64 / 1e6,
            q.ask_usdc_minor as f64 / 1e6,
            q.bid_usdc_minor as f64 / 1e6,
            q.half_spread_bps,
            q.base_spread_bps,
            q.staleness_bps,
            q.vol_bps
        );
    }

    // Alice buys into ARG30 — she receives an internal position (no token leaves the program).
    let alice = AccountId::new("alice");
    let fill = v
        .buy(
            &alice,
            &bundle(&id, 1000.0, 300, 7),
            1_000_000_000,
            u64::MAX,
            NOW_MS,
        )
        .unwrap();
    println!(
        "\nAlice pays $1,000 → position of {:.2} units ARG30 @ ${:.4} (internal ledger, not a token).",
        fill.units_minor as f64 / 1e6,
        fill.quoted_price_usdc_minor as f64 / 1e6
    );
    println!(
        "Shared vault: collateral ${:.0}  reserved (max) ${:.0}  free ${:.0}  coverage {:.2}x",
        v.collateral_usdc_minor as f64 / 1e6,
        v.total_reserved() as f64 / 1e6,
        v.free_collateral() as f64 / 1e6,
        v.coverage()
    );

    println!("\nCash payout per unit at expiry (base $10, cap 2x):");
    for (label, it) in [
        ("-30%", 700.0),
        ("0%", 1000.0),
        ("+15%", 1150.0),
        ("+100%", 2000.0),
        ("+150%", 2500.0),
    ] {
        println!("  {label:>5}  payout ${:.2}", f.payoff(it) as f64 / 1e6);
    }
}
