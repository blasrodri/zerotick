import React, { Suspense, lazy } from 'react';
import ReactDOM from 'react-dom/client';
import './index.css';

const App = lazy(() => import('./App.jsx'));

function Fallback() {
  return <div style={{ padding: 16, fontFamily: 'var(--mono)', color: 'var(--muted)' }}>zero&nbsp;tick&nbsp;loading…</div>;
}

class ErrorBoundary extends React.Component {
  state = { err: null };
  static getDerivedStateFromError(err) { return { err }; }
  componentDidCatch(err, info) { console.error('[ErrorBoundary]', err, info); }
  render() {
    if (this.state.err) return <div style={{ padding: 16, fontFamily: 'var(--mono)', fontSize: 12, color: '#ff8b8b', background: '#1a1212', border: '1px solid #3a1f1f', margin: 16, borderRadius: 8, whiteSpace: 'pre-wrap' }}>Error: {String(this.state.err)}<br/><br/>Check console (F12) — wallet selector may need network. Try a hard reload (Cmd+Shift+R).</div>;
    return this.props.children;
  }
}

ReactDOM.createRoot(document.getElementById('root')).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Suspense fallback={<Fallback />}>
        <App />
      </Suspense>
    </ErrorBoundary>
  </React.StrictMode>
);
