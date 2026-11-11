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
const VAULT_LP = 5_900_000; // shared-vault LP capital on top of per-market sale proceeds (display aggregate below)

/* formula IndexCappedV1 — mirrors src/protocol.rs */
const FORMULA = { baseBps: 50, skewBps: 150, staleBpsPerSec: 25, volDivisor: 2, maxAge: 10, cap: 2 };
function quote(m, spotLevel, ageSecs, devBps, pol) {
  const baseBps = pol?.base_spread_bps ?? FORMULA.baseBps;
  const skewCap = pol?.inventory_skew_bps ?? FORMULA.skewBps;
  const fair = m.base * Math.min(FORMULA.cap, Math.max(0, spotLevel / m.inception));
  const liability = m.minted * fair;
  const x = Math.min(1, liability / (FORMULA.cap * 10000));
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
  const [balance, setBalance] = useState(10000);
  const [holdings, setHoldings] = useState({});
  const [fills, setFills] = useState([]);
  const [toast, setToast] = useState(null);
  const [account, setAccount] = useState('');
  const live = LIVE_POSSIBLE && Boolean(account);
  const [selector, setSelector] = useState(null);
  const [modal, setModal] = useState(null);
  const [hoverCandle, setHoverCandle] = useState(null);
  const [simRet, setSimRet] = useState(30);
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
  const q = useMemo(() => quote(sel, spot, ageSecs, sel.dev, pol), [sel, spot, ageSecs, pol]);

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

  // oracle bundles arrive ~every 2.8s: age resets, selected market ticks
  useEffect(() => {
    const id = setInterval(() => {
      setObservedAt(Date.now());
      setSeq(s => s + 1);
      setMarkets(prev => prev.map(m => {
        const hot = m.id === selId || Math.random() > 0.9;
        if (!hot) return m;
        const drift = (Math.random() - 0.44) * 0.012;
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

  const vaultColl = useMemo(() => VAULT_LP + markets.reduce((s, m) => s + m.minted * m.price * 0.4, 0), [markets]);
  const totalReserved = useMemo(() => markets.reduce((s, m) => s + m.minted * m.base * FORMULA.cap, 0), [markets]);
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
  const simPayout = sel.base * Math.min(FORMULA.cap, Math.max(0, 1 + simRet / 100));
  const simPnl = receive * simPayout - amt;

  const flash = (msg) => { setToast(msg); setTimeout(() => setToast(null), 2600); };
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
    if (!buy && !canSell && !live) { flash(`Not enough s${sel.sym} — you hold ${myBal.toFixed(1)}`); return; }
    if (live) {
      setBusy(true);
      try {
        const b = await fetchBundle(sel.id, spot);
        const bundleArgs = { spot: b.spot, deviation_bps_1m: b.deviation_bps_1m, observed_at_ms: b.observed_at_ms, seq: b.seq, signature: b.signature };
        if (buy) {
          await sendFtTransferCall(selector, Math.round(amt * 1e6), { action: 'buy', market_id: sel.id, max_price_usdc_minor: String(Number.MAX_SAFE_INTEGER), ...bundleArgs });
          flash(`On-chain BUY submitted · ${sel.pair} ${fmt$(amt, 0)} — check On-chain panel`);
        } else {
          const unitsMinor = Math.round(receive * 1e6);
          await callMethod(selector, 'sell', { market_id: sel.id, units_minor: String(unitsMinor), min_price_usdc_minor: '0', ...bundleArgs });
          flash(`On-chain SELL submitted · payout lands in claimable — use Claim`);
        }
        setTimeout(refreshChain, 2500);
      } catch (e) {
        flash(`Transaction failed: ${String(e?.message || e).slice(0, 140)}`);
      } finally { setBusy(false); }
      return;
    }
    // PAPER path: local simulation only, explicitly labelled.
    const units = receive;
    setBalance(bal => buy ? bal - amt : bal + amt);
    setHoldings(h => ({ ...h, [sel.id]: Math.max(0, (h[sel.id] || 0) + (buy ? units : -units)) }));
    setMarkets(prev => prev.map(m => m.id === selId ? { ...m, minted: Math.max(0, m.minted + (buy ? Math.round(units) : -Math.round(units))), vol24: m.vol24 + amt } : m));
    setFills(f => [{ id: Date.now(), side: buy ? 'BUY' : 'SELL', tok: sel.pair, units, usdc: amt, px, t: Date.now(), paper: true }, ...f].slice(0, 7));
    flash(buy ? `PAPER +${units.toFixed(1)} units ${sel.pair} · ${fmt$(amt, 0)} @ ${fmt$(px)} (simulated)` : `PAPER closed ${units.toFixed(1)} units ${sel.pair} (simulated)`);
  };
  const claimNow = async () => {
    if (!live || busy) return;
    setBusy(true);
    try { await callMethod(selector, 'claim', {}); flash('Claim submitted — USDC transfer pending'); setTimeout(refreshChain, 3000); }
    catch (e) { flash(`Claim failed: ${String(e?.message || e).slice(0, 140)}`); }
    finally { setBusy(false); }
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
      <div className="tape" aria-hidden="true">
        <div className="tape-track">
          {[...gainers, ...gainers].map((m, i) => {
            const c = (m.price - m.prev) / m.prev * 100;
            return <span key={i} className="tape-item"><b>{m.pair}</b> {fmtFx(spotOf(m))} <i className={c >= 0 ? 'up' : 'down'}>{c >= 0 ? '▲' : '▼'} {Math.abs(c).toFixed(1)}%</i></span>;
          })}
        </div>
      </div>

      <header className="topbar">
        <div className="brand-wrap"><div className="logo">ZT</div><div className="brand">Zero<em>Tick</em></div><span className="live-dot" /></div>
        <div className="vault-pill" title="One shared USDC vault backs every internal position">
          SHARED VAULT <b>{fmtK(vaultColl)}</b><span className="sep" />COVERAGE <b className={coverage >= 1 ? 'ok' : 'bad'}>{coverage.toFixed(2)}×</b>
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
          : <>● PAPER TRADING — prices, balances and fills are simulated locally. Nothing here moves real funds. The “On-chain” panel shows real contract state via RPC. Not investment advice; synthetic positions can lose their full value.</>}
      </div>

      <div className="kpi-strip">
        <div className="kpi"><span>Synthetic markets</span><b>{markets.length}</b></div>
        <div className="kpi"><span>Shared vault</span><b>{fmtK(vaultColl)}</b></div>
        <div className="kpi"><span>Max liability covered</span><b>{coverage.toFixed(2)}×</b></div>
        <div className="kpi"><span>24h volume</span><b>{fmtK(totalVol)}</b></div>
        <div className="kpi"><span>Open units</span><b>{markets.reduce((s, m) => s + m.minted, 0).toLocaleString()}</b></div>
        <div className="kpi accent"><span>Quote update txs</span><b>0</b></div>
      </div>

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
          <div className="rail-controls">
            <div className="cat-pills">{CATS.map(c => <button key={c} className={`cat ${cat === c ? 'active' : ''}`} onClick={() => setCat(c)}>{c !== 'All' && <i className="cat-dot" style={{ background: CAT_COLOR[c] }} />}{c} <em>{c === 'All' ? markets.length : markets.filter(m => m.cat === c).length}</em></button>)}</div>
            <select className="sort" value={sort} onChange={e => setSort(e.target.value)}>
              <option value="gainers">↑ Gainers</option><option value="losers">↓ Losers</option><option value="vol">Volume</option><option value="tvl">TVL</option><option value="az">A–Z</option>
            </select>
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

          <div className="center-split">
            <div className="card">
              <div className="card-title">Oracle bundle <span className={`age-badge ${ageSecs <= 1 ? 'fresh' : ageSecs <= 4 ? 'warm' : 'stale'}`}>{ageSecs <= 1 ? '● BEST PRICE' : ageSecs <= 4 ? '● DECAYING' : '● STALE'} {ageSecs.toFixed(1)}s</span></div>
              <div className="bundle-grid">
                <div><span>Level</span><b>{spot.toLocaleString(undefined, { maximumFractionDigits: spot < 1 ? 4 : 1 })}</b></div>
                <div><span>Deviation 1m</span><b>{sel.dev} bps</b></div>
                <div><span>Sequence</span><b>#{seq}</b></div>
                <div><span>Signer</span><b>oracle.near ✓</b></div>
              </div>
              <div className="age-bar"><div style={{ width: `${Math.min(100, ageSecs / FORMULA.maxAge * 100)}%`, background: ageSecs <= 1 ? 'var(--green)' : ageSecs <= 4 ? 'var(--amber)' : 'var(--red)' }} /></div>
              <div className="checks">
                <span>✓ signature</span><span>✓ signer</span><span className={ageSecs > FORMULA.maxAge ? 'bad' : ''}>{ageSecs > FORMULA.maxAge ? '✗' : '✓'} fresh ≤{FORMULA.maxAge}s</span><span>✓ sequence</span><span className={sel.dev > 500 ? 'bad' : ''}>{sel.dev > 500 ? '✗' : '✓'} deviation</span>
              </div>
              <div className="spread-break">
                <span>Spread {Math.round(q.half * 2)} bps =</span>
                <i style={{ flex: FORMULA.baseBps, background: '#3d4a55' }} title="base" /><i style={{ flex: Math.max(1, q.stale), background: 'var(--amber)' }} title="staleness" /><i style={{ flex: Math.max(1, q.vol), background: '#4d7cff' }} title="vol" /><i style={{ flex: Math.max(1, q.skewBps), background: 'var(--lime)' }} title="skew" />
              </div>
              <div className="spread-legend"><span>base {FORMULA.baseBps}</span><span className="amber">age +{q.stale.toFixed(0)}</span><span className="blue">vol +{q.vol.toFixed(0)}</span><span className="lime">skew +{q.skewBps.toFixed(0)}</span></div>
              <button className="btn push" onClick={pushOracleNow}>⟳ New bundle — reset age</button>
            </div>

            <div className="card">
              <div className="card-title">Shared vault · this market</div>
              <div className="kv"><span>Vault collateral (all pairs)</span><b>{fmtK(vaultColl)}</b></div>
              <div className="kv"><span>Open units</span><b>{sel.minted.toLocaleString()}</b></div>
              <div className="kv"><span>Value owed now</span><b>{fmtK(liab)}</b></div>
              <div className="kv"><span>Stress reserve (2×)</span><b>{fmtK(maxLiab)}</b></div>
              <div className="kv"><span>Backing / unit</span><b>${backing.toFixed(2)}</b></div>
              <div style={{ marginTop: 8 }}><VaultDonut collateral={sel.coll * 1e6} exposure={liab * 1e6} /></div>
              <div style={{ marginTop: 10 }}><Bar pct={maxLiab / (sel.base * 2 * 10000) * 100} /><div className="hint">Per-market stress reserve {fmtK(maxLiab)} — one pair cannot drain the shared vault.</div></div>
            </div>
          </div>

          {sel.odds ? (
          <div className="card payoff">
            <div className="card-title">Resolution {sel.maturity} <span className="right">official result sends 1 or 0</span></div>
            <div className="yesno">
              <div className="yn yes"><span>YES · {(q.fair / sel.base * 100).toFixed(0)}% implied</span><b>$10.00 / unit</b><small>your {receive ? receive.toFixed(1) : '0'} units → {fmt$(receive * sel.base)} ({receive * sel.base - amt >= 0 ? '+' : ''}{fmt$(receive * sel.base - amt, 0)})</small></div>
              <div className="yn no"><span>NO · {(100 - q.fair / sel.base * 100).toFixed(0)}%</span><b>$0.00 / unit</b><small>your {receive ? receive.toFixed(1) : '0'} units → $0 (−{fmt$(amt, 0)})</small></div>
            </div>
            <div className="hint">Price from the {sel.source.split(' · ')[0]} book (its $1 share price is the probability). Payout here is on a $10 unit. Settlement follows the official result, not the last traded price. No funding, no leverage.</div>
          </div>
          ) : (
          <div className="card payoff">
            <div className="card-title">If the price moves <span className="right">1× · no funding · sell anytime at bid</span></div>
            <input type="range" aria-label="Simulated price move percent" min="-60" max="160" step="5" value={simRet} onChange={e => setSimRet(+e.target.value)} className="slider" />
            <div className="payoff-row">
              <span>Price <b className={simRet >= 0 ? 'up' : 'down'}>{simRet >= 0 ? '+' : ''}{simRet}%</b> → <b>{fmtFx(spot * (1 + simRet / 100))}</b></span>
              <span>Your {fmt$(amt, 0)} position → <b className={simRet >= 0 ? 'up' : 'down'}>{fmt$(amt * (1 + simRet / 100))} ({simRet >= 0 ? '+' : ''}{fmt$(amt * simRet / 100, 0)})</b></span>
            </div>
            <div className="payoff-bar"><div style={{ width: `${Math.min(100, Math.max(0, (1 + simRet / 100) / 2.6 * 100))}%` }} /></div>
            <div className="hint">Synthetic spot: fully funded 1×, no funding payments, no liquidation, no expiry. The vault reserves at a 2× stress move so closes stay solvent. Debt is out of scope (coupons).</div>
          </div>
          )}
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
          <div className="quick">{[25, 50, 75, 100].map(p => <button key={p} onClick={() => setSize(String(Math.round(balance * p / 100)))}>{p === 100 ? 'MAX' : p + '%'}</button>)}</div>
          <div className="receive">
            <span>You {buy ? 'receive' : 'send'}</span>
            <b>{fmt$(amt, 0)} <em>notional · {sel.pair}</em></b>
            <small>@ {fmtFx(spot * px / Math.max(q.fair, 0.01))}{sel.odds ? ` · odds ${(sel.base / Math.max(px, 0.01)).toFixed(2)}x` : sel.pair.includes('/') ? ` · ≈ ${(amt * spot).toLocaleString(undefined, { maximumFractionDigits: 0 })} ${sel.pair.split('/')[1]} exposure` : ''} · age {ageSecs.toFixed(1)}s</small>
          </div>
          <button className={`cta ${buy ? 'cta-buy' : 'cta-sell'}`} onClick={exec} disabled={tooOld || busy}>
            {busy ? 'SUBMITTING…' : tooOld ? 'ORACLE STALE — WAITING' : <>{buy ? '▲ BUY' : '▼ SELL'} {sel.pair} @ {fmtFx(spot * px / Math.max(q.fair, 0.01))}<small>{fmt$(amt, 0)} USDC · {live ? 'on-chain via USDC transfer' : 'paper simulation'}</small></>}
          </button>
          <div className="ticket-rows">
            <div className="kv"><span>Reserve locked (max)</span><b>{fmt$(receive * maxPay, 0)}</b></div>
            <div className="kv"><span>Vault coverage after</span><b>{coverage.toFixed(2)}×</b></div>
            <div className="kv"><span>Exit</span><b>anytime at bid</b></div>
          </div>
          <button className="adv" onClick={() => setShowAdv(a => !a)}>{showAdv ? '▾' : '▸'} Formula params · {sel.odds ? 'BinaryV1' : 'SyntheticSpotV1'}</button>
          {showAdv && (
            <div className="adv-body">
              <label>Base spread bps<input type="number" value={pol.base_spread_bps} onChange={e => setPol(p => ({ ...p, base_spread_bps: parseInt(e.target.value || '0') }))} /></label>
              <label>Skew bps<input type="number" value={pol.inventory_skew_bps} onChange={e => setPol(p => ({ ...p, inventory_skew_bps: parseInt(e.target.value || '0') }))} /></label>
              <div className="hint">Staleness +{FORMULA.staleBpsPerSec} bps/s · vol = dev÷{FORMULA.volDivisor} · hard reject &gt;{FORMULA.maxAge}s. Formulas are open — this market uses one audited kind; new kinds deploy behind the same vault.</div>
            </div>
          )}
          <div className="holdings">
            <div className="card-title">Your positions</div>
            {Object.entries(holdings).filter(([, v]) => v > 0.001).length === 0
              ? <div className="hint">None yet — your first buy lands here.</div>
              : Object.entries(holdings).filter(([, v]) => v > 0.001).map(([id, v]) => {
                  const m = markets.find(x => x.id === id);
                  return <div key={id} className="feed-row"><span><b>{m ? m.pair : id}</b></span><b>{v.toFixed(1)} · ${m ? (v * m.price).toFixed(0) : '—'}</b></div>;
                })}
          </div>
          {fills.length > 0 && <div className="holdings"><div className="card-title">Recent fills {fills[0]?.paper ? '(paper)' : ''}</div>{fills.map(f => <div key={f.id} className="feed-row"><span className={f.side === 'BUY' ? 'up' : 'down'}>{f.side} {f.tok}</span><b>{fmt$(f.usdc, 0)} @ {fmt$(f.px)}</b></div>)}</div>}
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

      <div className="movers">
        <div className="movers-head">🔥 Top movers — synthetic, listed nowhere else</div>
        <div className="movers-track">
          {gainers.slice(0, 8).map(m => {
            const c = (m.price - m.prev) / m.prev * 100;
            return <button key={m.id} className="mover" onClick={() => pick(m.id)}><b>{m.pair}</b><span>{m.name}</span><strong className={c >= 0 ? 'up' : 'down'}>{c >= 0 ? '▲ +' : '▼ '}{c.toFixed(1)}%</strong><em>{fmtFx(spotOf(m))} · vol {fmtK(m.vol24)}</em></button>;
          })}
        </div>
      </div>

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
