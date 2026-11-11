#!/usr/bin/env bash
# ZeroTick testnet deploy — FAIL-FAST. Anything that errors stops the script;
# there are no silent `|| true` seeding steps (the old v1 script called
# methods that do not exist and hid every failure).
#
# Prerequisites (all required; the script checks each one):
#   - near CLI installed and logged in (`near login` / seed phrase import)
#   - a funded testnet account to own the contract
#   - a NEP-141 USDC token id on testnet (e.g. a fake-USDC deployment you control)
#   - Rust via rustup (rust-toolchain.toml pins the channel + wasm target)
#
# Usage:
#   ACCOUNT_ID=you.testnet CONTRACT_ID=zerotick.you.testnet USDC_ID=usdc.you.testnet ./deploy-and-run.sh
#
# After deploy: markets are created via ft_transfer_call on the USDC token
# (see contracts/zerotick/src/lib.rs header for msg formats), then the owner
# enables risk per market with set_market_allocation. Run oracle-service/
# with CONTRACT_ID set to serve signed bundles, and point the frontend at it
# with VITE_ORACLE_BUNDLE_URL. This script intentionally does NOT seed fake
# markets or prices.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

: "${ACCOUNT_ID:?set ACCOUNT_ID (funded testnet account / future owner)}"
: "${CONTRACT_ID:?set CONTRACT_ID (account to deploy to)}"
: "${USDC_ID:?set USDC_ID (NEP-141 USDC token on testnet)}"
NETWORK="${NETWORK:-testnet}"
WASM="target/wasm32-unknown-unknown/release/zerotick_contract.wasm"

command -v near >/dev/null || { echo "near CLI missing: npm i -g near-cli"; exit 1; }
command -v cargo >/dev/null || { echo "cargo missing (install rustup)"; exit 1; }

echo "▸ build contract wasm"
cargo build --target wasm32-unknown-unknown --release -p zerotick-contract
test -f "$WASM" || { echo "wasm not found at $WASM"; exit 1; }

echo "▸ deploy $CONTRACT_ID on $NETWORK"
near deploy --accountId "$CONTRACT_ID" --wasmFile "$WASM" --networkId "$NETWORK"

echo "▸ init (owner=$ACCOUNT_ID usdc=$USDC_ID)"
near call "$CONTRACT_ID" new "{\"owner_id\":\"$ACCOUNT_ID\",\"usdc_token_id\":\"$USDC_ID\"}" --accountId "$ACCOUNT_ID" --networkId "$NETWORK"

echo "▸ verify"
near view "$CONTRACT_ID" get_config '{}' --networkId "$NETWORK"
near view "$CONTRACT_ID" vault_state '{}' --networkId "$NETWORK"

cat > frontend/.env <<EOF
VITE_CONTRACT_ID=$CONTRACT_ID
VITE_USDC_ID=$USDC_ID
VITE_NEAR_RPC=https://rpc.testnet.near.org
VITE_NETWORK_ID=testnet
VITE_ORACLE_BUNDLE_URL=${VITE_ORACLE_BUNDLE_URL:-}
VITE_WC_PROJECT_ID=${VITE_WC_PROJECT_ID:-}
EOF
echo "▸ frontend/.env written for $CONTRACT_ID"
echo "Next: fund the vault via ft_transfer_call deposit_lp, create markets via"
echo "ft_transfer_call create_market, enable allocation (owner), run oracle-service."
echo "UI without VITE_ORACLE_BUNDLE_URL stays in clearly-labelled paper mode."
