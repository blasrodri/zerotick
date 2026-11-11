// Fetch live binary-event odds from Polymarket / Kalshi and emit a ZeroTick
// OracleBundle (spot = probability 0..1). These venues ARE the oracle source
// for trading prices: a Polymarket share pays $1, so its price in cents is
// numerically the probability; Kalshi quotes the same contract in cents.
// Usage: node fetch-odds.mjs polymarket "Fed"   |   node fetch-odds.mjs kalshi FED
const [venue, query = ''] = process.argv.slice(2);
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const execFileP = promisify(execFile);
// Node's bundled CA store can miss local issuers on some machines; curl uses
// the OS trust store (still fully TLS-verified, no -k) as a fallback.
async function getJSON(url) {
  try { return await (await fetch(url)).json(); }
  catch (e) { const { stdout } = await execFileP('curl', ['-s', '--max-time', '20', url]); return JSON.parse(stdout); }
}

async function polymarket(q) {
  const url = `https://gamma-api.polymarket.com/public-search?q=${encodeURIComponent(q)}&limit_per_type=5&events_status=active`;
  const j = await getJSON(url);
  const ev = (j.events || [])[0];
  const m = ev?.markets?.[0];
  if (!m) throw new Error('no polymarket match for ' + q);
  const prices = JSON.parse(m.outcomePrices || '[]').map(Number);
  const p = prices[0];
  const spread = Number(m.spread ?? 0);
  return {
    venue: 'Polymarket', event: ev.title, market: m.question, slug: m.slug,
    probability: p, yes_cents: +(p * 100).toFixed(1),
    decimal_odds: p > 0 ? +(1 / p).toFixed(2) : null,
    deviation_bps_1m_proxy: Math.round(Math.abs(spread) * 10000),
  };
}

async function kalshi(q) {
  // q is a series ticker (KXFED, KXNBA, ...) — query it directly; no silent fallback.
  const url = `https://api.elections.kalshi.com/trade-api/v2/markets?limit=20&status=open&series_ticker=${encodeURIComponent(q)}`;
  const j = await getJSON(url);
  const m = (j.markets || [])[0];
  if (!m) throw new Error('no kalshi match for ' + q);
  const bid = Number(m.yes_bid_dollars ?? (m.yes_bid ?? 0) / 100);
  const ask = Number(m.yes_ask_dollars ?? (m.yes_ask ?? 0) / 100);
  const p = bid && ask ? (bid + ask) / 2 : (ask || bid);
  return {
    venue: 'Kalshi', market: m.title, ticker: m.ticker,
    probability: +p.toFixed(4), yes_cents: +(p * 100).toFixed(1),
    decimal_odds: p > 0 ? +(1 / p).toFixed(2) : null,
    deviation_bps_1m_proxy: Math.round(Math.abs(ask - bid) * 10000),
  };
}

const r = venue === 'kalshi' ? await kalshi(query) : await polymarket(query);
const now = Date.now();
// OracleBundle exactly as src/protocol.rs expects (signer/signature added by
// the oracle service that wraps this feed; here we show the unsigned payload).
const bundle = {
  spot: r.probability, // probability 0..1 — BinaryV1 fair = payout * spot
  deviation_bps_1m: r.deviation_bps_1m_proxy,
  observed_at_ms: now, observed_at_unix_secs: Math.floor(now / 1000),
  seq: now, signer: 'oracle.near', signature_valid: '(sign before submit)',
};
console.log(JSON.stringify({ ...r, bundle, note: 'Trading price only — resolution must come from the official outcome (1/0), not the last traded price.' }, null, 2));
