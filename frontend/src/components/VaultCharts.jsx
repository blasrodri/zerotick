import { useEffect, useRef, useState } from 'react';

/* tiny inline sparkline for market rows */
export function Sparkline({ points, up = true, w = 72, h = 24 }) {
  if (!points || points.length < 2) return null;
  const min = Math.min(...points), max = Math.max(...points), rng = Math.max(1e-9, max - min);
  const pts = points.map((p, i) => `${(i / (points.length - 1)) * w},${(h - 3) - ((p - min) / rng) * (h - 6)}`).join(' ');
  return (
    <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} style={{ display: 'block' }}>
      <polyline points={pts} fill="none" stroke={up ? '#22ff88' : '#ff5d6c'} strokeWidth="1.6" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

/*
 * Candlestick chart — canvas, devicePixelRatio aware, crosshair with OHLC readout.
 * candles: [{o,h,l,c,v}]
 */
export function CandleChart({ candles, height = 300, onHover }) {
  const wrapRef = useRef(null);
  const canvasRef = useRef(null);
  const [hover, setHover] = useState(null);

  const draw = (hoverIdx) => {
    const el = wrapRef.current, cv = canvasRef.current;
    if (!el || !cv || !candles?.length) return;
    const W = el.clientWidth || 620, H = height;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (cv.width !== W * dpr) { cv.width = W * dpr; cv.height = H * dpr; cv.style.width = W + 'px'; cv.style.height = H + 'px'; }
    const ctx = cv.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);

    const volH = 44, padTop = 10, padRight = 58, padBottom = 8;
    const chartH = H - padTop - padBottom - volH - 10;
    const chartW = W - padRight - 8;
    let hi = -Infinity, lo = Infinity, vMax = 0;
    candles.forEach(c => { hi = Math.max(hi, c.h); lo = Math.min(lo, c.l); vMax = Math.max(vMax, c.v); });
    const pad = (hi - lo) * 0.08 || 1; hi += pad; lo -= pad;
    const y = p => padTop + (hi - p) / (hi - lo) * chartH;
    const step = chartW / candles.length;
    const bodyW = Math.max(3, Math.min(11, step * 0.62));

    // grid + price labels
    ctx.font = '10px "JetBrains Mono", monospace';
    for (let i = 0; i <= 4; i++) {
      const p = lo + (hi - lo) * i / 4, gy = y(p);
      ctx.strokeStyle = 'rgba(255,255,255,0.055)'; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(0, gy); ctx.lineTo(chartW, gy); ctx.stroke();
      ctx.fillStyle = 'rgba(154,168,179,0.95)'; ctx.fillText(p.toFixed(2), chartW + 8, gy + 3);
    }

    // volume
    candles.forEach((c, i) => {
      const x = i * step + step / 2;
      const bh = (c.v / vMax) * volH;
      ctx.fillStyle = c.c >= c.o ? 'rgba(34,255,136,0.20)' : 'rgba(255,93,108,0.20)';
      ctx.fillRect(x - bodyW / 2, H - padBottom - bh, bodyW, bh);
    });

    // candles
    candles.forEach((c, i) => {
      const x = i * step + step / 2, up = c.c >= c.o;
      const col = up ? '#22ff88' : '#ff5d6c';
      ctx.strokeStyle = col; ctx.lineWidth = 1.2;
      ctx.beginPath(); ctx.moveTo(x, y(c.h)); ctx.lineTo(x, y(c.l)); ctx.stroke();
      const top = y(Math.max(c.o, c.c)), bot = y(Math.min(c.o, c.c));
      ctx.fillStyle = col;
      ctx.fillRect(x - bodyW / 2, top, bodyW, Math.max(1.5, bot - top));
    });

    // last price line + tag
    const last = candles[candles.length - 1];
    const ly = y(last.c), upL = last.c >= last.o;
    ctx.strokeStyle = upL ? 'rgba(34,255,136,0.45)' : 'rgba(255,93,108,0.45)';
    ctx.setLineDash([4, 5]); ctx.beginPath(); ctx.moveTo(0, ly); ctx.lineTo(chartW, ly); ctx.stroke(); ctx.setLineDash([]);
    ctx.fillStyle = upL ? '#22ff88' : '#ff5d6c';
    ctx.fillRect(chartW + 4, ly - 9, 50, 18);
    ctx.fillStyle = '#041008'; ctx.font = '700 10px "JetBrains Mono", monospace';
    ctx.fillText(last.c.toFixed(2), chartW + 9, ly + 3.5);

    // crosshair
    if (hoverIdx != null && candles[hoverIdx]) {
      const c = candles[hoverIdx], x = hoverIdx * step + step / 2;
      ctx.strokeStyle = 'rgba(255,255,255,0.22)'; ctx.lineWidth = 1; ctx.setLineDash([3, 4]);
      ctx.beginPath(); ctx.moveTo(x, padTop); ctx.lineTo(x, padTop + chartH); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(0, y(c.c)); ctx.lineTo(chartW, y(c.c)); ctx.stroke();
      ctx.setLineDash([]);
      ctx.beginPath(); ctx.arc(x, y(c.c), 3.5, 0, Math.PI * 2);
      ctx.fillStyle = '#fff'; ctx.fill();
    }
  };

  useEffect(() => { draw(hover); }, [candles, height, hover]);

  const handle = (e) => {
    const el = wrapRef.current; if (!el) return;
    const rect = el.getBoundingClientRect();
    const chartW = rect.width - 58 - 8;
    const idx = Math.max(0, Math.min(candles.length - 1, Math.floor((e.clientX - rect.left) / (chartW / candles.length))));
    setHover(idx);
    onHover?.(candles[idx], idx);
  };

  return (
    <div ref={wrapRef} style={{ position: 'relative', height, cursor: 'crosshair' }}
      onMouseMove={handle} onMouseLeave={() => { setHover(null); onHover?.(null); }}>
      <canvas ref={canvasRef} />
    </div>
  );
}

