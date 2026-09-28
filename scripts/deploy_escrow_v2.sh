#!/usr/bin/env bash
# Deploy ONLY PaymentEscrow v2 to Stellar testnet, beside the existing
# contracts. It reuses the testnet AgentRegistry and asset SAC already in
# addresses.json and deploys nothing else. v1's `payment_escrow` id stays in
# the address book as history; v2 is recorded as `payment_escrow_v2`.
#
# Usage:
#   make deploy-escrow-v2 SETTLER=G...            (or)
#   SETTLER=G... bash scripts/deploy_escrow_v2.sh
#
# Environment:
#   SETTLER   required. The account the backend signs `settle` with.
#   SOURCE    stellar-cli identity that pays for and signs the deploy
#             (default: admin).
#   ADMIN     the escrow admin, the only address that can rotate the
#             settler (default: SOURCE's address).
#   REGISTRY  AgentRegistry id (default: agent_registry in addresses.json).
#   ASSET_SAC asset contract id (default: asset_sac in addresses.json).

set -euo pipefail
cd "$(dirname "$0")/.."

if [ "${NETWORK:-testnet}" != "testnet" ]; then
  echo "✗ escrow v2 deploys to testnet only (NETWORK=$NETWORK)" >&2
  exit 1
fi
NETWORK="testnet"
ADDR_FILE="addresses.json"
SOURCE="${SOURCE:-admin}"

if [ -z "${SETTLER:-}" ]; then
  echo "✗ SETTLER is required, e.g.  make deploy-escrow-v2 SETTLER=G..." >&2
  exit 1
fi

is_account() { [[ "$1" =~ ^G[A-Z2-7]{55}$ ]]; }
is_contract() { [[ "$1" =~ ^C[A-Z2-7]{55}$ ]]; }

book() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], ""))' \
    "$ADDR_FILE" "$1"
}

ADMIN="${ADMIN:-$(stellar keys address "$SOURCE")}"
REGISTRY="${REGISTRY:-$(book agent_registry)}"
ASSET_SAC="${ASSET_SAC:-$(book asset_sac)}"

is_account "$SETTLER" || is_contract "$SETTLER" || { echo "✗ SETTLER is not an address: $SETTLER" >&2; exit 1; }
is_account "$ADMIN" || is_contract "$ADMIN" || { echo "✗ ADMIN is not an address: $ADMIN" >&2; exit 1; }
is_contract "$REGISTRY" || { echo "✗ no AgentRegistry id (REGISTRY or $ADDR_FILE): '$REGISTRY'" >&2; exit 1; }
is_contract "$ASSET_SAC" || { echo "✗ no asset SAC id (ASSET_SAC or $ADDR_FILE): '$ASSET_SAC'" >&2; exit 1; }

echo "→ network:  $NETWORK"
echo "→ source:   $SOURCE ($(stellar keys address "$SOURCE"))"
echo "→ admin:    $ADMIN"
echo "→ settler:  $SETTLER"
echo "→ registry: $REGISTRY"
echo "→ asset:    $ASSET_SAC"

echo "→ building the escrow wasm only"
stellar contract build --package orizon-payment-escrow

WASM_DIR="target/wasm32v1-none/release"
[ -d "$WASM_DIR" ] || WASM_DIR="target/wasm32-unknown-unknown/release"
ESC_WASM="$WASM_DIR/orizon_payment_escrow.wasm"
[ -f "$ESC_WASM" ] || { echo "✗ missing $ESC_WASM" >&2; exit 1; }

echo "→ deploying PaymentEscrow v2"
DEPLOY_OUT="$(stellar contract deploy \
  --source "$SOURCE" \
  --network "$NETWORK" \
  --wasm "$ESC_WASM" \
  -- \
  --admin "$ADMIN" \
  --usdc "$ASSET_SAC" \
  --registry "$REGISTRY" \
  --settler "$SETTLER" 2>&1)" || { echo "$DEPLOY_OUT" >&2; echo "✗ deploy failed" >&2; exit 1; }
ESC_ID="$(tail -n1 <<<"$DEPLOY_OUT")"
# A dropped RPC submission makes the CLI print an error instead of an id.
# Abort rather than write garbage into the address book (the tx may still
# land late; check the source account on the explorer before re-running).
if ! is_contract "$ESC_ID"; then
  echo "✗ deploy did not return a contract id: $ESC_ID" >&2
  exit 1
fi
echo "  PaymentEscrow v2: $ESC_ID"

# Read-only checks (simulated, never submitted).
view() {
  stellar contract invoke --id "$ESC_ID" --network "$NETWORK" \
    --source-account "$SOURCE" --send=no -- "$@" | tail -n1 | tr -d '"'
}
GOT_VERSION="$(view version)"
GOT_SETTLER="$(view settler)"
GOT_ADMIN="$(view admin)"
echo "  version=$GOT_VERSION settler=$GOT_SETTLER admin=$GOT_ADMIN"
[ "$GOT_VERSION" = "2" ] || { echo "✗ version is $GOT_VERSION, expected 2" >&2; exit 1; }
[ "$GOT_SETTLER" = "$SETTLER" ] || { echo "✗ settler mismatch" >&2; exit 1; }
[ "$GOT_ADMIN" = "$ADMIN" ] || { echo "✗ admin mismatch" >&2; exit 1; }

python3 - "$ADDR_FILE" "$ESC_ID" "$SETTLER" "$ADMIN" <<'PY'
import json, sys
path, esc, settler, admin = sys.argv[1:]
book = json.load(open(path))
book["payment_escrow_v2"] = esc
book["payment_escrow_v2_settler"] = settler
book["payment_escrow_v2_admin"] = admin
with open(path, "w") as f:
    json.dump(book, f, indent=2)
    f.write("\n")
PY

echo
echo "✓ deployed. $ADDR_FILE now records payment_escrow_v2 beside v1:"
cat "$ADDR_FILE"
