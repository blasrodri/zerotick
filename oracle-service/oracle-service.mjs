#!/usr/bin/env node
// ZeroTick oracle signer service.
//
// Signs OracleBundles in the contract's canonical format:
//   zerotick.oracle.v2|{contract}|{market}|{spot}|{deviation_bps_1m}|{observed_at_ms}|{seq}
//
// Endpoints:
//   GET /health  -> { ok, contract, signer }
//   GET /pubkey  -> { signer, pubkey: number[32] }   (register at create_market)
//   GET /bundle?market=USDARS&spot=1450.5 -> signed bundle JSON for one trade
//   GET /bundle?market=FEDCUT26&spot=1&fixing=1 -> settlement fixing (spot 0/1)
//
// The OPERATOR supplies `spot` from their licensed feed (FX reference rates,
// index levels, Polymarket/Kalshi mids via frontend/fetch-odds.mjs, official
// results for fixings). This service never invents a price.
//
// deviation_bps_1m methodology: rolling 60-second window of spots served for
// the market; deviation = stdev of per-observation returns, in bps, floored
// at 5 and capped at 10000. With fewer than 5 samples it reports 40 bps and
// `deviation_provisional: true`. It is computed from served history only —
// an operator MUST validate it against their raw feed before production use.
//
// Key custody: the ed25519 secret comes ONLY from ORACLE_SECRET_KEY
// (ed25519:... as printed by NEAR tooling) or ORACLE_KEY_FILE (JSON
// {"secret": "ed25519:..."}, mode 0600, outside the repo). Never commit a
// key. In production use a secrets manager / KMS, one key per market source
// where feasible, rotate with the contract's `rotate_oracle`, and restrict
// this service's network exposure (ORACLE_API_TOKEN is a minimum, not a
// substitute for network controls).
//
// Trust note: a single signer is a single point of trust. The contract
// bounds damage (allocation caps, reserve-capped payouts, deviation breaker,
// buy-side jump breaker) but cannot make a bad feed honest. Multi-source
// aggregation / quorum signing remains future work — see PROTOCOL.md.

import http from 'node:http';
import fs from 'node:fs';
import { KeyPair } from 'near-api-js';

const PORT = Number(process.env.ORACLE_PORT || 8787);
const CONTRACT_ID = process.env.CONTRACT_ID;
const SIGNER = process.env.ORACLE_SIGNER || 'oracle.near';
const TOKEN = process.env.ORACLE_API_TOKEN || '';
if (!CONTRACT_ID) { console.error('CONTRACT_ID env required'); process.exit(1); }

function loadKey() {
  if (process.env.ORACLE_SECRET_KEY) return KeyPair.fromString(process.env.ORACLE_SECRET_KEY);
  if (process.env.ORACLE_KEY_FILE) {
    const st = fs.statSync(process.env.ORACLE_KEY_FILE);
    if ((st.mode & 0o077) !== 0) console.warn('[oracle] WARNING: key file permissions are broader than 0600');
    return KeyPair.fromString(JSON.parse(fs.readFileSync(process.env.ORACLE_KEY_FILE, 'utf8')).secret);
  }
  console.error('ORACLE_SECRET_KEY or ORACLE_KEY_FILE env required');
  process.exit(1);
}
const key = loadKey();
const pubkey = Array.from(key.getPublicKey().data);

const lastSeq = new Map();      // market -> last seq issued
const history = new Map();      // market -> [{t, spot}] within 60s

function deviationBps(market, spot, now) {
  const h = (history.get(market) || []).filter(x => now - x.t <= 60_000);
  h.push({ t: now, spot });
  history.set(market, h.slice(-600));
  if (h.length < 5) return { dev: 40, provisional: true };
  const rets = [];
  for (let i = 1; i < h.length; i++) {
    if (h[i - 1].spot > 0) rets.push((h[i].spot - h[i - 1].spot) / h[i - 1].spot);
  }
  if (rets.length < 4) return { dev: 40, provisional: true };
  const mean = rets.reduce((a, b) => a + b, 0) / rets.length;
  const var_ = rets.reduce((a, b) => a + (b - mean) ** 2, 0) / rets.length;
  const dev = Math.round(Math.min(10_000, Math.max(5, Math.sqrt(var_) * 10_000)));
  return { dev, provisional: false };
}

function canonicalSpot(raw) {
  const n = Number(raw);
  if (!Number.isFinite(n)) return null;
  return { n, s: String(n) }; // String(Number) matches Rust f64 Display for ordinary decimals
}

const server = http.createServer((req, res) => {
  const json = (code, obj) => { res.writeHead(code, { 'content-type': 'application/json' }); res.end(JSON.stringify(obj)); };
  try {
    const url = new URL(req.url, 'http://oracle.local');
    if (TOKEN && req.headers['x-oracle-token'] !== TOKEN && url.pathname !== '/health') {
      return json(401, { error: 'unauthorized' });
    }
    if (url.pathname === '/health') return json(200, { ok: true, contract: CONTRACT_ID, signer: SIGNER });
    if (url.pathname === '/pubkey') return json(200, { signer: SIGNER, pubkey });
    if (url.pathname === '/bundle') {
      const market = url.searchParams.get('market') || '';
      if (!/^[A-Za-z0-9._-]{1,64}$/.test(market)) return json(400, { error: 'bad market' });
      const fixing = url.searchParams.get('fixing') === '1';
      const cs = canonicalSpot(url.searchParams.get('spot'));
      if (!cs || (fixing ? (cs.n !== 0 && cs.n !== 1 && cs.n < 0) : cs.n <= 0)) {
        return json(400, { error: fixing ? 'fixing spot must be >= 0 (binary: exactly 0 or 1)' : 'spot must be finite and > 0' });
      }
      if (fixing && cs.n !== 0 && cs.n !== 1) {
        // Non-binary fixings may be any non-negative level; binary markets
        // are enforced {0,1} on-chain, so other values would revert there.
      }
      const now = Date.now();
      const { dev, provisional } = fixing ? { dev: 0, provisional: false } : deviationBps(market, cs.n, now);
      const seq = Math.max(now, (lastSeq.get(market) || 0) + 1);
      lastSeq.set(market, seq);
      const msg = `zerotick.oracle.v2|${CONTRACT_ID}|${market}|${cs.s}|${dev}|${now}|${seq}`;
      const { signature } = key.sign(Buffer.from(msg));
      return json(200, {
        market, spot: cs.s, deviation_bps_1m: dev, deviation_provisional: provisional,
        observed_at_ms: String(now), seq: String(seq), signature: Array.from(signature),
        signer: SIGNER, contract: CONTRACT_ID,
      });
    }
    return json(404, { error: 'not found' });
  } catch (e) {
    return json(500, { error: String(e?.message || e) });
  }
});
server.listen(PORT, '127.0.0.1', () => console.log(`[oracle] signer=${SIGNER} contract=${CONTRACT_ID} listening on 127.0.0.1:${PORT}`));
