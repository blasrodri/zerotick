// Deploy ZeroTick (shared vault + NEP-141 custody + signed oracle bundles)
// to the local NEAR sandbox and prove the full flow on-chain, including
// real token movements through the TEST-ONLY mock USDC contract.
// Prereq: docker run -d -p 3030:3030 --name zerotick-near nearprotocol/sandbox:2.13.4
// Run from frontend/:  node deploy-local-v2.mjs
import { connect, keyStores, KeyPair, utils } from 'near-api-js';
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.NEAR_RPC || 'http://127.0.0.1:3030';
const NETWORK = 'sandbox';
const ROOT_ID = 'sandbox';
const ROOT_KEY = 'ed25519:3JoAjwLppjgvxkk6kNsu5wQj3FfUJnpBKWieC73hVTpBeA6FZiCc5tfyZL3a3tHeQJegQe4qGSv8FLsYp7TYd1r6'; // public sandbox validator key, local only
const CONTRACT_ID = process.env.CONTRACT_ID || 'zerotick2.sandbox';
const USDC_ID = process.env.USDC_ID || 'usdc2.sandbox';
const TRADER_ID = process.env.TRADER_ID || 'trader2.sandbox'; // distinct from the contract account: claims must move tokens between different accounts to be observable
const WASM = path.resolve('../target/wasm32-unknown-unknown/release/zerotick_contract.wasm');
const MOCK_WASM = path.resolve('../target/wasm32-unknown-unknown/release/mock_usdc.wasm');
const GAS = '200000000000000';
const sleep = ms => new Promise(r => setTimeout(r, ms));
let failures = 0;
const check = (ok, label) => { console.log(ok ? ' ✓' : ' ✗ FAIL:', label); if (!ok) { failures++; process.exitCode = 1; } };

function loadOrCreate(file) {
  if (fs.existsSync(file)) return KeyPair.fromString(JSON.parse(fs.readFileSync(file, 'utf8')).secret);
  const kp = KeyPair.fromRandom('ed25519');
  fs.writeFileSync(file, JSON.stringify({ secret: kp.toString() }, null, 2), { mode: 0o600 });
  return kp;
}
const contractKey = loadOrCreate('/tmp/zerotick2-key.json');
const oracleKey = loadOrCreate('/tmp/zerotick-oracle-key.json');
const usdcKey = loadOrCreate('/tmp/zerotick-usdc-key.json');
const traderKey = loadOrCreate('/tmp/zerotick-trader-key.json');

const keyStore = new keyStores.InMemoryKeyStore();
await keyStore.setKey(NETWORK, ROOT_ID, KeyPair.fromString(ROOT_KEY));
await keyStore.setKey(NETWORK, CONTRACT_ID, contractKey);
await keyStore.setKey(NETWORK, USDC_ID, usdcKey);
await keyStore.setKey(NETWORK, TRADER_ID, traderKey);
const near = await connect({ networkId: NETWORK, nodeUrl: RPC, keyStore });
const root = await near.account(ROOT_ID);
const contract = await near.account(CONTRACT_ID);
const usdc = await near.account(USDC_ID);
const trader = await near.account(TRADER_ID);

for (const [acct, key, id] of [[contract, contractKey, CONTRACT_ID], [usdc, usdcKey, USDC_ID], [trader, traderKey, TRADER_ID]]) {
  if (!await acct.state().then(() => true).catch(() => false)) {
    console.log('creating', id);
    await root.createAccount(id, key.getPublicKey(), utils.format.parseNearAmount('20'));
  }
}

console.log('deploying mock USDC (TEST ONLY)', fs.statSync(MOCK_WASM).size, 'bytes →', USDC_ID);
await usdc.deployContract(fs.readFileSync(MOCK_WASM));
await usdc.functionCall({ contractId: USDC_ID, methodName: 'new', args: {}, gas: GAS }).catch(() => {});
console.log('deploying ZeroTick', fs.statSync(WASM).size, 'bytes →', CONTRACT_ID);
await contract.deployContract(fs.readFileSync(WASM));

