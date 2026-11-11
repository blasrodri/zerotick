// NEAR access for ZeroTick.
//
// Modes:
// - PAPER (default): no oracle bundle service configured. The UI simulates
//   prices/balances locally and must label everything as paper.
// - LIVE: VITE_ORACLE_BUNDLE_URL points at the oracle signer service and a
//   wallet is connected. Buys/creates go through `ft_transfer_call` on the
//   USDC token; sells/claims call the ZeroTick contract directly.
// Views always go through plain JSON-RPC so sandbox works without a wallet.

const rpc = import.meta.env.VITE_NEAR_RPC || 'https://rpc.testnet.near.org';
const inferredNetwork = /127\.0\.0\.1|localhost|:3030/.test(rpc) ? 'sandbox' : 'testnet';

export const CONFIG = {
  networkId: import.meta.env.VITE_NETWORK_ID || inferredNetwork,
  nodeUrl: rpc,
  contractId: import.meta.env.VITE_CONTRACT_ID || 'zerotick.testnet',
  usdcId: import.meta.env.VITE_USDC_ID || '',
  oracleBundleUrl: (import.meta.env.VITE_ORACLE_BUNDLE_URL || '').replace(/\/$/, ''),
  walletConnectProjectId: import.meta.env.VITE_WC_PROJECT_ID || '',
};
export const LIVE_POSSIBLE = Boolean(CONFIG.oracleBundleUrl && CONFIG.usdcId);

export const VIEW_METHODS = ['get_config', 'vault_state', 'position_of', 'claimable_of', 'lp_shares_of', 'get_formula', 'quote', 'redemption_requested'];
export const CHANGE_METHODS = ['sell', 'redeem', 'settle_market', 'request_redemption', 'withdraw_lp', 'claim'];

function b64(str) {
  return btoa(String.fromCharCode(...new TextEncoder().encode(str)));
}

/// Read-only contract view via JSON-RPC (works on sandbox + testnet).
export async function viewViaRpc(method, args = {}, contractId = CONFIG.contractId) {
  const res = await fetch(CONFIG.nodeUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      jsonrpc: '2.0', id: 'zerotick', method: 'query',
      params: { request_type: 'call_function', finality: 'final', account_id: contractId, method_name: method, args_base64: b64(JSON.stringify(args)) },
    }),
  });
  const j = await res.json();
  if (j.error) throw new Error(j.error.cause?.name || j.error.message || 'RPC error');
  const bytes = j.result?.result;
  if (!bytes) throw new Error('empty view result');
  const text = new TextDecoder().decode(new Uint8Array(bytes));
  try {
    const parsed = JSON.parse(text);
    // Contract views return a JSON *string*; unwrap one more layer.
    if (typeof parsed === 'string' && parsed.trimStart().startsWith('{')) return JSON.parse(parsed);
    return parsed;
  } catch { return text; }
}

export async function initWallet() {
  if (CONFIG.networkId === 'sandbox') {
    console.warn('[near] wallet selector does not support the local sandbox; paper mode + RPC views only');
    return null;
  }
  try {
    const { setupWalletSelector } = await import('@near-wallet-selector/core');
    const { setupModal } = await import('@near-wallet-selector/modal-ui');
    const { setupNearWallet } = await import('@near-wallet-selector/near-wallet');
    const { setupSender } = await import('@near-wallet-selector/sender');
    await import('@near-wallet-selector/modal-ui/styles.css');
    const modules = [setupNearWallet(), setupSender()];
    // WalletConnect needs a real project id; omit the module without one
    // instead of shipping a placeholder that fails at runtime.
    if (CONFIG.walletConnectProjectId && CONFIG.walletConnectProjectId !== 'zerotick-hackathon') {
      const { setupWalletConnect } = await import('@near-wallet-selector/wallet-connect');
      modules.push(setupWalletConnect({
        projectId: CONFIG.walletConnectProjectId,
        metadata: { name: 'ZeroTick', description: 'Synthetic markets on NEAR', url: location.origin, icons: [] },
      }));
    }
    const selector = await setupWalletSelector({ network: CONFIG.networkId, modules });
    const modal = setupModal(selector, { contractId: CONFIG.contractId, onHide: () => {} });
    return { selector, modal };
  } catch (e) {
    console.warn('[near] wallet selector init failed, paper mode only', e);
    return null;
  }
}

async function walletOf(selector) {
  if (!selector) throw new Error('wallet not connected');
  return selector.wallet();
}

const GAS_200T = '200000000000000';

/// Funded action (deposit_lp / buy / create_market) via the USDC token.
export async function sendFtTransferCall(selector, amountMinor, msgObj) {
  if (!CONFIG.usdcId) throw new Error('VITE_USDC_ID not configured');
  const wallet = await walletOf(selector);
  return wallet.signAndSendTransaction({
    receiverId: CONFIG.usdcId,
    actions: [{
      type: 'FunctionCall',
      params: {
        methodName: 'ft_transfer_call',
        args: { receiver_id: CONFIG.contractId, amount: String(amountMinor), memo: null, msg: JSON.stringify(msgObj) },
        gas: GAS_200T, deposit: '1',
      },
    }],
  });
}

/// Unfunded change call on the ZeroTick contract (sell/claim/etc).
export async function callMethod(selector, methodName, args) {
  const wallet = await walletOf(selector);
  return wallet.signAndSendTransaction({
    receiverId: CONFIG.contractId,
    actions: [{ type: 'FunctionCall', params: { methodName, args, gas: GAS_200T, deposit: '0' } }],
  });
}

/// Signed oracle bundle from the signer service.
export async function fetchBundle(marketId, spot) {
  if (!CONFIG.oracleBundleUrl) throw new Error('oracle bundle service not configured');
  const q = new URLSearchParams({ market: marketId, spot: String(spot) });
  const res = await fetch(`${CONFIG.oracleBundleUrl}/bundle?${q}`);
  if (!res.ok) throw new Error(`oracle service HTTP ${res.status}`);
  return res.json();
}

export async function fetchOraclePubkey() {
  if (!CONFIG.oracleBundleUrl) throw new Error('oracle bundle service not configured');
  const res = await fetch(`${CONFIG.oracleBundleUrl}/pubkey`);
  if (!res.ok) throw new Error(`oracle service HTTP ${res.status}`);
  return res.json(); // { signer, pubkey: number[32] }
}
