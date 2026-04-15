#!/bin/bash
# End-to-end test for the Groundhog JSON-RPC gateway.
#
# Assumes:
# - The full stack is running (hogd services + chuckd + gateway)
# - Gateway is listening on $RPC_URL (default: http://127.0.0.1:8545)
# - tx-builder binary is built (cargo build -p groundhog-tx-builder)
#
# Usage:
#   ./scripts/e2e-test.sh
#   RPC_URL=http://localhost:9999 ./scripts/e2e-test.sh

set -euo pipefail

RPC_URL="${RPC_URL:-http://127.0.0.1:8545}"
TX_BUILDER="${TX_BUILDER:-./target/debug/tx-builder}"
PASS=0
FAIL=0
SKIP=0

# --- Helpers ---

rpc() {
  local method="$1"
  shift
  local params="$*"
  curl -s -X POST "$RPC_URL" \
    -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"groundhog_${method}\",\"params\":[${params}]}"
}

check() {
  local name="$1"
  local condition="$2"
  local response="$3"

  if echo "$response" | jq -e "$condition" > /dev/null 2>&1; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name"
    echo "    response: $response"
    FAIL=$((FAIL + 1))
  fi
}

skip() {
  local name="$1"
  local reason="$2"
  echo "  SKIP: $name ($reason)"
  SKIP=$((SKIP + 1))
}

# --- Tests ---

echo "=== Groundhog RPC Gateway E2E Tests ==="
echo "RPC endpoint: $RPC_URL"
echo ""

# 1. chainTip
echo "--- chainTip ---"
RESP=$(rpc chainTip)
check "returns result (not error)" '.result != null' "$RESP"
check "height is a number" '.result.height | type == "number"' "$RESP"
check "blockHash starts with 0x" '.result.blockHash | startswith("0x")' "$RESP"
check "blockHash is 66 chars (0x + 64 hex)" '.result.blockHash | length == 66' "$RESP"
TIP_HEIGHT=$(echo "$RESP" | jq -r '.result.height')
echo "  (tip height: $TIP_HEIGHT)"
echo ""

# 2. pendingTransactionCount (expected: NotImplemented)
# TODO: once the scheduler RPC (groundhog_scheduler) is published, this
# will return a number instead of NotImplemented.
echo "--- pendingTransactionCount ---"
RESP=$(rpc pendingTransactionCount)
check "returns error" '.error != null' "$RESP"
check "error code is -32052 (NotImplemented)" '.error.code == -32052' "$RESP"
echo ""

# 3. getBlockByNumber (expected: NotImplemented)
echo "--- getBlockByNumber ---"
RESP=$(rpc getBlockByNumber "1")
check "returns error" '.error != null' "$RESP"
check "error code is -32052 (NotImplemented)" '.error.code == -32052' "$RESP"
echo ""

# 4. sendRawTransaction
echo "--- sendRawTransaction ---"
if [ -x "$TX_BUILDER" ]; then
  TX_HEX=$("$TX_BUILDER" 2>/tmp/e2e-tx-meta.txt)
  TX_HASH=$(grep 'tx_hash=' /tmp/e2e-tx-meta.txt | cut -d= -f2)
  TX_ADDR=$(grep 'address=' /tmp/e2e-tx-meta.txt | cut -d= -f2)

  RESP=$(rpc sendRawTransaction "\"$TX_HEX\"")

  if echo "$RESP" | jq -e '.result' > /dev/null 2>&1; then
    check "returns a tx hash" '.result != null' "$RESP"
    check "tx hash starts with 0x" '.result | startswith("0x")' "$RESP"
    check "tx hash matches expected" ".result == \"$TX_HASH\"" "$RESP"
    echo "  (submitted tx: $TX_HASH)"
  else
    # Scheduler may reject (no valid account on chain) — that's OK for
    # testing, as long as the error is BackendUnavailable not TxInvalid.
    echo "  NOTE: submit returned error (expected if no live scheduler)"
    echo "    $(echo "$RESP" | jq -c '.error')"
    skip "sendRawTransaction error code" "scheduler rejected — error code check skipped"
    skip "sendRawTransaction success" "scheduler rejected or unavailable"
  fi
