# Security exceptions (time-bound, risk-accepted)

## EX-2026-01: frontend transitive `high` advisories in NEAR/WalletConnect tree

- **Observed:** 2026-11-11, `npm audit` in `frontend/`: 25 total
  (1 low, 3 moderate, 21 high, **0 critical**) after `npm audit fix`.
  The 21 highs are transitive through `@near-wallet-selector/*@8`,
  `near-api-js@4`, `@walletconnect/*`, and `vite@5`.
- **Why not fixed now:** clearing them requires major upgrades
  (wallet-selector 8→10, near-api-js 4→7, vite 5→8) that change wallet and
  signing APIs used by both the dApp and the sandbox custody proof. Doing
  that migration without a testnet wallet test would trade known transitive
  advisories for unknown signing breakage.
- **Risk accepted:** browser-side only; the app never handles private keys
  (signing stays inside the user's wallet), paper mode is the default, and
  no mainnet deployment is claimed. Sandbox deploy scripts run locally
  against a throwaway chain with throwaway keys.
- **Gate in the meantime:** CI fails on `critical` (`npm audit
  --audit-level=critical`, currently 0) and prints the full audit.
- **Exit condition / expiry:** upgrade wallet-selector + near-api-js +
  vite majors and re-run the sandbox proof and a testnet wallet signature;
  **expires 2027-02-01** — after that date CI must return to
  `--audit-level=high` or this exception must be re-approved in writing.

## Note: RustSec `wee_alloc` unmaintained (warning only)

`cargo audit` (2026-11-11): **0 vulnerabilities**; 1 warning —
`wee_alloc 0.4.5` unmaintained (RUSTSEC-2022-0054), pulled in by near-sdk.
Tracked upstream; revisit on the next near-sdk major bump.