export function VaultDonut({ collateral, exposure }) {
  const c = Math.max(0, collateral || 0), e = Math.min(c, Math.max(0, exposure || 0)), pct = c ? (e / c) * 100 : 0;
  const r = 30, circ = 2 * Math.PI * r, dash = (pct / 100) * circ;
  return (
    <div style={{ display: 'flex', gap: 12, alignItems: 'center' }}>
      <svg width="76" height="76" viewBox="0 0 100 100"><circle cx="50" cy="50" r="30" fill="none" stroke="#1d2b23" strokeWidth="9" /><circle cx="50" cy="50" r="30" fill="none" stroke="#22ff88" strokeWidth="9" strokeLinecap="round" strokeDasharray={`${dash} ${circ - dash}`} transform="rotate(-90 50 50)" style={{ transition: 'stroke-dasharray 360ms cubic-bezier(0.22,1,0.36,1)' }} /><text x="50" y="46" textAnchor="middle" fill="var(--text)" fontSize="13" fontFamily="var(--mono)" fontWeight="700">{pct.toFixed(1)}%</text><text x="50" y="59" textAnchor="middle" fill="var(--muted)" fontSize="8" fontFamily="var(--mono)">USED</text></svg>
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', gap: 4 }}>
        <div className="kv"><span>Collateral</span><b>${(c / 1e6).toLocaleString()}</b></div>
        <div className="kv"><span>Owed now</span><b>${(e / 1e6).toLocaleString()}</b></div>
        <div className="kv"><span>Headroom</span><b>${((c - e) / 1e6).toLocaleString()}</b></div>
      </div>
    </div>
  );
}

export function Bar({ pct, color = '#22ff88' }) {
  return (
    <div style={{ height: 8, borderRadius: 99, background: '#141b22', border: '1px solid rgba(255,255,255,0.07)', overflow: 'hidden' }}>
      <div style={{ height: '100%', width: `${Math.min(100, Math.max(0, pct))}%`, background: `linear-gradient(90deg, ${color}55, ${color})`, borderRadius: 99, transition: 'width 500ms cubic-bezier(0.22,1,0.36,1)' }} />
    </div>
  );
}