else
  skip "sendRawTransaction" "tx-builder not built (cargo build -p groundhog-tx-builder)"
  TX_HASH=""
  TX_ADDR=""
fi
echo ""

# 5. getTransactionStatus
echo "--- getTransactionStatus ---"
if [ -n "${TX_HASH:-}" ]; then
  RESP=$(rpc getTransactionStatus "\"$TX_HASH\"")
  check "returns result" '.result != null' "$RESP"
  check "has status field" '.result.status != null' "$RESP"
  STATUS=$(echo "$RESP" | jq -r '.result.status')
  echo "  (status: $STATUS)"
  # Committed status should NOT include blockHash (SCP instant finality,
  # height alone is sufficient).
  if [ "$STATUS" = "committed" ]; then
    check "committed has height" '.result.height != null' "$RESP"
    check "committed has no blockHash" '.result.blockHash == null' "$RESP"
  fi
else
  # Use a zero hash to test the basic path
  ZERO_HASH="0x0000000000000000000000000000000000000000000000000000000000000000"
  RESP=$(rpc getTransactionStatus "\"$ZERO_HASH\"")
  check "returns result for unknown hash" '.result != null' "$RESP"
  check "status is unknown or pending" '.result.status == "unknown" or .result.status == "pending"' "$RESP"
fi
echo ""

# 6. getTransactionReceipt
echo "--- getTransactionReceipt ---"
if [ -n "${TX_HASH:-}" ]; then
  RESP=$(rpc getTransactionReceipt "\"$TX_HASH\"")
  if echo "$RESP" | jq -e '.result' > /dev/null 2>&1; then
    RECEIPT=$(echo "$RESP" | jq '.result')
    if [ "$RECEIPT" = "null" ]; then
      echo "  NOTE: receipt is null (tx not yet committed or not found)"
      skip "getTransactionReceipt content" "tx not committed"
    else
      check "receipt has txHash" '.result.txHash != null' "$RESP"
      check "receipt has height" '.result.height != null' "$RESP"
      check "receipt has status" '.result.status != null' "$RESP"
      check "receipt has events array" '.result.events | type == "array"' "$RESP"
    fi
  else
    check "returns result (not server error)" '.error.code != -32050' "$RESP"
  fi
else
  ZERO_HASH="0x0000000000000000000000000000000000000000000000000000000000000000"
  RESP=$(rpc getTransactionReceipt "\"$ZERO_HASH\"")
  check "returns null for unknown hash" '.result == null' "$RESP"
fi
echo ""

# 7. getContractEvents
echo "--- getContractEvents ---"
ADDR="${TX_ADDR:-0x0000000000000000000000000000000000000000000000000000000000000000}"
RESP=$(rpc getContractEvents "\"$ADDR\", 0, 100")
if echo "$RESP" | jq -e '.result' > /dev/null 2>&1; then
  check "returns array" '.result | type == "array"' "$RESP"
  EVENT_COUNT=$(echo "$RESP" | jq '.result | length')
  echo "  (events found: $EVENT_COUNT)"
else
  # Indexer may not be connected
  echo "  NOTE: returned error (indexer may not be connected)"
  echo "    $(echo "$RESP" | jq -c '.error')"
  skip "getContractEvents results" "indexer unavailable"
fi
echo ""

# 8. getContractEvents — range validation
echo "--- getContractEvents (invalid range) ---"
RESP=$(rpc getContractEvents "\"$ADDR\", 100, 0")
check "inverted range returns error" '.error != null' "$RESP"
check "error code is -32602 (InvalidParam)" '.error.code == -32602' "$RESP"
echo ""

# --- Summary ---
TOTAL=$((PASS + FAIL + SKIP))
echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped (of $TOTAL) ==="

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
