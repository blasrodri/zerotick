#!/usr/bin/env bash
# ZeroTick — full local stack: NEAR sandbox (Docker) + mock USDC (TEST ONLY)
# + ZeroTick contract with real custody flow + UI (paper mode + RPC views;
# browser wallets do not support the sandbox network).
# Usage: ./run-local.sh   then open http://127.0.0.1:5173
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

echo "▸ NEAR sandbox (docker: zerotick-near)"
if ! docker ps --format '{{.Names}}' | grep -qx zerotick-near; then
  docker rm -f zerotick-near >/dev/null 2>&1 || true
  docker run -d --name zerotick-near -p 3030:3030 nearprotocol/sandbox:2.13.4 >/dev/null
fi
for i in $(seq 1 30); do curl -s http://127.0.0.1:3030/status >/dev/null && break; sleep 1; done
curl -s http://127.0.0.1:3030/status >/dev/null || { echo "sandbox RPC did not come up on :3030"; exit 1; }

echo "▸ build contract wasm (zerotick + mock-usdc)"
cargo build --target wasm32-unknown-unknown --release -p zerotick-contract -p mock-usdc 2>&1 | tail -n 2

# Fresh accounts per run: sandbox state layouts differ across versions.
TS="$(date +%s)"
export CONTRACT_ID="${CONTRACT_ID:-zt${TS}.sandbox}"
export USDC_ID="${USDC_ID:-usdc${TS}.sandbox}"
export TRADER_ID="${TRADER_ID:-tr${TS}.sandbox}"
echo "▸ deploy + on-chain custody proof → contract=$CONTRACT_ID usdc(mock)=$USDC_ID"
(cd frontend && node deploy-local-v2.mjs)

cat > frontend/.env <<EOF
VITE_CONTRACT_ID=$CONTRACT_ID
VITE_USDC_ID=$USDC_ID
VITE_NEAR_RPC=http://127.0.0.1:3030
VITE_NETWORK_ID=sandbox
EOF
echo "▸ frontend → http://127.0.0.1:5173  (paper UI + live RPC views; contract $CONTRACT_ID)"
cd frontend
[ -d node_modules ] || npm install --legacy-peer-deps
npm run build 2>&1 | tail -n 2
exec npm run start
