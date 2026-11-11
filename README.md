# ZeroTick

**ZeroTick turns any trusted price into a tradable market.**

A permissionless synthetic market factory on NEAR: one shared USDC vault backs
many synthetic markets (emerging-market FX, local indexes, binary events) with
no tokenized underlying and no per-pair liquidity pool. A market needs a
registered oracle signer, an audited payoff formula, and vault collateral
reserved against its maximum payout. Prices widen as oracle data ages or
deviates, and circuit breakers reject stale, wild, or jumping prints.

> **Status: research prototype.** The mechanism, custody flow, and negative
> paths are proven on a local NEAR sandbox against a test-only mock USDC.
> It has **not** been audited and must not hold mainnet funds. See
> [PROTOCOL.md](PROTOCOL.md) for the exact rules and [VC_SUMMARY.md](VC_SUMMARY.md)
> for the executive summary.

**Live demo (paper mode, no wallet or funds needed): https://blasrodri.github.io/zerotick/**

Built for NEAR HACKATHON — tracks: `defi` (shared-vault synthetic trading,
liquidity-protected pricing) and `realworld` (emerging-market FX/index access
that has no clean on-chain venue). NEAR-specific pieces: `env::ed25519_verify`
oracle bundles re-derived at execution (zero quote transactions), NEP-141
`ft_transfer_call` funding, `claim()` payouts with failure-restore callbacks,
and near-sdk wasm contracts.

## Try it in 2 minutes (judge walkthrough)

Hosted demo — do this, see that:

1. Open https://blasrodri.github.io/zerotick/ → see 62 markets (FX, indexes,
   stocks, events), USD/ARS chart, paper banner. Header reads
   `CASH $10,000 · POSITIONS $0.00 · POOL $0.00`.
2. Click **▲ BUY USD/ARS** (default $250) → Cash becomes $9,750, a position
   row appears under the chart (`spent $250 @ 1,4xx · now …`), and its
   PnL in the header moves every ~3 seconds.
3. Click that position row → Sell is pre-selected; press **MAX**, then
   **▼ Sell** → position closes, Cash increases by the payout
   ($250 ± spread/price move), trade appears under "Recent trades".
4. Open the **Earn** tab → **Deposit $500** → "Yours is worth $500",
   pool cash rises by $500; **Withdraw $500** → both revert. This pool is
   the counterparty that paid your trade in step 3.
5. Pick a Sports/Events market (e.g. FEDCUT) → see the YES/NO resolution
   card; your positions panel stays put regardless of market type.

Full on-chain proof (real token custody, not paper) — run locally:

```bash
./run-local.sh
```

Do that → see the sandbox deploy print `✓` for LP deposit via
`ft_transfer_call`, refunded underpaid/forged trades, 0%-allocation gating,
buy/sell/claim with a separate trader account, binary 0.62-fixing rejection,
settlement + claim, non-owner admin rejections, ending with
`DONE on-chain … failures=0`. The same flow runs in CI (`.github/workflows/ci.yml`,
`sandbox` job).

Verify everything else:

```bash
cargo test        # 63 tests: core, 10 review regressions, invariants/permissions/PnL
cargo run         # worked pricing example (staleness widens the spread)
```

## Layout

| Path | What |
|---|---|
| `src/protocol.rs` | Canonical Rust core: shared vault, formulas, pricing, settlement (63 tests incl. `tests/invariants.rs`) |
| `src/{pricing,market,functions}.rs` | Earlier prototypes, kept for reference only — do not build on them |
| `contracts/zerotick/` | NEAR contract: ed25519 bundle verification + real NEP-141 custody (`ft_transfer_call` in, `claim()` out) |
| `contracts/mock-usdc/` | **Test-only** mintable NEP-141 token for sandbox proofs. Never a real asset |
| `frontend/` | React + Vite terminal: paper trading by default, live trading when an oracle service + wallet are configured |
| `oracle-service/` | Operator-run ed25519 bundle signer (`/bundle`, `/pubkey`, `/health`) |
| `run-local.sh` | Full local stack: sandbox + deploy + on-chain custody proof + UI |
| `deploy-and-run.sh` | Fail-fast testnet deploy (requires funded account + USDC token id) |

## Quick start (full local stack)

Prerequisites: Docker, Rust via rustup (pinned by `rust-toolchain.toml`), Node 20 (`.nvmrc`).

```bash
./run-local.sh        # NEAR sandbox (Docker) + deploy + custody proof + serve UI
# then open http://127.0.0.1:5173
```

The browser wallet selector cannot talk to the local sandbox network, so the
local UI runs in clearly-labelled **paper mode** with real contract state in
the "On-chain" panel (plain RPC views). The custody proof
(`frontend/deploy-local-v2.mjs`) exercises real token transfers with a
separate trader account: LP deposit, market creation, allocation gating,
oracle rejections with refunds, buy/sell, binary settlement, LP withdrawal,
and `claim()` payouts.

## Core demo & tests (no Docker)

```bash
cargo test            # 63 tests: core + regressions + invariants/permissions/PnL
cargo run             # worked pricing/vault example
cargo build --target wasm32-unknown-unknown --release -p zerotick-contract -p mock-usdc
```

## Live mode (testnet)

```bash
ACCOUNT_ID=you.testnet CONTRACT_ID=zerotick.you.testnet USDC_ID=usdc.you.testnet ./deploy-and-run.sh
# run the signer (keys only via env / 0600 key file — never commit them):
cd oracle-service && npm ci
CONTRACT_ID=zerotick.you.testnet ORACLE_KEY_FILE=/path/to/key.json npm start
# point the frontend at it: VITE_ORACLE_BUNDLE_URL=http://127.0.0.1:8787
```

Markets start at **0% vault allocation**; the contract owner enables risk per
market with `set_market_allocation`. Funds enter only via `ft_transfer_call`
on USDC and leave only via `claim()` — see the custody model at the top of
`contracts/zerotick/src/lib.rs`.

## Known limits (before any mainnet value)

- No independent audit; f64 pricing (fixed-point redesign is future work).
- Single oracle signer per market (no quorum/aggregation yet); deviation is
  self-reported by that signer and only bounded by breakers and caps.
- Sell payouts are capped at each lot's reserved maximum (2× stress reserve)
  — that cap is what keeps `coverage >= 1` true after any spot move.
- Data licensing, market/legal classification, and LP adverse-selection
  economics are unresolved product questions, not code questions.

License: MIT — see [LICENSE](LICENSE).