const call = (method, args, deposit = '0') => contract.functionCall({ contractId: CONTRACT_ID, methodName: method, args, gas: GAS, attachedDeposit: deposit });
const view = (method, args, id = CONTRACT_ID) => contract.viewFunction({ contractId: id, methodName: method, args });
const balance = async (who = CONTRACT_ID) => BigInt(await view('ft_balance_of', { account_id: who }, USDC_ID));
const ftCall = (amount, msgObj) => contract.functionCall({
  contractId: USDC_ID, methodName: 'ft_transfer_call',
  args: { receiver_id: CONTRACT_ID, amount: String(amount), memo: null, msg: JSON.stringify(msgObj) },
  gas: GAS, attachedDeposit: '1',
});
const ftCallTrader = (amount, msgObj) => trader.functionCall({
  contractId: USDC_ID, methodName: 'ft_transfer_call',
  args: { receiver_id: CONTRACT_ID, amount: String(amount), memo: null, msg: JSON.stringify(msgObj) },
  gas: GAS, attachedDeposit: '1',
});
const tcall = (method, args) => trader.functionCall({ contractId: CONTRACT_ID, methodName: method, args, gas: GAS, attachedDeposit: '0' });

// canonical signed payload — must byte-match the contract (spot via String()).
let seq = 1000;
function bundle(market, spot, dev = 40, ageMs = 200) {
  seq += 1;
  const observed = Date.now() - ageMs;
  const msg = `zerotick.oracle.v2|${CONTRACT_ID}|${market}|${spot}|${dev}|${observed}|${seq}`;
  const { signature } = oracleKey.sign(Buffer.from(msg));
  return { spot: String(spot), deviation_bps_1m: dev, observed_at_ms: String(observed), seq: String(seq), signature: Array.from(signature) };
}

await call('new', { owner_id: CONTRACT_ID, usdc_token_id: USDC_ID }).then(() => console.log(' ✓ init (owner + USDC token configured)')).catch(() => console.log(' · already initialized'));
await contract.functionCall({ contractId: USDC_ID, methodName: 'mint', args: { account_id: CONTRACT_ID, amount: '200000000000' }, gas: GAS });
await contract.functionCall({ contractId: USDC_ID, methodName: 'mint', args: { account_id: TRADER_ID, amount: '50000000000' }, gas: GAS });
console.log(' ✓ minted mock USDC to LP actor + trader; trader balance =', (await balance(TRADER_ID)).toString());

// Direct ft_on_transfer (not via the token) must be rejected.
await call('ft_on_transfer', { sender_id: CONTRACT_ID, amount: '1000000', msg: '{"action":"deposit_lp"}' })
  .then(() => check(false, 'direct ft_on_transfer rejected'))
  .catch(() => check(true, 'direct ft_on_transfer rejected (only token may call)'));

await ftCall(100_000_000_000n, { action: 'deposit_lp' });
console.log(' ✓ LP deposit $100k via ft_transfer_call · vault:', await view('vault_state', {}));

const pubkey = Array.from(oracleKey.getPublicKey().data);
const createMsg = (id, kind, extra = {}) => ({
  action: 'create_market', market_id: id, name: id, oracle_signer: 'oracle.near',
  oracle_pubkey: pubkey, kind, base_usdc_minor: '10000000', inception: '1400', maturity_unix_secs: '0', ...extra,
});

// Underpaid creation ($5 < $10): receiver panics, token refunds, no market.
const balBefore = await balance();
await ftCall(5_000_000n, createMsg('CHEAP', 'SyntheticSpotV1'));
await sleep(1500);
check((await balance()) === balBefore, 'underpaid creation refunded in full');
await view('get_formula', { market_id: 'CHEAP' }).then(() => check(false, 'CHEAP market absent')).catch(() => check(true, 'CHEAP market absent'));

// Unknown kind must be rejected (also refunded).
await ftCall(10_000_000n, createMsg('BOGUS', 'MoonV9'));
await sleep(1500);
check((await balance()) === balBefore, 'unknown-kind creation refunded');

