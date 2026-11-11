import { useEffect, useMemo, useRef, useState } from 'react';
import { initWallet, CONFIG, LIVE_POSSIBLE, viewViaRpc, fetchBundle, fetchOraclePubkey, sendFtTransferCall, callMethod } from './lib/near.js';
import { CandleChart, Sparkline, Bar, VaultDonut } from './components/VaultCharts.jsx';

/* seeded PRNG so the 62 listed markets are stable across reloads (paper data) */
function mulberry(seed) {
  return function () {
    seed |= 0; seed = (seed + 0x6D2B79F5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const UNDERLYINGS = [
  // LatAm FX — synthetic exposure that has no clean on-chain venue
  { sym: 'USDARS', pair: 'USD/ARS', name: 'Argentine peso', source: 'BCRA ref', cat: 'FX', inception: 1400 },
  { sym: 'USDBRL', pair: 'USD/BRL', name: 'Brazilian real', source: 'BCB PTAX', cat: 'FX', inception: 5.2 },
  { sym: 'USDMXN', pair: 'USD/MXN', name: 'Mexican peso', source: 'Banxico FIX', cat: 'FX', inception: 17.2 },
  { sym: 'USDCLP', pair: 'USD/CLP', name: 'Chilean peso', source: 'BCCh', cat: 'FX', inception: 950 },
  { sym: 'USDCOP', pair: 'USD/COP', name: 'Colombian peso', source: 'BanRep TRM', cat: 'FX', inception: 4100 },
  { sym: 'USDPEN', pair: 'USD/PEN', name: 'Peruvian sol', source: 'BCRP', cat: 'FX', inception: 3.7 },
  { sym: 'USDUYU', pair: 'USD/UYU', name: 'Uruguayan peso', source: 'BCU', cat: 'FX', inception: 40 },
  // Asia FX
  { sym: 'USDJPY', pair: 'USD/JPY', name: 'Japanese yen', source: 'BOJ ref', cat: 'FX', inception: 155 },
  { sym: 'USDCNY', pair: 'USD/CNY', name: 'Chinese yuan', source: 'PBOC parity', cat: 'FX', inception: 7.2 },
  { sym: 'USDINR', pair: 'USD/INR', name: 'Indian rupee', source: 'RBI ref', cat: 'FX', inception: 83 },
  { sym: 'USDKRW', pair: 'USD/KRW', name: 'Korean won', source: 'BOK ref', cat: 'FX', inception: 1370 },
  { sym: 'USDTWD', pair: 'USD/TWD', name: 'Taiwan dollar', source: 'CBC ref', cat: 'FX', inception: 32 },
  { sym: 'USDTHB', pair: 'USD/THB', name: 'Thai baht', source: 'BOT ref', cat: 'FX', inception: 36 },
  // Europe FX
  { sym: 'USDEUR', pair: 'USD/EUR', name: 'Euro (per USD)', source: 'ECB ref', cat: 'FX', inception: 0.92 },
  { sym: 'USDGBP', pair: 'USD/GBP', name: 'Pound (per USD)', source: 'BoE ref', cat: 'FX', inception: 0.79 },
  { sym: 'USDCHF', pair: 'USD/CHF', name: 'Swiss franc', source: 'SNB ref', cat: 'FX', inception: 0.88 },
  { sym: 'USDPLN', pair: 'USD/PLN', name: 'Polish zloty', source: 'NBP ref', cat: 'FX', inception: 4.0 },
  { sym: 'USDTRY', pair: 'USD/TRY', name: 'Turkish lira', source: 'CBRT ref', cat: 'FX', inception: 34 },
  // EMEA ex-Europe
  { sym: 'USDZAR', pair: 'USD/ZAR', name: 'South African rand', source: 'SARB ref', cat: 'FX', inception: 18 },
  { sym: 'USDNGN', pair: 'USD/NGN', name: 'Nigerian naira', source: 'CBN ref', cat: 'FX', inception: 1550 },
  // Indexes — synthetic spot on benchmarks with no clean on-chain venue
  { sym: 'ARG30', pair: 'ARG30', name: 'Merval 30', source: 'BYMA', cat: 'Indexes', inception: 1000 },
  { sym: 'BOV', pair: 'BOV', name: 'Bovespa', source: 'B3', cat: 'Indexes', inception: 500 },
  { sym: 'NIFTY', pair: 'NIFTY', name: 'Nifty 50', source: 'NSE', cat: 'Indexes', inception: 24000 },
  { sym: 'NIKKEI', pair: 'NIKKEI', name: 'Nikkei 225', source: 'JPX', cat: 'Indexes', inception: 38000 },
  { sym: 'KOSPI', pair: 'KOSPI', name: 'KOSPI', source: 'KRX', cat: 'Indexes', inception: 2600 },
  { sym: 'HSI', pair: 'HSI', name: 'Hang Seng', source: 'HKEX', cat: 'Indexes', inception: 18000 },
  { sym: 'MEXBOL', pair: 'MEXBOL', name: 'IPC Mexico', source: 'BMV', cat: 'Indexes', inception: 55000 },
  { sym: 'STOXX', pair: 'STOXX', name: 'Euro Stoxx 50', source: 'Euronext', cat: 'Indexes', inception: 4800 },
  // Single-name stocks — synthetic spot, 1x
  { sym: 'YPF', pair: 'YPF', name: 'YPF', source: 'BYMA / NYSE ADR', cat: 'Stocks', inception: 25 },
  { sym: 'GGAL', pair: 'GGAL', name: 'Grupo Galicia', source: 'BYMA / Nasdaq ADR', cat: 'Stocks', inception: 50 },
  { sym: 'VALE', pair: 'VALE', name: 'Vale', source: 'B3 / NYSE ADR', cat: 'Stocks', inception: 11 },
  { sym: 'BABA', pair: 'BABA', name: 'Alibaba 9988', source: 'HKEX', cat: 'Stocks', inception: 120 },
  { sym: 'MELI', pair: 'MELI', name: 'MercadoLibre', source: 'Nasdaq', cat: 'Stocks', inception: 2100 },
  { sym: 'PBR', pair: 'PBR', name: 'Petrobras', source: 'B3 / NYSE ADR', cat: 'Stocks', inception: 15 },
  { sym: 'ITUB', pair: 'ITUB', name: 'Itau Unibanco', source: 'B3 / NYSE ADR', cat: 'Stocks', inception: 9 },
  { sym: 'NU', pair: 'NU', name: 'Nu Holdings', source: 'NYSE', cat: 'Stocks', inception: 13 },
  { sym: 'SQM', pair: 'SQM', name: 'SQM Lithium', source: 'NYSE ADR', cat: 'Stocks', inception: 45 },
  { sym: 'TSM', pair: 'TSM', name: 'TSMC', source: 'TWSE / NYSE ADR', cat: 'Stocks', inception: 190 },
  // ETF basket levels (synthetic on the index level, not fund shares)
  { sym: 'EWZ', pair: 'EWZ', name: 'Brazil Large Cap', source: 'MSCI Brazil', cat: 'ETFs', inception: 27 },
  { sym: 'EEM', pair: 'EEM', name: 'EM Basket', source: 'MSCI EM', cat: 'ETFs', inception: 44 },
  { sym: 'ARGT', pair: 'ARGT', name: 'Argentina Basket', source: 'MSCI Argentina', cat: 'ETFs', inception: 55 },
  { sym: 'EWW', pair: 'EWW', name: 'Mexico Basket', source: 'MSCI Mexico', cat: 'ETFs', inception: 62 },
  { sym: 'ECH', pair: 'ECH', name: 'Chile Basket', source: 'MSCI Chile', cat: 'ETFs', inception: 28 },
  { sym: 'EPU', pair: 'EPU', name: 'Peru Basket', source: 'MSCI Peru', cat: 'ETFs', inception: 42 },
  // Commodities — synthetic spot
  { sym: 'COPPER', pair: 'COPPER', name: 'Copper', source: 'COMEX', cat: 'Commodities', inception: 4000 },
  { sym: 'LITHIUM', pair: 'LITHIUM', name: 'Lithium Carbonate', source: 'Fastmarkets', cat: 'Commodities', inception: 90000 },
  { sym: 'WTI', pair: 'WTI', name: 'WTI Crude', source: 'NYMEX', cat: 'Commodities', inception: 80 },
  { sym: 'SOY', pair: 'SOY', name: 'Soybeans', source: 'CBOT', cat: 'Commodities', inception: 1100 },
  { sym: 'GOLD', pair: 'GOLD', name: 'Gold', source: 'COMEX', cat: 'Commodities', inception: 2400 },
  { sym: 'SILVER', pair: 'SILVER', name: 'Silver', source: 'COMEX', cat: 'Commodities', inception: 31 },
  // Sports — binary odds. Oracle spot = win probability (Polymarket/Kalshi mid); resolution 1/0 from official result.
  { sym: 'BOCA', pair: 'BOCA', name: 'Boca wins Libertadores', source: 'Polymarket · CONMEBOL', cat: 'Sports', inception: 1, odds: true, prob: 0.22, maturity: 'LIB26' },
  { sym: 'RIVER', pair: 'RIVER', name: 'River wins Libertadores', source: 'Polymarket · CONMEBOL', cat: 'Sports', inception: 1, odds: true, prob: 0.18, maturity: 'LIB26' },
  { sym: 'ARGWC', pair: 'ARGWC', name: 'Argentina wins World Cup', source: 'Polymarket · FIFA', cat: 'Sports', inception: 1, odds: true, prob: 0.14, maturity: 'WC26' },
  { sym: 'BRAWC', pair: 'BRAWC', name: 'Brazil wins World Cup', source: 'Kalshi · FIFA', cat: 'Sports', inception: 1, odds: true, prob: 0.16, maturity: 'WC26' },
  { sym: 'REALM', pair: 'REALM', name: 'Real Madrid wins UCL', source: 'Kalshi · UEFA', cat: 'Sports', inception: 1, odds: true, prob: 0.19, maturity: 'UCL26' },
  { sym: 'LAKERS', pair: 'LAKERS', name: 'Lakers win NBA title', source: 'Kalshi · NBA', cat: 'Sports', inception: 1, odds: true, prob: 0.09, maturity: 'NBA26' },
  // Prediction events — binary odds on politics / macro / crypto milestones
  { sym: 'FEDCUT', pair: 'FEDCUT', name: 'Fed cuts rates', source: 'Kalshi · FOMC', cat: 'Events', inception: 1, odds: true, prob: 0.62, maturity: 'DEC26' },
  { sym: 'FEDHIKE', pair: 'FEDHIKE', name: 'Fed hikes rates', source: 'Kalshi · FOMC', cat: 'Events', inception: 1, odds: true, prob: 0.08, maturity: 'DEC26' },
  { sym: 'ARGELEC', pair: 'ARGELEC', name: 'Ruling coalition wins AR election', source: 'Polymarket · official count', cat: 'Events', inception: 1, odds: true, prob: 0.48, maturity: 'ELEC27' },
  { sym: 'BRELEC', pair: 'BRELEC', name: 'Lula re-elected in Brazil', source: 'Polymarket · TSE', cat: 'Events', inception: 1, odds: true, prob: 0.44, maturity: 'ELEC26' },
  { sym: 'CPI30', pair: 'CPI30', name: 'Argentina CPI under 30% y/y', source: 'Kalshi · INDEC', cat: 'Events', inception: 1, odds: true, prob: 0.55, maturity: 'DEC26' },
  { sym: 'BTC150', pair: 'BTC150', name: 'Bitcoin above $150k', source: 'Polymarket · exchange print', cat: 'Events', inception: 1, odds: true, prob: 0.31, maturity: 'DEC26' },
];
const CATS = ['All', 'FX', 'Indexes', 'Stocks', 'ETFs', 'Commodities', 'Sports', 'Events'];
const CAT_COLOR = { FX: '#22ff88', Indexes: '#4dc3ff', Stocks: '#2dd4bf', ETFs: '#9be15d', Commodities: '#ff9f43', Sports: '#ffd23d', Events: '#ff7ad9' };

function genCandles(rand, endPrice, n = 48) {
  const out = [];
  let p = endPrice * (0.86 + rand() * 0.1);
  for (let i = 0; i < n; i++) {
    const drift = (endPrice - p) / Math.max(1, n - i) * 0.55;
    const o = p;
    const c = Math.max(0.4, o + drift + (rand() - 0.46) * endPrice * 0.022);
    const h = Math.max(o, c) * (1 + rand() * 0.008);
    const l = Math.min(o, c) * (1 - rand() * 0.008);
    out.push({ o, h, l, c, v: 40 + rand() * 220 });
    p = c;
  }
  out[n - 1].c = endPrice;
  return out;
}

function genMarkets() {
  const rand = mulberry(20261231);
  const out = [];
  for (const u of UNDERLYINGS) {
    {
      const ret = -0.18 + rand() * 0.62; // bullish-skewed tape
      const price = u.odds
        ? +(10 * Math.min(0.94, Math.max(0.04, u.prob + (rand() - 0.5) * 0.08))).toFixed(2)
        : +(10 * Math.min(2, Math.max(0.2, 1 + ret))).toFixed(2);
      const prev = +(price / (1 + (rand() - 0.38) * 0.09)).toFixed(2);
      const minted = 300 + Math.floor(rand() * 4200);
      const collPer = 22 + rand() * 12;
      out.push({
        id: u.sym, sym: u.sym, name: u.name, source: u.source, cat: u.cat, pair: u.pair,
        maturity: u.maturity || '', odds: !!u.odds, base: 10, inception: u.inception,
        price, prev, minted,
        coll: Math.round(minted * collPer),
        cap: 25000, // per-market reserved cap, USDC
        vol24: Math.round(4000 + rand() * 240000),
        dev: Math.round(12 + rand() * 85),
        candles: genCandles(rand, price),
      });
    }
  }
  return out;
}
const INITIAL = genMarkets();
// (pool capital is live paper state below, seeded by POOL_SEED)

/* formula IndexCappedV1 — mirrors src/protocol.rs */
const FORMULA = { baseBps: 50, skewBps: 150, staleBpsPerSec: 25, volDivisor: 2, maxAge: 10, cap: 2 };
function quote(m, spotLevel, ageSecs, devBps, pol, poolUtil = 0) {
  const baseBps = pol?.base_spread_bps ?? FORMULA.baseBps;
  const skewCap = pol?.inventory_skew_bps ?? FORMULA.skewBps;
  const fair = m.base * Math.min(FORMULA.cap, Math.max(0, spotLevel / m.inception));
  const liability = m.minted * fair;
  // Liquidity charge: whichever is tighter — this market or the whole pool.
  const x = Math.min(1, Math.max(liability / (FORMULA.cap * 10000), poolUtil || 0));
  const stale = FORMULA.staleBpsPerSec * Math.max(0, ageSecs);
  const vol = devBps / FORMULA.volDivisor;
  const half = baseBps + stale + vol;
  const skewBps = skewCap * x;
  return {
    fair, x, stale, vol, skewBps, half,
    ask: fair * (1 + half / 1e4 + skewBps / 1e4),
    bid: Math.max(0, fair * (1 - half / 1e4 + skewBps / 1e4)),
  };
}
const fmt$ = (v, d = 2) => '$' + Number(v).toLocaleString(undefined, { minimumFractionDigits: d, maximumFractionDigits: d });
const fmtK = (v) => v >= 1e6 ? '$' + (v / 1e6).toFixed(2) + 'M' : v >= 1000 ? '$' + (v / 1e3).toFixed(0) + 'k' : '$' + Math.round(v).toLocaleString();
const spotOf = (m) => m.inception * (m.price / m.base);
const fmtFx = (v) => v >= 1000 ? v.toLocaleString(undefined, { maximumFractionDigits: 1 }) : v >= 10 ? v.toFixed(2) : v >= 1 ? v.toFixed(4) : v.toFixed(5);

const PAPER_KEY = 'zerotick-paper-v2';
function loadPaper() {
  try {
    const v2 = JSON.parse(localStorage.getItem(PAPER_KEY) || 'null');
    if (v2) return v2;
    const v1 = JSON.parse(localStorage.getItem('zerotick-paper-v1') || 'null') || {};
    return { balance: v1.balance, holdings: v1.holdings, cost: v1.cost };
  } catch { return {}; }
}
const POOL_SEED = 100_000; // other LPs' paper capital; the pool backs YOUR paper trades

export default function App() {
  const [markets, setMarkets] = useState(INITIAL);
  const [selId, setSelId] = useState(INITIAL[0].id);
  const [query, setQuery] = useState('');
  const [cat, setCat] = useState('All');
  const [sort, setSort] = useState('gainers');
  const [focused, setFocused] = useState(false);
  const [highlight, setHighlight] = useState(0);
  const [buy, setBuy] = useState(true);
  const [size, setSize] = useState('250');
  const [balance, setBalance] = useState(() => loadPaper().balance ?? 10000);
  const [holdings, setHoldings] = useState(() => loadPaper().holdings || {});
  const [cost, setCost] = useState(() => loadPaper().cost || {}); // USDC cost basis per market
  const [lp, setLp] = useState(() => loadPaper().lp || { deposited: 0, shares: 0 });
  const [poolCash, setPoolCash] = useState(() => loadPaper().poolCash ?? POOL_SEED);
  const [poolShares, setPoolShares] = useState(() => loadPaper().poolShares ?? POOL_SEED);
  const [poolAmt, setPoolAmt] = useState('500');
  const [tab, setTab] = useState('trade'); // 'trade' | 'earn' — separate screens, separate PnL
  const [poolMode, setPoolMode] = useState('deposit'); // Earn screen action toggle
  const saveWarned = useRef(false);
  const [fills, setFills] = useState([]);
  const [toast, setToast] = useState(null);
  const [account, setAccount] = useState('');
  const live = LIVE_POSSIBLE && Boolean(account);
  const [selector, setSelector] = useState(null);
  const [modal, setModal] = useState(null);
  const [hoverCandle, setHoverCandle] = useState(null);
  const [showAdv, setShowAdv] = useState(false);
  const [pol, setPol] = useState({ base_spread_bps: 50, inventory_skew_bps: 150 });
  const [busy, setBusy] = useState(false);
  const [chain, setChain] = useState(null); // {vault, position, claimable, err} from RPC views
  const [seq, setSeq] = useState(1042);
  const [observedAt, setObservedAt] = useState(Date.now());
  const [nowTick, setNowTick] = useState(Date.now());
  const inputRef = useRef(null);
  const [showCreate, setShowCreate] = useState(false);
  const [cForm, setCForm] = useState({ pair: 'USD/PHP', name: 'Philippine peso', cat: 'FX', source: 'BSP ref', level: '58.5', prob: '35', resolves: 'DEC26' });
  const isOddsCat = (c) => c === 'Sports' || c === 'Events';

  const sel = useMemo(() => markets.find(m => m.id === selId) || markets[0], [markets, selId]);
  const ageSecs = Math.max(0, (nowTick - observedAt) / 1000);
  const spot = useMemo(() => sel.inception * (sel.price / sel.base), [sel]);
  const spotCandles = useMemo(() => sel.candles.map(c => { const k = sel.inception / sel.base; return { o: c.o * k, h: c.h * k, l: c.l * k, c: c.c * k, v: c.v }; }), [sel]);
  const fxK = sel.inception / sel.base;
  const poolUtilNow = poolCash > 0 ? Math.min(1, Object.entries(holdings).reduce((s2, [id, u]) => {
    const m = markets.find(x => x.id === id);
    return s2 + (m ? u * m.base * FORMULA.cap : 0);
  }, 0) / poolCash) : 1;
  const q = useMemo(() => quote(sel, spot, ageSecs, sel.dev, pol, poolUtilNow), [sel, spot, ageSecs, pol, poolUtilNow]);

  // wallet (dynamic import — never blocks first render)
  useEffect(() => {
    let c = false;
    (async () => { try { const r = await initWallet(); if (!c && r) { setSelector(r.selector); setModal(r.modal); } } catch {} })();
    return () => { c = true; };
  }, []);
  useEffect(() => {
    if (!selector) return;
    const s = selector.store.on('signIn', () => setAccount(selector.store.getState().accounts?.[0]?.accountId || 'connected.testnet'));
    return () => s.remove();
  }, [selector]);

  // on-chain truth via RPC views (read-only; independent of paper simulation)
  const refreshChain = async () => {
    try {
      const who = account || CONFIG.contractId;
      const [vault, position, claimable] = await Promise.all([
        viewViaRpc('vault_state', {}),
        viewViaRpc('position_of', { account: who, market_id: selId }).catch(() => '—'),
        viewViaRpc('claimable_of', { account: who }).catch(() => '—'),
      ]);
      setChain({ vault, position, claimable, err: null, who });
    } catch (e) {
      setChain({ vault: null, position: '—', claimable: '—', err: String(e?.message || e), who: account || CONFIG.contractId });
    }
  };
  useEffect(() => {
    refreshChain();
    const id = setInterval(refreshChain, 15000);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [account, selId]);

  // close the create-market dialog with Escape
  useEffect(() => {
    if (!showCreate) return;
    const h = (e) => { if (e.key === 'Escape') setShowCreate(false); };
    window.addEventListener('keydown', h);
    return () => window.removeEventListener('keydown', h);
  }, [showCreate]);

  // live clock for oracle age (price visibly worsens between bundles)
  useEffect(() => { const id = setInterval(() => setNowTick(Date.now()), 100); return () => clearInterval(id); }, []);

  // oracle bundles arrive ~every 2.8s: age resets, EVERY market ticks so
  // held positions/PnL move no matter which market is selected
  useEffect(() => {
    const id = setInterval(() => {
      setObservedAt(Date.now());
      setSeq(s => s + 1);
      setMarkets(prev => prev.map(m => {
        const drift = (Math.random() - 0.47) * 0.006;
        const np = Math.max(0.5, +(m.price * (1 + drift)).toFixed(2));
        const candles = [...m.candles];
        const last = { ...candles[candles.length - 1] };
        last.c = np; last.h = Math.max(last.h, np); last.l = Math.min(last.l, np); last.v += 30 + Math.random() * 90;
        candles[candles.length - 1] = last;
        if (Math.random() > 0.72) candles.push({ o: np, h: np, l: np, c: np, v: 20 });
        return { ...m, price: np, candles: candles.slice(-56), vol24: m.vol24 + Math.floor(Math.random() * 900), dev: Math.max(8, Math.min(140, m.dev + Math.round((Math.random() - 0.5) * 14))) };
      }));
    }, 2800);
    return () => clearInterval(id);
  }, [selId]);

  const filtered = useMemo(() => {
    let list = cat === 'All' ? markets : markets.filter(m => m.cat === cat);
    const qq = query.trim().toLowerCase();
    if (qq) list = list.filter(m => m.id.toLowerCase().includes(qq) || m.name.toLowerCase().includes(qq) || m.sym.toLowerCase().includes(qq) || m.source.toLowerCase().includes(qq));
    const chg = m => (m.price - m.prev) / m.prev;
    if (sort === 'gainers') list = [...list].sort((a, b) => chg(b) - chg(a));
    else if (sort === 'losers') list = [...list].sort((a, b) => chg(a) - chg(b));
    else if (sort === 'vol') list = [...list].sort((a, b) => b.vol24 - a.vol24);
    else if (sort === 'tvl') list = [...list].sort((a, b) => b.coll - a.coll);
    else list = [...list].sort((a, b) => a.id.localeCompare(b.id));
    return list;
  }, [markets, query, cat, sort]);

  useEffect(() => setHighlight(0), [query, cat, sort]);
  useEffect(() => {
    const h = (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); inputRef.current?.focus(); inputRef.current?.select(); }
      if (!focused) return;
      if (e.key === 'ArrowDown') { e.preventDefault(); setHighlight(v => Math.min(filtered.length - 1, v + 1)); }
      if (e.key === 'ArrowUp') { e.preventDefault(); setHighlight(v => Math.max(0, v - 1)); }
      if (e.key === 'Enter' && filtered[highlight]) { setSelId(filtered[highlight].id); setQuery(''); inputRef.current?.blur(); }
      if (e.key === 'Escape') { setQuery(''); inputRef.current?.blur(); }
    };
    window.addEventListener('keydown', h);
    return () => window.removeEventListener('keydown', h);
  }, [focused, filtered, highlight]);

  const vaultColl = poolCash;
  const totalReserved = Object.entries(holdings).reduce((s2, [id, u]) => {
    const m = markets.find(x => x.id === id);
    return s2 + (m ? u * m.base * FORMULA.cap : 0);
  }, 0);
  const poolNavOf = (liab) => Math.max(0, poolCash - liab);
  const totalMtm = useMemo(() => markets.reduce((s, m) => s + m.minted * m.price, 0), [markets]);
  const coverage = totalReserved ? vaultColl / totalReserved : 99;
  const totalVol = useMemo(() => markets.reduce((s, m) => s + m.vol24, 0), [markets]);
  const gainers = useMemo(() => [...markets].sort((a, b) => (b.price - b.prev) / b.prev - (a.price - a.prev) / a.prev).slice(0, 14), [markets]);

  const liab = sel.minted * q.fair;
  const maxPay = sel.base * FORMULA.cap;
  const maxLiab = sel.minted * maxPay;
  const backing = sel.minted ? sel.coll / sel.minted : 0;
  const amt = parseFloat(size) || 0;
  const px = buy ? q.ask : q.bid;
  const receive = px > 0 ? amt / px : 0;
  const myBal = holdings[sel.id] || 0;
  const canSell = !buy && myBal >= receive - 1e-9;
  const tooOld = ageSecs > FORMULA.maxAge;

  const flash = (msg) => { setToast(msg); setTimeout(() => setToast(null), 2600); };
  useEffect(() => {
    try { localStorage.setItem(PAPER_KEY, JSON.stringify({ balance, holdings, cost, lp, poolCash, poolShares })); }
    catch { if (!saveWarned.current) { saveWarned.current = true; flash('Could not save paper positions in this browser — they will reset on refresh'); } }
  }, [balance, holdings, cost, lp, poolCash, poolShares]);

  // positions marked at EXIT value: what a sell pays now (bid, capped $20/unit)
  const exitValue = (m, units) => Math.min(units * quote(m, spotOf(m), ageSecs, m.dev, pol, poolUtilNow).bid, units * m.base * FORMULA.cap);
  const posRows = Object.entries(holdings).filter(([, v]) => v > 0.001).map(([id, units]) => {
    const m = markets.find(x => x.id === id);
    const spent = cost[id] || 0;
    const value = m ? exitValue(m, units) : 0;
    const avg = units ? spent / units : 0;
    return { id, m, units, spent, avg, now: m?.price || 0, entryLevel: m ? m.inception * (avg / m.base) : 0, nowLevel: m ? spotOf(m) : 0, value, pnl: value - spent, pct: spent ? (value - spent) / spent * 100 : 0 };
  }).sort((a, b) => a.id.localeCompare(b.id)); // stable order: rows never move as prices tick
  const totalPnl = posRows.reduce((s2, r) => s2 + r.pnl, 0);
  const totalPosValue = posRows.reduce((s2, r) => s2 + r.value, 0);
  const poolNav = poolNavOf(totalPosValue);
  const myLpValue = poolShares > 0 ? lp.shares * poolNav / poolShares : 0;
  const myPoolPct = poolShares > 0 ? lp.shares / poolShares * 100 : 0;
  // Pool PnL = worth now minus net cash put in (deposits - withdrawals).
  const lpPnl = myLpValue - lp.deposited;
  const lpPnlPct = lp.deposited > 0 ? lpPnl / lp.deposited * 100 : 0;
  const connect = () => {
    if (modal) { modal.show(); return; }
    flash(CONFIG.networkId === 'sandbox'
      ? 'No browser wallet on the local sandbox — paper mode, RPC views below are real'
      : 'Wallet unavailable — staying in paper mode (no fake account)');
  };
  const pushOracle = () => { setObservedAt(Date.now()); setSeq(s => s + 1); flash('Fresh oracle bundle accepted — best price restored'); };
  const exec = async () => {
    if (!(amt > 0) || tooOld || busy) return;
    if (buy && amt > balance && !live) { flash(`Insufficient paper USDC — balance ${fmt$(balance, 0)}`); return; }
    if (!buy && myBal <= 0.001 && !live) { flash(`No ${sel.pair} position to sell`); return; }
    // Dollar-denominated sells cap at units actually held (bid moves continuously;
    // asking for $X at a decayed bid must mean "sell all", not "blocked").
    if (live) {
      setBusy(true);
      try {
        const b = await fetchBundle(sel.id, spot);
        const bundleArgs = { spot: b.spot, deviation_bps_1m: b.deviation_bps_1m, observed_at_ms: b.observed_at_ms, seq: b.seq, signature: b.signature };
        if (buy) {
          await sendFtTransferCall(selector, Math.round(amt * 1e6), { action: 'buy', market_id: sel.id, max_price_usdc_minor: String(Number.MAX_SAFE_INTEGER), ...bundleArgs });
          flash(`On-chain BUY submitted · ${sel.pair} ${fmt$(amt, 0)} — check On-chain panel`);
        } else {
          const chainUnits = Number(chain?.position) / 1e6;
          const sellUnits = Number.isFinite(chainUnits) && chainUnits > 0 ? Math.min(receive, chainUnits) : receive;
          const unitsMinor = Math.round(sellUnits * 1e6);
          await callMethod(selector, 'sell', { market_id: sel.id, units_minor: String(unitsMinor), min_price_usdc_minor: '0', ...bundleArgs });
          flash(`On-chain SELL submitted · payout lands in claimable — use Claim`);
        }
        setTimeout(refreshChain, 2500);
      } catch (e) {
        flash(`Transaction failed: ${String(e?.message || e).slice(0, 140)}`);
      } finally { setBusy(false); }
      return;
    }
    // PAPER path: one consistent ledger vs the pool (mirrors the core).
    let units = receive;
    let payout = 0;
    if (buy) {
      const reserveAfter = totalReserved + units * sel.base * FORMULA.cap;
      if (poolCash + amt < reserveAfter) { flash(`Pool cannot cover this trade — reserve ${fmt$(reserveAfter, 0)} > pool ${fmt$(poolCash + amt, 0)}`); return; }
      setBalance(bal => bal - amt);
      setPoolCash(c => c + amt);
    } else {
      units = Math.min(units, myBal);
      if (units >= myBal * 0.999) units = myBal; // sell-all leaves no dust
      payout = Math.min(units * q.bid, units * sel.base * FORMULA.cap, poolCash);
      setBalance(bal => bal + payout);
      setPoolCash(c => c - payout);
    }
    setCost(c => buy
      ? { ...c, [sel.id]: (c[sel.id] || 0) + amt }
      : { ...c, [sel.id]: Math.max(0, (c[sel.id] || 0) * (myBal ? 1 - units / myBal : 0)) });
    setHoldings(h => ({ ...h, [sel.id]: Math.max(0, (h[sel.id] || 0) + (buy ? units : -units)) }));
    setMarkets(prev => prev.map(m => m.id === selId ? { ...m, minted: Math.max(0, m.minted + (buy ? Math.round(units) : -Math.round(units))), vol24: m.vol24 + amt } : m));
    setFills(f => [{ id: Date.now(), side: buy ? 'BUY' : 'SELL', tok: sel.pair, units, usdc: buy ? amt : payout, px, level: spot * px / Math.max(q.fair, 0.01), t: Date.now(), paper: true }, ...f].slice(0, 7));
    flash(buy ? `PAPER bought ${fmt$(amt, 0)} of ${sel.pair} @ ${fmtFx(spot * px / Math.max(q.fair, 0.01))}` : `PAPER sold for ${fmt$(payout, 0)}`);
  };
  const claimNow = async () => {
    if (!live || busy) return;
    setBusy(true);
    try { await callMethod(selector, 'claim', {}); flash('Claim submitted — USDC transfer pending'); setTimeout(refreshChain, 3000); }
    catch (e) { flash(`Claim failed: ${String(e?.message || e).slice(0, 140)}`); }
    finally { setBusy(false); }
  };

  const poolMove = async (deposit) => {
    const a = parseFloat(poolAmt) || 0;
    if (!(a > 0) || busy) return;
    if (live) {
      setBusy(true);
      try {
        if (deposit) { await sendFtTransferCall(selector, Math.round(a * 1e6), { action: 'deposit_lp' }); flash(`Pool deposit submitted · ${fmt$(a, 0)}`); }
        else { await callMethod(selector, 'withdraw_lp', { shares: String(Math.round(a * 1e6)) }); flash('Pool withdrawal credited — use Claim in On-chain panel'); }
        setTimeout(refreshChain, 2500);
      } catch (e) { flash(`Pool transaction failed: ${String(e?.message || e).slice(0, 140)}`); }
      finally { setBusy(false); }
      return;
    }
    if (deposit) {
      if (a > balance) { flash(`Insufficient paper USDC — balance ${fmt$(balance, 0)}`); return; }
      if (poolNav <= 0 && poolShares > 0) { flash('Pool has no value left to price new shares'); return; }
      const minted = poolShares === 0 ? a : a * poolShares / poolNav;
      setBalance(b => b - a); setPoolCash(c => c + a); setPoolShares(t => t + minted);
      setLp(x => ({ deposited: x.deposited + a, shares: x.shares + minted }));
      flash(`PAPER deposited ${fmt$(a, 0)} → ${minted.toFixed(1)} pool shares`);
    } else {
      if (!(lp.shares > 0)) { flash('Nothing deposited in the pool yet'); return; }
      const free = Math.max(0, poolCash - totalReserved);
      const wantShares = poolNav > 0 ? Math.min(lp.shares, a * poolShares / poolNav) : 0;
      const out = Math.min(poolShares > 0 ? wantShares * poolNav / poolShares : 0, free);
      if (!(out > 0)) { flash(`Pool cash is reserved against open positions — free now: ${fmt$(free, 0)}`); return; }
      const burned = poolNav > 0 ? out * poolShares / poolNav : wantShares;
      setBalance(b => b + out); setPoolCash(c => c - out); setPoolShares(t => t - burned);
      setLp(x => ({ deposited: Math.max(0, x.deposited - out), shares: Math.max(0, x.shares - burned) }));
      flash(`PAPER withdrew ${fmt$(out, 0)} from the pool${out < a ? ` (asked ${fmt$(a, 0)}; rest is reserved)` : ''}`);
    }
  };

  async function launchMarket() {
    const sym = cForm.pair.toUpperCase().replace(/[^A-Z0-9]/g, '');
    if (!sym || !cForm.name.trim()) { flash('Name and pair are required'); return; }
    if (markets.some(m => m.id.toUpperCase() === sym)) { flash(`${sym} already exists`); return; }
    if (balance < 10) { flash('Need $10 USDC for the creation fee'); return; }
    const odds = isOddsCat(cForm.cat);
    const prob = Math.min(0.94, Math.max(0.04, parseFloat(cForm.prob) / 100 || 0));
    const level = parseFloat(cForm.level);
    if (!odds && !(level > 0)) { flash('Spot level must be > 0'); return; }
    if (odds && !(prob > 0)) { flash('Probability must be 1–99%'); return; }
    if (live && !busy) {
      setBusy(true);
      try {
        const ok = await fetchOraclePubkey();
        const maturity = Math.floor(Date.now() / 1000) + 30 * 86400;
        await sendFtTransferCall(selector, 10_000_000, {
          action: 'create_market', market_id: sym, name: cForm.name.trim(),
          oracle_signer: ok.signer, oracle_pubkey: ok.pubkey,
          kind: odds ? 'BinaryV1' : 'SyntheticSpotV1', base_usdc_minor: '10000000',
          inception: odds ? '1' : String(level), maturity_unix_secs: String(maturity),
        });
        setShowCreate(false);
        flash(`On-chain market ${sym} submitted at 0% allocation — owner must enable risk before buys`);
        setTimeout(refreshChain, 2500);
      } catch (e) { flash(`Create failed: ${String(e?.message || e).slice(0, 140)}`); }
      finally { setBusy(false); }
      return;
    }
    const price = odds ? +(10 * prob).toFixed(2) : 10;
    const nm = {
      id: sym, sym, pair: cForm.pair.toUpperCase(), name: cForm.name.trim(),
      source: cForm.source.trim() || 'Custom oracle', cat: cForm.cat,
      maturity: odds ? (cForm.resolves.trim() || 'DEC26') : '', odds,
      base: 10, inception: odds ? 1 : level, price, prev: price,
      inv: 0, cap: 25000, coll: 0, minted: 0, tvl: 0, vol24: 0, dev: 40,
      high: price, low: price,
      candles: genCandles(mulberry(Date.now() % 2147483647), price),
    };
    setMarkets(prev => [nm, ...prev]);
    setBalance(b => b - 10);
    setCat('All'); setQuery(''); setSelId(sym); setShowCreate(false);
    flash(`PAPER market ${nm.pair} added locally — NOT created on-chain`);
  }
  const pick = (id) => { setSelId(id); };

  const cndl = hoverCandle || spotCandles[spotCandles.length - 1];
  const dayChg = (sel.price - sel.prev) / sel.prev * 100;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand-wrap"><div className="logo">ZT</div><div className="brand">Zero<em>Tick</em></div><span className="live-dot" /></div>
        <div className="screen-tabs" role="tablist" aria-label="Main screens" style={{ margin: '0 auto' }}>
          <button className={`screen-tab ${tab === 'trade' ? 'active' : ''}`} role="tab" aria-selected={tab === 'trade'} onClick={() => setTab('trade')}>Trade</button>
          <button className={`screen-tab ${tab === 'earn' ? 'active' : ''}`} role="tab" aria-selected={tab === 'earn'} onClick={() => setTab('earn')}>Earn</button>
        </div>
        <div className="vault-pill" title="Cash is shared. Trade and Pool each track their own PnL on their screen.">
          CASH <b>{fmt$(balance, 0)}</b><span className="sep" />POSITIONS <b>{fmt$(totalPosValue)}</b><span className="sep" />POOL <b>{fmt$(myLpValue)}</b>
        </div>
        <div className="top-actions">
          {account
            ? <><span className="wallet-pill"><span className="dot" />{account}</span><button className="btn" onClick={() => setAccount('')}>Disconnect</button></>
            : <button className="btn btn-accent" onClick={connect}>Connect wallet</button>}
        </div>
      </header>

      <div className="paper-banner" role="note">
        {live
          ? <>● LIVE MODE — trades submit real transactions on {CONFIG.networkId}. Market data below is still the local paper tape until the oracle feed is wired per market.</>
          : <>● PAPER — simulated money, saved in your browser. Real contract state is in the On-chain box.</>}
      </div>

      {tab === 'trade' && (
      <main className="terminal">
        {/* left rail */}
        <aside className="rail">
          <div className="rail-top"><span className="panel-title">Markets</span><button className="btn btn-accent" style={{ padding: '7px 11px', fontSize: 12 }} onClick={() => setShowCreate(true)}>+ New · $10</button></div>
          <div className="search-outer">
            <div className={`search-bar ${focused ? 'on' : ''}`}>
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="11" cy="11" r="7" /><line x1="16" y1="16" x2="22" y2="22" /></svg>
              <input ref={inputRef} aria-label="Search markets" placeholder="Search markets…" value={query} onChange={e => setQuery(e.target.value)}
                onFocus={() => setFocused(true)} onBlur={() => setTimeout(() => setFocused(false), 150)} />
              {query ? <button className="clear" onClick={() => setQuery('')}>×</button> : <kbd>⌘K</kbd>}
            </div>
            {focused && query && filtered.length > 0 && (
              <div className="suggest">
                {filtered.slice(0, 6).map((m, i) => (
                  <button key={m.id} className={`suggest-row ${i === highlight ? 'hl' : ''}`} onMouseEnter={() => setHighlight(i)} onClick={() => { pick(m.id); setQuery(''); }}>
                    <span><b>{m.pair}</b> <small>{m.name} · {m.maturity}</small></span><span className={m.price >= m.prev ? 'up' : 'down'}>{fmtFx(spotOf(m))}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
          <div className="rail-count">{filtered.length} markets</div>
          <div className="rail-list">
            {filtered.map(m => {
              const c = (m.price - m.prev) / m.prev * 100;
              return (
                <button key={m.id} className={`mrow ${m.id === selId ? 'active' : ''}`} onClick={() => pick(m.id)}>
                  <div className="mrow-main">
                    <div className="mrow-top"><b>{m.pair}</b><span className={`chg ${c >= 0 ? 'up' : 'down'}`}>{c >= 0 ? '+' : ''}{c.toFixed(1)}%</span></div>
                    <div className="mrow-sub"><i className="cat-dot" style={{ background: CAT_COLOR[m.cat] }} /> {m.cat} · {m.name} · vol {fmtK(m.vol24)}</div>
                  </div>
                  <div className="mrow-right"><Sparkline points={m.candles.slice(-10).map(x => x.c)} up={c >= 0} /><div className="mprice">{fmtFx(spotOf(m))}</div></div>
                </button>
              );
            })}
            {filtered.length === 0 && <div className="empty">Nothing matches “{query}”.<button onClick={() => { setQuery(''); setCat('All'); }}>Clear</button></div>}
          </div>
        </aside>

        {/* center */}
        <section className="center-col">
          <div className="pair-head">
            <div>
              <div className="pair-title">{sel.pair}<span className={`chg big ${dayChg >= 0 ? 'up' : 'down'}`}>{dayChg >= 0 ? '▲ +' : '▼ '}{dayChg.toFixed(2)}%</span></div>
              <div className="pair-sub">{sel.name} · {sel.cat} · {sel.source} oracle · {sel.odds ? 'BinaryV1 · pays $10 if YES' : 'SyntheticSpotV1 · no expiry · close anytime'}</div>
            </div>
            <div className="pair-price">{sel.odds
              ? <><div className="bigprice">{(q.fair / sel.base * 100).toFixed(0)}% <small style={{ fontSize: 15, color: 'var(--muted)' }}>{(sel.base / Math.max(q.fair, 0.01)).toFixed(2)}× odds</small></div><div className="pair-sub">win probability · = {fmt$(q.fair)} per $10 unit · {sel.source.split(' · ')[0]} mid</div></>
              : <><div className="bigprice">{fmtFx(spot * (q.fair / Math.max(sel.price, 0.01)))}</div><div className="pair-sub">{sel.pair.includes('/') ? `${sel.pair.split('/')[1]} per 1 USD` : 'USD price'} · oracle level {fmtFx(spot)}</div></>}</div>
          </div>

          <div className="ohlc">
            {cndl && <><span>O <b>{fmtFx(cndl.o)}</b></span><span>H <b>{fmtFx(cndl.h)}</b></span><span>L <b>{fmtFx(cndl.l)}</b></span><span>C <b className={cndl.c >= cndl.o ? 'up' : 'down'}>{fmtFx(cndl.c)}</b></span><span>Vol <b>{Math.round(cndl.v)}</b></span></>}
            <span className="ohlc-right">ask <b className="up">{fmtFx(spot * q.ask / Math.max(q.fair, 0.01))}</b> · bid <b>{fmtFx(spot * q.bid / Math.max(q.fair, 0.01))}</b></span>
          </div>
          <div className="chart-wrap"><CandleChart candles={spotCandles} height={300} onHover={setHoverCandle} /></div>

          <div className="hint" style={{ margin: '8px 2px' }}>Price {ageSecs.toFixed(1)}s old · spread {Math.round(q.half * 2)} bps{q.skewBps > 1 ? ` (incl. +${Math.round(q.skewBps)} liquidity — pool ${(poolUtilNow * 100).toFixed(0)}% reserved)` : ''} · Pool {fmtK(vaultColl)}</div>

          {sel.odds && (
          <div className="card payoff">
            <div className="card-title">Resolution {sel.maturity} <span className="right">official result sends 1 or 0</span></div>
            <div className="yesno">
              <div className="yn yes"><span>YES · {(q.fair / sel.base * 100).toFixed(0)}% implied</span><b>$10.00 / unit</b><small>your {receive ? receive.toFixed(1) : '0'} units → {fmt$(receive * sel.base)} ({receive * sel.base - amt >= 0 ? '+' : ''}{fmt$(receive * sel.base - amt, 0)})</small></div>
              <div className="yn no"><span>NO · {(100 - q.fair / sel.base * 100).toFixed(0)}%</span><b>$0.00 / unit</b><small>your {receive ? receive.toFixed(1) : '0'} units → $0 (−{fmt$(amt, 0)})</small></div>
            </div>
            <div className="hint">Settles on the official result: YES pays $10/unit, NO pays $0.</div>
          </div>
          )}

          {/* Positions: always rendered, same place, stable order — for every market type. */}
          <div className="card payoff" data-testid="positions-panel">
            <div className="card-title">Your positions {posRows.length > 0 && <span className="right">total {fmt$(totalPosValue)} · <b className={totalPnl >= 0 ? 'up' : 'down'}>{totalPnl >= 0 ? '+' : '−'}{fmt$(Math.abs(totalPnl))}</b></span>}</div>
            {posRows.length === 0
              ? <div className="hint">None yet. Buy something on the right — it shows up here with its profit/loss.</div>
              : posRows.map(r => (
                <button key={r.id} className="feed-row pos-row" onClick={() => { pick(r.id); setBuy(false); }} title="Select to sell">
                  <span><b>{r.m ? r.m.pair : r.id}</b> <small>spent {fmt$(r.spent, 0)} @ {fmtFx(r.entryLevel)} · now {fmtFx(r.nowLevel)}</small></span>
                  <b className={r.pnl >= 0 ? 'up' : 'down'}>{fmt$(r.value)} ({r.pnl >= 0 ? '+' : '−'}{fmt$(Math.abs(r.pnl))}, {r.pct >= 0 ? '+' : ''}{r.pct.toFixed(1)}%)</b>
                </button>))}
            <div className="hint">Click a position to sell it. Paper positions are saved in your browser.</div>
          </div>
        </section>

        {/* right ticket */}
        <aside className="ticket">
          <div className="tabs buy-sell">
            <button className={`tab buy ${buy ? 'active' : ''}`} onClick={() => setBuy(true)}>Buy</button>
            <button className={`tab sell ${!buy ? 'active' : ''}`} onClick={() => setBuy(false)}>Sell</button>
          </div>
          <div className="bal-row"><span>{live ? 'Paper reference balance' : 'Paper USDC balance'}</span><b>${balance.toLocaleString()}</b></div>
          <label className="flabel" htmlFor="amt-input">Amount · USDC</label>
          <div className="amount-box"><input id="amt-input" type="number" min="1" value={size} onChange={e => setSize(e.target.value)} /><span>USDC</span></div>
          <div className="quick">{[25, 50, 75, 100].map(p => <button key={p} onClick={() => setSize(String(Math.round((buy ? balance : Math.min(myBal * q.bid, myBal * sel.base * FORMULA.cap)) * p / 100)))}>{p === 100 ? 'MAX' : p + '%'}</button>)}</div>
          <div className="receive">
            <span>{buy ? 'You get' : 'You sell'}</span>
            <b>{fmt$(amt, 0)} <em>of {sel.pair}</em></b>
            <small>at {fmtFx(spot * px / Math.max(q.fair, 0.01))}{sel.odds ? ` · if YES pays ${fmt$(receive * sel.base, 0)}` : ''}</small>
          </div>
          <button className={`cta ${buy ? 'cta-buy' : 'cta-sell'}`} onClick={exec} disabled={tooOld || busy}>
            {busy ? 'SUBMITTING…' : tooOld ? 'ORACLE STALE — WAITING' : <>{buy ? '▲ BUY' : '▼ SELL'} {sel.pair} @ {fmtFx(spot * px / Math.max(q.fair, 0.01))}<small>{fmt$(amt, 0)} USDC · {live ? 'on-chain via USDC transfer' : 'paper simulation'}</small></>}
          </button>
          {fills.length > 0 && <div className="holdings"><div className="card-title">Recent trades</div>{fills.map(f => <div key={f.id} className="feed-row"><span className={f.side === 'BUY' ? 'up' : 'down'}>{f.side} {f.tok}</span><b>{fmt$(f.usdc, 0)} @ {fmtFx(f.level || 0)}</b></div>)}</div>}
          <div className="holdings">
            <div className="card-title">On-chain · {CONFIG.networkId}</div>
            {chain?.err
              ? <div className="hint">RPC unreachable: {chain.err}. Paper numbers above are unaffected and simulated.</div>
              : chain?.vault
                ? <>
                    <div className="kv"><span>Vault collateral</span><b>{fmt$((Number(chain.vault.collateral) || 0) / 1e6, 0)}</b></div>
                    <div className="kv"><span>Coverage</span><b>{chain.vault.coverage === 'inf' ? '∞' : Number(chain.vault.coverage).toFixed(2)}×</b></div>
                    <div className="kv"><span>Markets on-chain</span><b>{chain.vault.markets}</b></div>
                    <div className="kv"><span>Position ({String(chain.who).slice(0, 18)})</span><b>{chain.position}</b></div>
                    <div className="kv"><span>Claimable (minor)</span><b>{chain.claimable}</b></div>
                    {live && chain.claimable !== '0' && chain.claimable !== '—' && <button className="btn btn-accent" style={{ marginTop: 8 }} onClick={claimNow} disabled={busy}>Claim USDC</button>}
                  </>
                : <div className="hint">Loading contract state from {CONFIG.nodeUrl}…</div>}
            <button className="btn" style={{ marginTop: 8 }} onClick={refreshChain}>⟳ Refresh on-chain state</button>
          </div>
        </aside>
      </main>
      )}

      {tab === 'earn' && (
      <main className="earn-wrap">
        <div className="earn-stats">
          <div className="earn-stat"><span>Pool cash</span><b>{live && chain?.vault ? fmt$((Number(chain.vault.collateral) || 0) / 1e6, 0) : fmt$(poolCash, 0)}</b><small>pays every trade on every market</small></div>
          <div className="earn-stat"><span>Yours is worth</span><b>{live ? '—' : fmt$(myLpValue)}</b><small>{live ? 'settles on-chain at withdrawal' : `put in ${fmt$(lp.deposited, 0)} · ${myPoolPct.toFixed(2)}% of pool`}</small></div>
          <div className="earn-stat"><span>Pool PnL</span><b className={lpPnl >= 0 ? 'up' : 'down'}>{live ? '—' : `${lpPnl >= 0 ? '+' : '−'}${fmt$(Math.abs(lpPnl))}`}</b><small>{live ? 'see withdrawal value on-chain' : `${lpPnlPct >= 0 ? '+' : ''}${lpPnlPct.toFixed(1)}% on your deposit`}</small></div>
        </div>

        <div className="earn-grid">
          <div className="card">
            <div className="pool-modes" role="group" aria-label="Pool action">
              <button className={`tab ${poolMode === 'deposit' ? 'active' : ''}`} onClick={() => setPoolMode('deposit')}>Deposit</button>
              <button className={`tab ${poolMode === 'withdraw' ? 'active' : ''}`} onClick={() => setPoolMode('withdraw')}>Withdraw</button>
            </div>
            <label className="flabel" htmlFor="pool-amt" style={{ marginTop: 12 }}>Amount · USDC</label>
            <div className="amount-box"><input id="pool-amt" type="number" min="1" value={poolAmt} onChange={e => setPoolAmt(e.target.value)} /><span>USDC</span></div>
            <div className="quick" style={{ marginTop: 8 }}>
              {[25, 50, 75, 100].map(pct => <button key={pct} onClick={() => setPoolAmt(String(Math.round((poolMode === 'deposit' ? balance : myLpValue) * pct / 100)))}>{pct === 100 ? 'MAX' : pct + '%'}</button>)}
            </div>
            <button className="cta cta-buy" style={{ marginTop: 14 }} onClick={() => poolMove(poolMode === 'deposit')} disabled={busy || !(parseFloat(poolAmt) > 0)}>
              {busy ? 'SUBMITTING…' : <>{poolMode === 'deposit' ? 'Deposit' : 'Withdraw'} {fmt$(parseFloat(poolAmt) || 0, 0)}<small>{poolMode === 'deposit' ? `cash ${fmt$(balance, 0)} available` : `free to withdraw ${fmt$(Math.max(0, poolCash - totalReserved), 0)}`}</small></>}
            </button>
            <div className="hint" style={{ marginTop: 10 }}>Withdrawals pay only from free cash — money reserved against open positions stays until those positions close.</div>
            {!live && lp.shares > 0 && posRows.length > 0 && (
              <div className="hint" style={{ marginTop: 6 }}>Heads up: you own {myPoolPct.toFixed(2)}% of this pool <em>and</em> trade against it — part of your trading profit is paid by your own deposit. Judge the two PnLs together, not separately.</div>
            )}
          </div>

          <div>
            <div className="card">
              <div className="card-title">Pool details</div>
              <div className="kv" style={{ marginTop: 6 }}><span>Reserved for open positions</span><b>{live && chain?.vault ? fmt$((Number(chain.vault.reserved_max) || 0) / 1e6) : fmt$(totalReserved)}</b></div>
              <div className="kv"><span>Free to withdraw</span><b>{live && chain?.vault ? fmt$((Number(chain.vault.free) || 0) / 1e6) : fmt$(Math.max(0, poolCash - totalReserved))}</b></div>
              <div className="kv"><span>Your cash (shared with trading)</span><b>{fmt$(balance, 0)}</b></div>
              {!live && <div className="kv"><span>Your shares</span><b>{lp.shares.toFixed(1)}</b></div>}
            </div>
            <div className="card" style={{ marginTop: 12 }}>
              <div className="card-title">How you make / lose money here</div>
              <div className="hint">1. Every trade pays a spread into the pool — that's income.<br/>2. When traders win, the pool pays them — that's the loss.<br/>3. Each payout is capped at its reserved maximum, so one trade can't empty the pool.</div>
            </div>
          </div>
        </div>
      </main>
      )}

      <footer className="footer"><span>{live ? 'Live' : 'Paper'} · shared vault · sells capped at reserved max · signed oracle bundles</span><span>{CONFIG.contractId} · {CONFIG.networkId}</span></footer>
      {showCreate && (
        <div className="overlay" role="dialog" aria-modal="true" aria-label="Launch a market" onClick={() => setShowCreate(false)}>
          <div className="create-card" onClick={e => e.stopPropagation()}>
            <div className="create-head"><div><div className="create-title">Launch a market</div><div className="create-sub">One call on-chain: <code>create_market(id, name, oracle_signer, pubkey, kind, base, inception, maturity, fee)</code></div></div><button className="clear" onClick={() => setShowCreate(false)}>✕</button></div>
            <div className="create-grid">
              <div className="field"><label>Pair / event code</label><input value={cForm.pair} onChange={e => setCForm(f => ({ ...f, pair: e.target.value }))} placeholder="USD/PHP or FEDCUT" /></div>
              <div className="field"><label>Name</label><input value={cForm.name} onChange={e => setCForm(f => ({ ...f, name: e.target.value }))} placeholder="Philippine peso" /></div>
              <div className="field"><label>Category</label><select value={cForm.cat} onChange={e => setCForm(f => ({ ...f, cat: e.target.value }))}>{CATS.filter(c => c !== 'All').map(c => <option key={c} value={c}>{c}</option>)}</select></div>
              <div className="field"><label>Oracle source</label><input value={cForm.source} onChange={e => setCForm(f => ({ ...f, source: e.target.value }))} placeholder="BSP ref / Polymarket / Kalshi" /></div>
              {isOddsCat(cForm.cat)
                ? <div className="field"><label>Win probability % (venue mid)</label><input type="number" min="1" max="99" value={cForm.prob} onChange={e => setCForm(f => ({ ...f, prob: e.target.value }))} /></div>
                : <div className="field"><label>Current spot level (inception)</label><input type="number" value={cForm.level} onChange={e => setCForm(f => ({ ...f, level: e.target.value }))} /></div>}
              {isOddsCat(cForm.cat) && <div className="field"><label>Resolves</label><input value={cForm.resolves} onChange={e => setCForm(f => ({ ...f, resolves: e.target.value }))} placeholder="DEC26" /></div>}
            </div>
            <div className="fee-row"><span>Formula (auto by category)</span><b>{isOddsCat(cForm.cat) ? 'BinaryV1 · pays $10 if YES' : 'SyntheticSpotV1 · no expiry'}</b></div>
            <div className="fee-row"><span>Oracle signer</span><b>oracle.near · pubkey registered on-chain</b></div>
            <div className="fee-row total"><span>Creation fee → shared vault</span><b>$10.00 USDC</b></div>
            <div className="hint">The $10 goes into the one shared vault as collateral (LPs benefit). Duplicate or invalid creations are rejected before any fee is taken. Your market is tradeable the moment the oracle's first signed bundle lands.</div>
            <button className="btn btn-accent big" onClick={launchMarket}>Pay $10 · launch market</button>
          </div>
        </div>
      )}
      {toast && <div className="toast" role="status" aria-live="polite">{toast}</div>}
    </div>
  );

  function pushOracleNow() { setObservedAt(Date.now()); setSeq(s => s + 1); flash('Fresh bundle accepted'); }
}
