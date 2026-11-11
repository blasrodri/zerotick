# ZeroTick Protocol — design (as implemented)

This document describes what the code in this repo actually does. Canonical
implementation: `src/protocol.rs` (`SharedVault`), wrapped by
`contracts/zerotick/` on NEAR. `src/pricing.rs`, `src/market.rs`, and
`src/functions.rs` are earlier prototypes kept for reference; they are not
deployed and new code must not build on them. `ZeroTickProtocol` (multi-vault
registry + router in `protocol.rs`) is library code exercised by unit tests;
the deployed NEAR contract is a **single** `SharedVault`.

**Positions are internal ledger entries, not tokens.** Nothing is minted to a
wallet and positions cannot leave the vault. Buy opens a lot, sell closes
lots FIFO, expiry cash-settles in USDC. There is no leverage, no shorting,
and no liquidation.

## Formula kinds (one per market, chosen at creation)

| Kind | Fair | Expiry / payout |
|---|---|---|
| `SyntheticSpotV1` | `base × spot/inception` | Never expires. Exit = sell. Cash payout capped at the lot's reserved max (2× stress reserve) |
| `IndexCappedV1` | `base × min(2, spot/inception)` | Fixed tenor (default 7d); payoff = capped fair at fixing |
| `DirectV1` | `spot` (already USDC minor) | Like IndexCapped for reserve purposes |
| `BinaryV1` | `payout × p`, `p ∈ [0,1]` | Settles at maturity; fixing must be exactly `1` (pay `base`) or `0` (pay 0) |

Formulas are validated at creation (`Formula::validate`): positive base,
finite positive inception, positive caps/limits, and a real future maturity
for expiring kinds. Unknown kinds are rejected, never defaulted.

## Custody (NEP-141)

- **In:** only via `ft_transfer_call` on the configured USDC token. The
  transferred amount is the credited amount; `msg` selects
  `deposit_lp` / `create_market` (amount = fee, ≥ $10) / `buy`
  (amount = spend). A panic in `ft_on_transfer` reverts the transfer.
- **Out:** sell / redeem / settle / LP-withdraw credit an internal
  `claimable` balance; `claim()` performs the real `ft_transfer` and a
  callback restores the balance if the transfer fails.
- The contract account must hold enough NEAR for the 1-yocto attachments.
  With real USDC, receivers may need `storage_deposit` registration first.

## Market creation & risk enablement

Creation is permissionless (anyone can pay the $10 fee and register an
oracle pubkey) **but starts at 0% allocation** (`Unrated`): no buys execute
until the contract owner calls `set_market_allocation(market, bps)`. The
owner can also `halt`/`unhalt`, `rotate_oracle` (new signer + pubkey, seq
resets), and `transfer_ownership`.

## Oracle bundle

`{ market, spot, deviation_bps_1m, observed_at_ms, seq, signer, signature }`,
signed over canonical bytes
`zerotick.oracle.v2|{contract}|{market}|{spot}|{deviation}|{observed_at_ms}|{seq}`.

- Canonical encoding: `spot` must equal Rust `Display` of the parsed f64;
  integers are plain decimal. Non-finite spots are rejected.
- Trading checks, in order: not halted → signature valid → signer matches →
  finite spot (`> 0`; binary `≤ 1`) → sequence → age in `[-1s, max_age]` →
  deviation ≤ breaker → not matured.
- **Sequence:** monotonic per market. Reusing the *identical* bundle is
  allowed until superseded (many traders share one oracle round); a
  *different* bundle with an already-seen `seq` is `ReplayNonce`.
- **Buy-side jump breaker:** a buy whose fair jumps >3× versus the last
  accepted fair is rejected as a broken print. Sells/settlements are exempt
  so exits are never trapped.
- `deviation_bps_1m` is asserted by the same signer it protects against —
  a known single-signer trust limit, bounded by the breaker, allocation
  caps, and reserve-capped payouts. Aggregation/quorum is future work.

## Pricing — time is an input

```
half_spread_bps = base_spread + staleness_bps_per_sec × age_secs
                + deviation_bps_1m / vol_divisor + tenor_charge
ask = fair × (1 + half_spread + skew×x)   bid = fair × (1 − half_spread + skew×x)
x   = market liability at fair / per-market reserve cap   (0..1)
```

Best price requires data ≲1s old; hard reject past `max_age_secs` (10s).

## Solvency

- Each buy reserves its lot's explicit maximum payout; a buy executes only
  if post-trade `collateral ≥ total reserved`, under the per-market reserve
  cap, the market's NAV allocation %, and the 500% gross-reference cap.
- **Sell payout per lot = min(units × bid, pro-rata reserved max)** — the
  same cap as settlement. This is what keeps `coverage = collateral /
  reserved ≥ 1` after arbitrary spot moves; uncapped `fair` is display-only
  beyond the reserve multiple.
- LP shares price off NAV (collateral − MTM liabilities). Deposits are
  rejected while NAV ≤ 0 with shares outstanding. Withdrawals pay from free
  collateral, respect epochs, and block while expired lots are unsettled.
- Minimum trade $1; settlement is paged (50 lots/call on-chain).
- All money arithmetic is checked; release builds keep `overflow-checks`
  on, so a wrap traps instead of corrupting balances.

## Settlement

- Fixing bundle: valid signature/signer, finite spot, `seq` not older than
  last, `observed_at ∈ [expiry, expiry + 24h]`, not from the future.
  Binary fixings must be exactly `0` or `1` — a trading probability can
  never settle a market.
- `settle_market` closes every expired lot in pages, credits claimable per
  holder, clears redemption requests, and halts the market on the last page.
  `request_redemption` lets holders pre-register; `redeem` settles one
  holder directly. Synthetic spot never settles.

## Why zero quote txs

Prices are never posted on-chain. Anyone can recompute a quote off-chain
from the same bundle + formula + inventory; the contract re-derives and
enforces it at execution. Idle markets cost nothing. (Note: pricing is f64
today, so off-chain recomputation must match Rust/Wasm float rounding — a
fixed-point wire format is the known hardening step.)

## Oracle operations

`oracle-service/` signs bundles for operator-supplied spots (licensed feeds;
`frontend/fetch-odds.mjs` shows Polymarket/Kalshi probability sourcing for
binary markets). Deviation is computed from a rolling 60s window of served
spots (see the service header for methodology and custody requirements).
Resolution fixings must come from the official outcome, never the last
traded price.

## Explicit non-goals / open items

Multi-signer quorum, fixed-point pricing, transferable position tokens,
per-market collateral segregation, LP hedging, data licensing, and any
mainnet deployment. No audit has been performed.