await ftCall(10_000_000n, createMsg('USDARS', 'SyntheticSpotV1', { name: 'USD/ARS' }));
await sleep(1500);
console.log(' ✓ created USDARS via ft_transfer_call; formula:', await view('get_formula', { market_id: 'USDARS' }));

// Buys are disabled until the owner enables allocation (creation starts at 0%).
await ftCall(1_000_000_000n, { action: 'buy', market_id: 'USDARS', max_price_usdc_minor: '999999999', ...bundle('USDARS', 1450) });
await sleep(1500);
check((await view('position_of', { account: CONTRACT_ID, market_id: 'USDARS' })) === '0', 'buy blocked at 0% allocation (refunded)');
await call('set_market_allocation', { market_id: 'USDARS', allocation_bps: 10000 });
console.log(' ✓ owner enabled USDARS allocation (100%)');

console.log('\n-- negative oracle tests (no position, full refund) --');
const negBal = await balance();
const neg = async (label, b) => {
  await ftCall(1_000_000n, { action: 'buy', market_id: 'USDARS', max_price_usdc_minor: '999999999', ...b });
  await sleep(1200);
  const pos = await view('position_of', { account: CONTRACT_ID, market_id: 'USDARS' });
  check(pos === '0' && (await balance()) === negBal, label);
};
const good = bundle('USDARS', 1450);
await neg('bad signature', { ...good, signature: good.signature.map((x, i) => i === 0 ? x ^ 1 : x) });
await neg('stale oracle (30s old)', bundle('USDARS', 1450, 40, 30000));
await neg('deviation circuit breaker (900bps)', bundle('USDARS', 1450, 900, 100));

console.log('\n-- live trades with real token custody --');
console.log('quote fresh  :', await view('quote', { market_id: 'USDARS', ...bundle('USDARS', 1450, 40, 200) }));
console.log('quote 4s old :', await view('quote', { market_id: 'USDARS', ...bundle('USDARS', 1450, 40, 4000) }), '← wider spread');
await ftCallTrader(1_000_000_000n, { action: 'buy', market_id: 'USDARS', max_price_usdc_minor: '999999999', ...bundle('USDARS', 1450, 40, 200) });
await sleep(1500);
const held = Number(await view('position_of', { account: TRADER_ID, market_id: 'USDARS' }));
check(held > 0, `BUY $1,000 USD/ARS credited to trader (units=${held})`);
await tcall('sell', { market_id: 'USDARS', units_minor: String(Math.floor(held / 2)), min_price_usdc_minor: '0', ...bundle('USDARS', 1520, 40, 200) });
const claimable1 = BigInt(await view('claimable_of', { account: TRADER_ID }));
check(claimable1 > 0n, `SELL half credited claimable=${claimable1}`);
const balPreClaim = await balance(TRADER_ID);
await tcall('claim', {});
await sleep(2000);
check((await balance(TRADER_ID)) === balPreClaim + claimable1, 'claim() transferred real mock USDC contract→trader');
check((await view('claimable_of', { account: TRADER_ID })) === '0', 'claimable zero after claim');

const fedMaturity = Math.floor(Date.now() / 1000) + 90;
await ftCall(10_000_000n, createMsg('FEDCUT26', 'BinaryV1', { name: 'Fed cuts rates', inception: '1', maturity_unix_secs: String(fedMaturity) }));
await sleep(1500);
await call('set_market_allocation', { market_id: 'FEDCUT26', allocation_bps: 10000 });
await ftCallTrader(380_000_000n, { action: 'buy', market_id: 'FEDCUT26', max_price_usdc_minor: '999999999', ...bundle('FEDCUT26', 0.62, 40, 200) });
await sleep(1500);
check(Number(await view('position_of', { account: TRADER_ID, market_id: 'FEDCUT26' })) > 0, 'BUY $380 FEDCUT26 binary @62% (trader)');
await tcall('request_redemption', { market_id: 'FEDCUT26' });
console.log('vault:', await view('vault_state', {}));
console.log('waiting for binary maturity…');
await sleep(Math.max(0, (fedMaturity + 2) * 1000 - Date.now()));
// A probability print (0.62) must NOT settle a binary.
await call('settle_market', { market_id: 'FEDCUT26', ...bundle('FEDCUT26', 0.62, 0, 0) })
  .then(() => check(false, 'binary 0.62 fixing rejected'))
  .catch(() => check(true, 'binary 0.62 fixing rejected (needs exactly 0 or 1)'));
