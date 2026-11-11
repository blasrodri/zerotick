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

## Layout

| Path | What |
|---|---|
| `src/protocol.rs` | Canonical Rust core: shared vault, formulas, pricing, settlement (53 unit tests workspace-wide) |
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
cargo test            # protocol + regression tests
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