const closeout = await call('settle_market', { market_id: 'FEDCUT26', ...bundle('FEDCUT26', 1, 0, 0) });
console.log(' ✓ CLOSE-OUT FEDCUT26 YES:', JSON.stringify(closeout?.status ?? closeout).slice(0, 160));
check((await view('position_of', { account: TRADER_ID, market_id: 'FEDCUT26' })) === '0', 'position closed by settle_market');
const claimable2 = BigInt(await view('claimable_of', { account: TRADER_ID }));
check(claimable2 > 380_000_000n, `settlement credited claimable=${claimable2}`);
const bal2 = await balance(TRADER_ID);
await tcall('claim', {});
await sleep(2000);
check((await balance(TRADER_ID)) === bal2 + claimable2, 'settlement claim transferred real mock USDC contract→trader');

// LP exit: withdraw a slice of shares, then claim the token.
const shares = BigInt(await view('lp_shares_of', { account: CONTRACT_ID }));
check(shares > 0n, `LP shares held=${shares}`);
await call('withdraw_lp', { shares: '1000000000' });
const claimable3 = BigInt(await view('claimable_of', { account: CONTRACT_ID }));
check(claimable3 > 0n, `withdraw_lp credited claimable=${claimable3}`);
await call('claim', {}, '0');
await sleep(2000);
check((await view('claimable_of', { account: CONTRACT_ID })) === '0', 'LP withdrawal claimed');

console.log('\n-- permission negatives (trader is not the owner) --');
const mustFailCall = async (label, fn) => {
  try { await fn(); check(false, label); } catch { check(true, label); }
};
await mustFailCall('non-owner set_market_allocation rejected', () => tcall('set_market_allocation', { market_id: 'USDARS', allocation_bps: 1 }));
await mustFailCall('non-owner halt rejected', () => tcall('halt', { market_id: 'USDARS' }));
await mustFailCall('non-owner rotate_oracle rejected', () => tcall('rotate_oracle', { market_id: 'USDARS', oracle_signer: 'mallory.near', oracle_pubkey: pubkey }));
await mustFailCall('non-owner transfer_ownership rejected', () => tcall('transfer_ownership', { new_owner: TRADER_ID }));
await mustFailCall('withdraw_lp with no shares rejected', () => tcall('withdraw_lp', { shares: '1000000' }));
await mustFailCall('claim with zero claimable rejected', () => tcall('claim', {}));
// Halt actually stops trading (buy refunded), unhalt restores quotes.
await call('halt', { market_id: 'USDARS' });
const heldHalt = await view('position_of', { account: TRADER_ID, market_id: 'USDARS' });
await ftCallTrader(10_000_000n, { action: 'buy', market_id: 'USDARS', max_price_usdc_minor: '999999999', ...bundle('USDARS', 1500, 40, 200) });
await sleep(1500);
check((await view('position_of', { account: TRADER_ID, market_id: 'USDARS' })) === heldHalt, 'halted market refuses buys (refunded)');
await mustFailCall('non-owner unhalt rejected', () => tcall('unhalt', { market_id: 'USDARS' }));
await call('unhalt', { market_id: 'USDARS' });
check((await view('quote', { market_id: 'USDARS', ...bundle('USDARS', 1500, 40, 200) })).includes('fair'), 'unhalt restores quoting');

console.log('vault after:', await view('vault_state', {}));
console.log(`\nDONE on-chain · contract=${CONTRACT_ID} · usdc(mock)=${USDC_ID} · rpc=${RPC} · failures=${failures}`);
if (failures > 0) process.exitCode = 1;
