#!/usr/bin/env bash
# Real local-DX smoke test.
#
# Exercises the user-facing CLI path against a real hog-devnode binary:
#   groundhog new -> build -> persistent node -> dev-0 deploy/call/view/storage
#   -> dev-0/dev-1 transfer with exact raw balance deltas
#   -> key generate -> wallet create -> saved-wallet deploy
#   -> saved-wallet airdrop -> second deploy/call
#   -> stop -> restart -> verify persisted contract state
#
# Required environment:
#   GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode
#
# Optional environment:
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>  # intro token only; XDR still validates shape
#   SMOKE_STANDALONE_GATEWAY=1  # route view/storage checks through hog-gateway
#   SMOKE_SDK_PATH=/path/to/sdk_rs/crates/sdk  # test scaffold against local SDK
#   SMOKE_PROTOCOL_RS_PATH=/path/to/protocol-rs  # defaults to sdk_rs sibling
#   KEEP_WORKDIR=1
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLI="$REPO_ROOT/target/debug/groundhog"
GATEWAY="$REPO_ROOT/target/debug/hog-gateway"
PROJECT_NAME="smoke-counter"
SMOKE_PROFILE="local"
INITIAL_VALUE=7
INCREMENT_AMOUNT=5

log() {
    printf '==> %s\n' "$*" >&2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

json_field() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

path = sys.argv[1].split(".")
payload = json.loads(sys.argv[2])
value = payload
for part in path:
    if value is None:
        break
    value = value.get(part) if isinstance(value, dict) else value[int(part)]
if value is None:
    print("null")
elif isinstance(value, (dict, list)):
    print(json.dumps(value, sort_keys=True, separators=(",", ":")))
else:
    print(value)
PY
}

json_len() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

path = sys.argv[1].split(".")
payload = json.loads(sys.argv[2])
value = payload
for part in path:
    if value is None:
        break
    value = value.get(part) if isinstance(value, dict) else value[int(part)]
print(len(value) if value is not None else 0)
PY
}

assert_transfer_balance_deltas() {
    python3 - "$@" <<'PY'
import sys

labels = (
    "sender before",
    "sender after",
    "recipient before",
    "recipient after",
    "raw amount",
    "transaction cost",
)
values = []
for label, raw in zip(labels, sys.argv[1:], strict=True):
    if not raw.isascii() or not raw.isdecimal():
        raise SystemExit(f"{label} is not a non-negative decimal integer: {raw!r}")
    values.append(int(raw))

sender_before, sender_after, recipient_before, recipient_after, raw_amount, transaction_cost = values
sender_delta = sender_before - sender_after
recipient_delta = recipient_after - recipient_before
if recipient_delta != raw_amount or sender_delta != raw_amount + transaction_cost:
    raise SystemExit(
        "transfer balance delta mismatch: "
        f"sender={sender_before}->{sender_after} (delta {sender_delta}), "
        f"recipient={recipient_before}->{recipient_after} (delta {recipient_delta}), "
        f"expected recipient delta={raw_amount} and "
        f"sender delta={raw_amount}+{transaction_cost}"
    )
print(sender_delta - raw_amount)
PY
}

assert_exact_balance_increase() {
    python3 - "$@" <<'PY'
import sys

before, after, expected = (int(value) for value in sys.argv[1:])
if before < 0 or after < 0 or expected < 0 or after - before != expected:
    raise SystemExit(
        f"balance increase mismatch: {before}->{after} "
        f"(delta {after - before}), expected={expected}"
    )
PY
}

toml_field() {
    python3 - "$1" "$2" <<'PY'
import sys
import tomllib

path, dotted = sys.argv[1:]
with open(path, "rb") as handle:
    value = tomllib.load(handle)
for part in dotted.split("."):
    value = value.get(part) if isinstance(value, dict) else None
    if value is None:
        break
print("null" if value is None else value)
PY
}

toml_profile_field() {
    python3 - "$1" "$2" "$3" <<'PY'
import sys
import tomllib

path, profile_name, field = sys.argv[1:]
with open(path, "rb") as handle:
    payload = tomllib.load(handle)
value = payload.get("profile", {}).get(profile_name, {}).get(field)
print("null" if value is None else value)
PY
}

counter_value_key() {
    python3 - <<'PY'
import hashlib

def cxs_u32(value):
    if value <= 62:
        return bytes([value])
    if value <= 0xFF:
        return bytes([0x3F, value])
    if value <= 0xFFFF:
        return bytes([0x7F]) + value.to_bytes(2, "little")
    return bytes([0xBF]) + value.to_bytes(4, "little")

def cxs_string(value):
    raw = value.encode("utf-8")
    return cxs_u32(len(raw)) + raw

def child(preimage, value_bytes):
    return preimage + cxs_u32(len(value_bytes)) + value_bytes

# sdk::storage roots contract state under the storage struct's domain, then
# assigns fields by u32 slot. Counter.value is the first field:
#   Keypath::root().child("Counter").child(&0u32).generate_key()
preimage = b""
preimage = child(preimage, cxs_string("Counter"))
preimage = child(preimage, cxs_u32(0))
print("0x" + hashlib.sha256(preimage).hexdigest())
PY
}

wait_for_node_json() {
    local path="$1"
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if [[ -s "$path" ]]; then
            python3 - "$path" <<'PY' >/dev/null 2>&1 && return 0
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    line = handle.readline()
json.loads(line)
PY
        fi
        sleep 0.2
    done
    return 1
}

free_tcp_port() {
    python3 - <<'PY'
import socket

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

wait_for_gateway_rpc() {
    local url="$1"
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if python3 - "$url" <<'PY' >/dev/null 2>&1; then
import json
import sys
import urllib.request

url = sys.argv[1]
payload = json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "groundhog_version",
    "params": [],
}).encode()
request = urllib.request.Request(
    url,
    data=payload,
    headers={"content-type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(request, timeout=1) as response:
    data = json.loads(response.read().decode())
if data.get("error"):
    raise SystemExit(1)
PY
            return 0
        fi
        sleep 0.2
    done
    return 1
}

find_hogd_dev_binary() {
    if [[ -n "${GROUNDHOG_DEVNODE_BINARY:-}" ]]; then
        printf '%s\n' "$GROUNDHOG_DEVNODE_BINARY"
        return
    fi

    die "set GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode"
}

detect_hogd_protocol() {
    local binary="$1"
    if [[ -n "${HOGD_PROTOCOL_OVERRIDE:-}" ]]; then
        printf '%s\n' "$HOGD_PROTOCOL_OVERRIDE"
        return
    fi

    local version_file
    version_file="$(dirname "$binary")/../version.c"
    if [[ -f "$version_file" ]]; then
        local protocol
        protocol="$(python3 - "$version_file" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r'protocol_commit\[\]\s*=\s*"([0-9a-fA-F]+)"', text)
if match:
    print(f"hogp/{match.group(1)[:12]}")
PY
)"
        if [[ -n "$protocol" ]]; then
            printf '%s\n' "$protocol"
            return
        fi
    fi

    # Older devnode builds printed protocol metadata for --help.
    # Strip ANSI color escapes before extracting the protocol marker.
    local output
    output="$("$binary" --help 2>&1 || true)"
    python3 -c 'import re, sys
text = sys.stdin.read()
text = re.sub(r"\x1b\[[0-9;]*m", "", text)
matches = re.findall(r"hogd protocol: (hogp/[A-Za-z0-9]+)", text)
if matches:
    print(matches[-1])
' <<< "$output"
}

update_rpc_url() {
    local config="$1"
    local profile="$2"
    local rpc_url="$3"
    python3 - "$config" "$profile" "$rpc_url" <<'PY'
import sys

path, profile, rpc_url = sys.argv[1:]
with open(path, "r", encoding="utf-8") as handle:
    text = handle.read()
lines = []
replaced = False
in_profile = False
profile_found = False
for line in text.splitlines():
    stripped = line.strip()
    if stripped.startswith("[profile."):
        if in_profile and not replaced:
            lines.append(f'rpc_url = "{rpc_url}"')
            replaced = True
        in_profile = stripped == f"[profile.{profile}]"
        profile_found = profile_found or in_profile
    if in_profile and stripped.startswith("rpc_url"):
        lines.append(f'rpc_url = "{rpc_url}"')
        replaced = True
    else:
        lines.append(line)
if in_profile and not replaced:
    lines.append(f'rpc_url = "{rpc_url}"')
    replaced = True
if not profile_found:
    raise SystemExit(f"profile.{profile} section not found")
if not replaced:
    raise SystemExit(f"profile.{profile} rpc_url was not updated")
with open(path, "w", encoding="utf-8") as handle:
    handle.write("\n".join(lines) + "\n")
PY
}

override_sdk_path() {
    local manifest="$1"
    local sdk_path="$2"
    local protocol_path="$3"
    [[ -d "$sdk_path" ]] || die "SMOKE_SDK_PATH is not a directory: $sdk_path"
    [[ -d "$protocol_path" ]] \
        || die "SMOKE_PROTOCOL_RS_PATH is not a directory: $protocol_path"

    python3 - "$manifest" "$sdk_path" "$protocol_path" <<'PY'
import json
import pathlib
import sys

manifest = pathlib.Path(sys.argv[1])
sdk_path = pathlib.Path(sys.argv[2]).resolve()
protocol_path = pathlib.Path(sys.argv[3]).resolve()
replacement = f'sdk = {{ path = {json.dumps(str(sdk_path))} }}'
lines = []
replaced = False
for line in manifest.read_text(encoding="utf-8").splitlines():
    if line.startswith("sdk = "):
        lines.append(replacement)
        replaced = True
    else:
        lines.append(line)
if not replaced:
    raise SystemExit("sdk dependency line not found")
patches = {
    "groundhog-spec": "groundhog-spec",
    "groundhog-trace": "groundhog-trace",
    "groundhog-xdr": "groundhog-xdr",
    "groundhog-syscall": "groundhog-syscall",
    "hogexec-internal": "hogexec-internal",
}
lines.extend(["", '[patch."sparse+https://groundhog.dev/crates/api/v1/crates/"]'])
for package, directory in patches.items():
    path = protocol_path / directory
    if not path.is_dir():
        raise SystemExit(f"protocol crate path not found: {path}")
    lines.append(f'{package} = {{ path = {json.dumps(str(path))} }}')
manifest.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
}

run_json() {
    local label="$1"
    shift
    log "$label: $(display_command "$@")"
    local output
    if ! output="$("$@" 2> >(sed "s/^/[$label stderr] /" >&2))"; then
        printf '%s\n' "$output" >&2
        die "$label failed"
    fi
    printf '%s\n' "$output"
}

run_view_json() {
    local label="$1"
    shift
    if [[ -n "${VIEW_FROM:-}" ]]; then
        run_json "$label" "$CLI" --json view --from "$VIEW_FROM" "$@"
    else
        run_json "$label" "$CLI" --json view "$@"
    fi
}

rpc_hash_query() {
    local url="$1"
    local method="$2"
    local tx_hash="$3"
    python3 - "$url" "$method" "$tx_hash" <<'PY'
import json
import sys
import urllib.request

url, method, tx_hash = sys.argv[1:]
payload = json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": method,
    "params": [tx_hash],
}).encode()
request = urllib.request.Request(
    url,
    data=payload,
    headers={"content-type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(request, timeout=5) as response:
    print(response.read().decode())
PY
}

assert_transaction_committed() {
    local label="$1"
    local url="$2"
    local tx_hash="$3"
    local expected_height="$4"
    local receipt status
    receipt="$(rpc_hash_query "$url" groundhog_getTransactionReceipt "$tx_hash")"
    [[ "$(json_field result.txHash "$receipt")" == "$tx_hash" ]] \
        || die "$label receipt hash did not match $tx_hash: $receipt"
    [[ "$(json_field result.height "$receipt")" == "$expected_height" ]] \
        || die "$label receipt height did not match $expected_height: $receipt"

    status="$(rpc_hash_query "$url" groundhog_getTransactionStatus "$tx_hash")"
    [[ "$(json_field result.status "$status")" == "committed" ]] \
        || die "$label transaction status was not committed: $status"
    [[ "$(json_field result.height "$status")" == "$expected_height" ]] \
        || die "$label committed height did not match $expected_height: $status"
    log "$label receipt/status agree: tx=$tx_hash height=$expected_height"
}

display_command() {
    local parts=("$@")
    if [[ "${parts[0]}" == "$CLI" ]]; then
        parts[0]="groundhog"
    fi
    printf '%q ' "${parts[@]}"
}

assert_call_succeeded() {
    local label="$1"
    local payload="$2"
    local status_summary
    status_summary="$(json_field data.status_summary "$payload")"
    if [[ "$status_summary" == "succeeded" ]]; then
        return
    fi

    local tx_hash wallet_entrypoint wallet_entrypoint_source target_entrypoint detail_available
    tx_hash="$(json_field data.tx_hash "$payload")"
    wallet_entrypoint="$(json_field data.diagnostics.wallet.entrypoint_id "$payload")"
    wallet_entrypoint_source="$(json_field data.diagnostics.wallet.entrypoint_source "$payload")"
    target_entrypoint="$(json_field data.diagnostics.target.entrypoint_id "$payload")"
    detail_available="$(json_field data.diagnostics.transaction_detail.available "$payload")"
    die "$label status was $status_summary (tx=$tx_hash wallet_entrypoint=$wallet_entrypoint \
wallet_entrypoint_source=$wallet_entrypoint_source target_entrypoint=$target_entrypoint \
transaction_detail_available=$detail_available)"
}

collect_descendants() {
    local pid="$1"
    local child
    pgrep -P "$pid" 2>/dev/null | while read -r child; do
        printf '%s\n' "$child"
        collect_descendants "$child"
    done
}

stop_node() {
    local pid="$1"
    [[ -n "$pid" ]] || return 0
    if ! kill -0 "$pid" >/dev/null 2>&1; then
        wait "$pid" >/dev/null 2>&1 || true
        return 0
    fi

    kill -TERM "$pid" >/dev/null 2>&1 || true
    local deadline=$((SECONDS + 40))
    while kill -0 "$pid" >/dev/null 2>&1; do
        if (( SECONDS >= deadline )); then
            kill -KILL "$pid" >/dev/null 2>&1 || true
            wait "$pid" >/dev/null 2>&1 || true
            return 1
        fi
        sleep 0.2
    done
    wait "$pid" >/dev/null 2>&1
}

DEVNODE_BINARY="$(find_hogd_dev_binary)"
[[ -x "$DEVNODE_BINARY" ]] || die "hog-devnode binary is not executable: $DEVNODE_BINARY"

HOGD_PROTOCOL="$(detect_hogd_protocol "$DEVNODE_BINARY")"

if [[ "${SMOKE_STANDALONE_GATEWAY:-0}" == "1" ]]; then
    log "Building CLI and standalone gateway"
    cargo build -p groundhog-cli -p groundhog-json-rpc-gateway --manifest-path "$REPO_ROOT/Cargo.toml"
else
    log "Building CLI"
    cargo build -p groundhog-cli --manifest-path "$REPO_ROOT/Cargo.toml"
fi

WORK="$(mktemp -d)"
export XDG_CONFIG_HOME="$WORK/config"
export XDG_CACHE_HOME="$WORK/cache"
NODE_DATA_DIR="$WORK/node-data"
NODE_ARGS=(--json node --port 0 --hog-devnode-binary "$DEVNODE_BINARY" --data-dir "$NODE_DATA_DIR")
if [[ -n "$HOGD_PROTOCOL" ]]; then
    NODE_ARGS+=(--hogd-protocol "$HOGD_PROTOCOL")
    log "Using hogd protocol override: $HOGD_PROTOCOL"
fi
NODE_PID=""
NODE_DESCENDANTS=""
GATEWAY_PID=""
cleanup() {
    if [[ -n "$GATEWAY_PID" ]] && kill -0 "$GATEWAY_PID" >/dev/null 2>&1; then
        kill "$GATEWAY_PID" >/dev/null 2>&1 || true
        wait "$GATEWAY_PID" >/dev/null 2>&1 || true
    fi
    local descendants="$NODE_DESCENDANTS"
    if [[ -n "$NODE_PID" ]] && kill -0 "$NODE_PID" >/dev/null 2>&1; then
        descendants="$(collect_descendants "$NODE_PID" || true)"
        NODE_DESCENDANTS="$descendants"
        stop_node "$NODE_PID" || true
        sleep 0.5
    fi
    if [[ -n "$descendants" ]]; then
        # shellcheck disable=SC2086
        kill $descendants >/dev/null 2>&1 || true
        sleep 0.2
        # shellcheck disable=SC2086
        kill -9 $descendants >/dev/null 2>&1 || true
    fi
    if [[ "${KEEP_WORKDIR:-0}" == "1" ]]; then
        log "Keeping workdir: $WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

log "Workdir: $WORK"
cd "$WORK"

log "Scaffolding counter contract"
"$CLI" new "$PROJECT_NAME" --template counter
cd "$PROJECT_NAME"
export GROUNDHOG_PROFILE="$SMOKE_PROFILE"

if [[ -n "${SMOKE_SDK_PATH:-}" ]]; then
    log "Using local SDK path: $SMOKE_SDK_PATH"
    SDK_REPO_ROOT="$(cd "$SMOKE_SDK_PATH/../.." && pwd)"
    SMOKE_PROTOCOL_RS_PATH="${SMOKE_PROTOCOL_RS_PATH:-$(dirname "$SDK_REPO_ROOT")/protocol-rs}"
    log "Using local protocol-rs path: $SMOKE_PROTOCOL_RS_PATH"
    override_sdk_path Cargo.toml "$SMOKE_SDK_PATH" "$SMOKE_PROTOCOL_RS_PATH"
fi

log "Building contract WASM"
"$CLI" build

log "Starting local node"
"$CLI" "${NODE_ARGS[@]}" > "$WORK/node.json" 2> "$WORK/node.err" &
NODE_PID=$!
if ! wait_for_node_json "$WORK/node.json"; then
    cat "$WORK/node.err" >&2 || true
    die "local node did not emit startup JSON"
fi
NODE_JSON="$(head -n 1 "$WORK/node.json")"
NODE_DESCENDANTS="$(collect_descendants "$NODE_PID" || true)"
RPC_URL="$(json_field data.rpc_url "$NODE_JSON")"
READ_PLANE="$(json_field data.backend.planes.read "$NODE_JSON")"
VIEW_PLANE="$(json_field data.backend.planes.view "$NODE_JSON")"
RUNTIME_MANIFEST="$(json_field data.runtime_manifest.path "$NODE_JSON")"
CHAIN_STATE="$(json_field data.chain_state "$NODE_JSON")"
REPORTED_DATA_DIR="$(json_field data.data_dir "$NODE_JSON")"
log "Node RPC: $RPC_URL (read plane: $READ_PLANE, view plane: $VIEW_PLANE)"
[[ "$REPORTED_DATA_DIR" == "$NODE_DATA_DIR" ]] \
    || die "node data_dir was $REPORTED_DATA_DIR, expected $NODE_DATA_DIR"
[[ "$CHAIN_STATE" == "fresh" ]] \
    || die "initial node chain_state was $CHAIN_STATE, expected fresh"
[[ "$VIEW_PLANE" == "view_gateway" ]] || die "node view plane was $VIEW_PLANE, expected view_gateway"
[[ "$RUNTIME_MANIFEST" != "null" && -n "$RUNTIME_MANIFEST" ]] \
    || die "groundhog node did not report a runtime manifest path"
[[ -r "$RUNTIME_MANIFEST" ]] || die "runtime manifest is not readable: $RUNTIME_MANIFEST"
NODE_PROFILE="$(toml_field groundhog.toml default_profile)"
[[ "$NODE_PROFILE" == "$SMOKE_PROFILE" ]] \
    || die "default_profile was $NODE_PROFILE, expected $SMOKE_PROFILE"
PROFILE_RPC_URL="$(toml_profile_field groundhog.toml "$NODE_PROFILE" rpc_url)"
PROFILE_RUNTIME_MANIFEST="$(toml_profile_field groundhog.toml "$NODE_PROFILE" runtime_manifest)"
[[ "$PROFILE_RPC_URL" == "null" ]] \
    || die "project profile.$NODE_PROFILE.rpc_url was dirtied: $PROFILE_RPC_URL"
[[ "$PROFILE_RUNTIME_MANIFEST" == "null" ]] \
    || die "project profile.$NODE_PROFILE.runtime_manifest was dirtied: $PROFILE_RUNTIME_MANIFEST"
GLOBAL_CONFIG="$XDG_CONFIG_HOME/groundhog/config.toml"
GLOBAL_RPC_URL="$(toml_field "$GLOBAL_CONFIG" local_runtime_state.rpc_url)"
GLOBAL_RUNTIME_MANIFEST="$(toml_field "$GLOBAL_CONFIG" local_runtime_state.runtime_manifest)"
[[ "$GLOBAL_RPC_URL" == "$RPC_URL" ]] \
    || die "local_runtime_state.rpc_url was $GLOBAL_RPC_URL, expected $RPC_URL"
[[ "$GLOBAL_RUNTIME_MANIFEST" == "$RUNTIME_MANIFEST" ]] \
    || die "local_runtime_state.runtime_manifest was $GLOBAL_RUNTIME_MANIFEST, expected $RUNTIME_MANIFEST"
unset GROUNDHOG_RUNTIME_MANIFEST

TRANSFER_SENDER="dev-0"
TRANSFER_RECIPIENT="dev-1"
TRANSFER_AMOUNT="0.0000001"
TRANSFER_RAW_AMOUNT="1"
TRANSFER_TRANSACTION_COST_RAW="12007"

SENDER_BEFORE_JSON="$(run_json "sender balance before" "$CLI" --json balance "$TRANSFER_SENDER")"
RECIPIENT_BEFORE_JSON="$(run_json "recipient balance before" "$CLI" --json balance "$TRANSFER_RECIPIENT")"
SENDER_ADDRESS="$(json_field data.address "$SENDER_BEFORE_JSON")"
RECIPIENT_ADDRESS="$(json_field data.address "$RECIPIENT_BEFORE_JSON")"
SENDER_BALANCE_BEFORE="$(json_field data.raw_balance "$SENDER_BEFORE_JSON")"
RECIPIENT_BALANCE_BEFORE="$(json_field data.raw_balance "$RECIPIENT_BEFORE_JSON")"

TRANSFER_JSON="$(run_json transfer "$CLI" --json transfer "$TRANSFER_RECIPIENT" "$TRANSFER_AMOUNT")"
[[ "$(json_field data.status.result "$TRANSFER_JSON")" == "ok" ]] \
    || die "transfer did not commit successfully: $(json_field data.status "$TRANSFER_JSON")"
[[ "$(json_field data.rawAmount "$TRANSFER_JSON")" == "$TRANSFER_RAW_AMOUNT" ]] \
    || die "transfer raw amount did not equal $TRANSFER_RAW_AMOUNT"
[[ "$(json_field data.sender "$TRANSFER_JSON")" == "$SENDER_ADDRESS" ]] \
    || die "transfer sender did not match $TRANSFER_SENDER address $SENDER_ADDRESS"
[[ "$(json_field data.recipient "$TRANSFER_JSON")" == "$RECIPIENT_ADDRESS" ]] \
    || die "transfer recipient did not match $TRANSFER_RECIPIENT address $RECIPIENT_ADDRESS"

SENDER_AFTER_JSON="$(run_json "sender balance after" "$CLI" --json balance "$TRANSFER_SENDER")"
RECIPIENT_AFTER_JSON="$(run_json "recipient balance after" "$CLI" --json balance "$TRANSFER_RECIPIENT")"
SENDER_BALANCE_AFTER="$(json_field data.raw_balance "$SENDER_AFTER_JSON")"
RECIPIENT_BALANCE_AFTER="$(json_field data.raw_balance "$RECIPIENT_AFTER_JSON")"
TRANSFER_FEE_RAW="$(assert_transfer_balance_deltas \
    "$SENDER_BALANCE_BEFORE" \
    "$SENDER_BALANCE_AFTER" \
    "$RECIPIENT_BALANCE_BEFORE" \
    "$RECIPIENT_BALANCE_AFTER" \
    "$TRANSFER_RAW_AMOUNT" \
    "$TRANSFER_TRANSACTION_COST_RAW")"
log "transfer result: sender=$SENDER_ADDRESS $SENDER_BALANCE_BEFORE->$SENDER_BALANCE_AFTER recipient=$RECIPIENT_ADDRESS $RECIPIENT_BALANCE_BEFORE->$RECIPIENT_BALANCE_AFTER raw=$TRANSFER_RAW_AMOUNT fee=$TRANSFER_FEE_RAW height=$(json_field data.height "$TRANSFER_JSON") tx=$(json_field data.txHash "$TRANSFER_JSON")"

DIRECT_RECIPIENT="$RECIPIENT_ADDRESS"
DIRECT_BEFORE_JSON="$(run_json "direct recipient balance before" "$CLI" --json balance "$DIRECT_RECIPIENT")"
DIRECT_BALANCE_BEFORE="$(json_field data.raw_balance "$DIRECT_BEFORE_JSON")"
DIRECT_TRANSFER_JSON="$(run_json "direct recipient transfer" "$CLI" --json transfer "$DIRECT_RECIPIENT" "$TRANSFER_AMOUNT")"
[[ "$(json_field data.status.result "$DIRECT_TRANSFER_JSON")" == "ok" ]] \
    || die "direct recipient transfer did not commit successfully: $(json_field data.status "$DIRECT_TRANSFER_JSON")"
[[ "$(json_field data.recipient "$DIRECT_TRANSFER_JSON")" == "$DIRECT_RECIPIENT" ]] \
    || die "direct transfer recipient did not match $DIRECT_RECIPIENT"
DIRECT_AFTER_JSON="$(run_json "direct recipient balance after" "$CLI" --json balance "$DIRECT_RECIPIENT")"
DIRECT_BALANCE_AFTER="$(json_field data.raw_balance "$DIRECT_AFTER_JSON")"
assert_exact_balance_increase "$DIRECT_BALANCE_BEFORE" "$DIRECT_BALANCE_AFTER" "$TRANSFER_RAW_AMOUNT"
log "direct-address transfer result: recipient=$DIRECT_RECIPIENT $DIRECT_BALANCE_BEFORE->$DIRECT_BALANCE_AFTER raw=$TRANSFER_RAW_AMOUNT height=$(json_field data.height "$DIRECT_TRANSFER_JSON") tx=$(json_field data.txHash "$DIRECT_TRANSFER_JSON")"

GATEWAY_RPC_URL=""
if [[ "${SMOKE_STANDALONE_GATEWAY:-0}" == "1" ]]; then
    GATEWAY_PORT="$(free_tcp_port)"
    GATEWAY_RPC_URL="http://127.0.0.1:$GATEWAY_PORT"

    log "Starting standalone gateway from runtime manifest: $GATEWAY_RPC_URL"
    "$GATEWAY" --runtime-manifest "$RUNTIME_MANIFEST" --listen "127.0.0.1:$GATEWAY_PORT" \
        --scheduler-reconnect-grace-ms 0 \
        > "$WORK/gateway.out" 2> "$WORK/gateway.err" &
    GATEWAY_PID=$!
    if ! wait_for_gateway_rpc "$GATEWAY_RPC_URL"; then
        cat "$WORK/gateway.err" >&2 || true
        die "standalone gateway did not answer groundhog_version"
    fi
fi

DEPLOY_JSON="$(run_json deploy "$CLI" --json deploy)"
CONTRACT_ADDRESS="$(json_field data.address "$DEPLOY_JSON")"
[[ "$CONTRACT_ADDRESS" != "null" && -n "$CONTRACT_ADDRESS" ]] \
    || die "deploy did not return a contract address"
DEPLOY_HEIGHT="$(json_field data.height "$DEPLOY_JSON")"
DEPLOY_TX_HASH="$(json_field data.tx_hash "$DEPLOY_JSON")"
log "deploy result: address=$CONTRACT_ADDRESS height=$DEPLOY_HEIGHT tx=$DEPLOY_TX_HASH"
assert_transaction_committed "embedded gateway" "$RPC_URL" "$DEPLOY_TX_HASH" "$DEPLOY_HEIGHT"
if [[ -n "$GATEWAY_RPC_URL" ]]; then
    assert_transaction_committed \
        "standalone gateway" "$GATEWAY_RPC_URL" "$DEPLOY_TX_HASH" "$DEPLOY_HEIGHT"
fi

INITIALIZE_JSON="$(run_json "call initialize" "$CLI" --json call "$PROJECT_NAME" initialize "$INITIAL_VALUE")"
assert_call_succeeded "initialize" "$INITIALIZE_JSON"
log "initialize result: status=$(json_field data.status_summary "$INITIALIZE_JSON") height=$(json_field data.height "$INITIALIZE_JSON") tx=$(json_field data.tx_hash "$INITIALIZE_JSON")"

INCREMENT_JSON="$(run_json "call increment" "$CLI" --json call "$PROJECT_NAME" increment "$INCREMENT_AMOUNT")"
assert_call_succeeded "increment" "$INCREMENT_JSON"
log "increment result: status=$(json_field data.status_summary "$INCREMENT_JSON") height=$(json_field data.height "$INCREMENT_JSON") tx=$(json_field data.tx_hash "$INCREMENT_JSON")"

VIEW_FROM="dev-0"
if [[ -n "$GATEWAY_RPC_URL" ]]; then
    log "Switching project RPC to standalone gateway for view/storage checks"
    export GROUNDHOG_RUNTIME_MANIFEST="$RUNTIME_MANIFEST"
    update_rpc_url groundhog.toml "$SMOKE_PROFILE" "$GATEWAY_RPC_URL"
fi

EXPECTED_VALUE=$(( INITIAL_VALUE + INCREMENT_AMOUNT ))
VIEW_JSON="$(run_view_json "view get" "$PROJECT_NAME" get)"
VIEW_OUTPUT="$(json_field data.output.result "$VIEW_JSON")"
[[ "$VIEW_OUTPUT" == "return" ]] || die "view get output was $VIEW_OUTPUT, expected return"
VIEW_RETURN="$(json_field data.returnValue "$VIEW_JSON")"
[[ "$VIEW_RETURN" == "$EXPECTED_VALUE" ]] || die "view get return was $VIEW_RETURN, expected $EXPECTED_VALUE"
log "view get result: height=$(json_field data.height "$VIEW_JSON") caller=$(json_field data.caller "$VIEW_JSON") return=$VIEW_RETURN"

VIEW_ENTRYPOINT="$(json_field data.entrypointId "$VIEW_JSON")"
RAW_VIEW_JSON="$(run_json "raw view get" "$CLI" --json view "$CONTRACT_ADDRESS" "$VIEW_ENTRYPOINT" --calldata 0x)"
RAW_VIEW_OUTPUT="$(json_field data.output.result "$RAW_VIEW_JSON")"
[[ "$RAW_VIEW_OUTPUT" == "return" ]] || die "raw view get output was $RAW_VIEW_OUTPUT, expected return"
RAW_VIEW_RETURN="$(json_field data.returnValue "$RAW_VIEW_JSON")"
[[ "$RAW_VIEW_RETURN" == "null" ]] || die "raw view get decoded return was $RAW_VIEW_RETURN, expected null"
RAW_VIEW_DATA="$(json_field data.output.returnData "$RAW_VIEW_JSON")"
[[ "$RAW_VIEW_DATA" != "null" && "$RAW_VIEW_DATA" != "0x" ]] \
    || die "raw view get did not return raw returnData"
log "raw view get result: height=$(json_field data.height "$RAW_VIEW_JSON") entrypoint=$VIEW_ENTRYPOINT returnData=$RAW_VIEW_DATA"

ERROR_VIEW_JSON="$(run_view_json "view error" "$PROJECT_NAME" error)"
ERROR_VIEW_OUTPUT="$(json_field data.output.result "$ERROR_VIEW_JSON")"
[[ "$ERROR_VIEW_OUTPUT" == "abort" ]] || die "view error output was $ERROR_VIEW_OUTPUT, expected abort"
ERROR_ABORT_DATA="$(json_field data.output.abortData "$ERROR_VIEW_JSON")"
[[ "$ERROR_ABORT_DATA" != "null" ]] || die "view error did not return abortData"
ERROR_RETURN="$(json_field data.returnValue "$ERROR_VIEW_JSON")"
[[ "$ERROR_RETURN" == "null" ]] || die "view error decoded return was $ERROR_RETURN, expected null"
log "view error result: height=$(json_field data.height "$ERROR_VIEW_JSON") abortData=$ERROR_ABORT_DATA"

# `view` runs the contract under hogd `ViewingModeData`: writes happen against
# a discarded scratchpad, so a mutating entrypoint like `decrement` returns
# normally. The post-storage check below is the authoritative guarantee that
# nothing actually committed.
WRITE_VIEW_JSON="$(run_view_json "view decrement simulated" "$PROJECT_NAME" decrement 1)"
WRITE_VIEW_OUTPUT="$(json_field data.output.result "$WRITE_VIEW_JSON")"
[[ "$WRITE_VIEW_OUTPUT" == "return" ]] || die "view decrement output was $WRITE_VIEW_OUTPUT, expected return"
log "view decrement simulated result: height=$(json_field data.height "$WRITE_VIEW_JSON") output=$WRITE_VIEW_OUTPUT"

# View an event-emitting entrypoint to confirm hogd captures events from the
# simulated frame.
PING_TAG=99
PING_VIEW_JSON="$(run_view_json "view ping" "$PROJECT_NAME" ping "$PING_TAG")"
PING_VIEW_OUTPUT="$(json_field data.output.result "$PING_VIEW_JSON")"
[[ "$PING_VIEW_OUTPUT" == "return" ]] || die "view ping output was $PING_VIEW_OUTPUT, expected return"
PING_EVENTS="$(json_field data.events "$PING_VIEW_JSON")"
[[ "$PING_EVENTS" != "null" && "$PING_EVENTS" != "[]" ]] \
    || die "view ping did not surface any events (got $PING_EVENTS)"
PING_EVENT_CONTRACT="$(json_field data.events.0.contractAddress "$PING_VIEW_JSON")"
[[ "$PING_EVENT_CONTRACT" == "$CONTRACT_ADDRESS" ]] \
    || die "view ping event contractAddress was $PING_EVENT_CONTRACT, expected $CONTRACT_ADDRESS"
PING_EVENT_TOPICS="$(json_field data.events.0.topics "$PING_VIEW_JSON")"
[[ "$PING_EVENT_TOPICS" != "[]" ]] || die "view ping event had no topics"
PING_EVENT_COUNT="$(json_len data.events "$PING_VIEW_JSON")"
PING_EVENT_TOPIC0="$(json_field data.events.0.topics.0 "$PING_VIEW_JSON")"
PING_EVENT_DATA="$(json_field data.events.0.data "$PING_VIEW_JSON")"
log "view ping result: height=$(json_field data.height "$PING_VIEW_JSON") events=$PING_EVENT_COUNT \
contract=$PING_EVENT_CONTRACT topic0=$PING_EVENT_TOPIC0 data=$PING_EVENT_DATA"

if [[ -n "$GATEWAY_RPC_URL" ]]; then
    log "Skipping call get through standalone gateway because local block driving is owned by groundhog node"
else
    GET_JSON="$(run_json "call get" "$CLI" --json call "$PROJECT_NAME" get)"
    assert_call_succeeded "get" "$GET_JSON"
    GET_RETURN="$(json_field data.return_value "$GET_JSON")"
    if [[ "$GET_RETURN" == "null" ]]; then
        GET_RETURN="<unavailable until receipt return_data lands>"
    fi
    log "get result: status=$(json_field data.status_summary "$GET_JSON") height=$(json_field data.height "$GET_JSON") tx=$(json_field data.tx_hash "$GET_JSON") return=$GET_RETURN"
fi

COUNTER_VALUE_KEY="$(counter_value_key)"
STORAGE_JSON="$(run_json storage "$CLI" --json storage "$CONTRACT_ADDRESS" "$COUNTER_VALUE_KEY")"
STORAGE_ITEM="$(json_field data.item "$STORAGE_JSON")"
[[ "$STORAGE_ITEM" != "null" ]] || die "counter value storage was absent"
STORAGE_KIND="$(json_field data.item.kind "$STORAGE_JSON")"
[[ "$STORAGE_KIND" == "nonnegativeInt" ]] || die "counter value storage kind was $STORAGE_KIND"
EXPECTED_VALUE=$(( INITIAL_VALUE + INCREMENT_AMOUNT ))
STORAGE_VALUE="$(json_field data.item.value "$STORAGE_JSON")"
[[ "$STORAGE_VALUE" == "$EXPECTED_VALUE" ]] || die "counter storage value was $STORAGE_VALUE, expected $EXPECTED_VALUE"
log "storage args: address=$CONTRACT_ADDRESS key=$COUNTER_VALUE_KEY"
log "storage result: height=$(json_field data.block_height "$STORAGE_JSON") kind=$STORAGE_KIND value=$STORAGE_VALUE"

if [[ -n "$GATEWAY_RPC_URL" ]]; then
    log "Switching project RPC back to groundhog node for wallet-create write checks"
    update_rpc_url groundhog.toml "$SMOKE_PROFILE" "$RPC_URL"
fi

USER_KEY="smoke-user"
log "Generating non-dev signing key: $USER_KEY"
"$CLI" keys generate "$USER_KEY"

WALLET_JSON="$(run_json "wallet create" "$CLI" --json wallet create --key "$USER_KEY")"
WALLET_ADDRESS="$(json_field data.wallet "$WALLET_JSON")"
[[ "$WALLET_ADDRESS" != "null" && -n "$WALLET_ADDRESS" ]] \
    || die "wallet create did not return a wallet address"
WALLET_PREFUND="$(json_field data.prefund_raw_amount "$WALLET_JSON")"
[[ "$WALLET_PREFUND" == "200000000" ]] \
    || die "wallet create prefund was $WALLET_PREFUND, expected 200000000"
[[ "$(json_field data.prefund_tx_hash "$WALLET_JSON")" != "null" ]] \
    || die "wallet create did not return a prefund transaction hash"
[[ "$(json_field data.create_tx_hash "$WALLET_JSON")" != "null" ]] \
    || die "wallet create did not return a create transaction hash"
log "wallet create result: address=$WALLET_ADDRESS prefund=$WALLET_PREFUND"

"$CLI" keys use "$USER_KEY"
USER_DEPLOY_JSON="$(run_json "saved-wallet deploy" "$CLI" --json deploy)"
USER_CONTRACT_ADDRESS="$(json_field data.address "$USER_DEPLOY_JSON")"
[[ "$USER_CONTRACT_ADDRESS" != "null" && -n "$USER_CONTRACT_ADDRESS" ]] \
    || die "saved-wallet deploy did not return a contract address"
log "saved-wallet deploy result: address=$USER_CONTRACT_ADDRESS height=$(json_field data.height "$USER_DEPLOY_JSON") tx=$(json_field data.tx_hash "$USER_DEPLOY_JSON")"

WALLET_BALANCE_BEFORE_JSON="$(run_json "saved-wallet balance before airdrop" "$CLI" --json balance "$USER_KEY")"
WALLET_BALANCE_BEFORE="$(json_field data.raw_balance "$WALLET_BALANCE_BEFORE_JSON")"
AIRDROP_JSON="$(run_json "saved-wallet airdrop" "$CLI" --json airdrop "$USER_KEY")"
[[ "$(json_field data.status.result "$AIRDROP_JSON")" == "ok" ]] \
    || die "saved-wallet airdrop did not commit successfully"
WALLET_BALANCE_AFTER_JSON="$(run_json "saved-wallet balance after airdrop" "$CLI" --json balance "$USER_KEY")"
WALLET_BALANCE_AFTER="$(json_field data.raw_balance "$WALLET_BALANCE_AFTER_JSON")"
AIRDROP_RAW_AMOUNT="$(json_field data.raw_amount "$AIRDROP_JSON")"
assert_exact_balance_increase "$WALLET_BALANCE_BEFORE" "$WALLET_BALANCE_AFTER" "$AIRDROP_RAW_AMOUNT"
log "saved-wallet airdrop result: balance=$WALLET_BALANCE_BEFORE->$WALLET_BALANCE_AFTER raw=$AIRDROP_RAW_AMOUNT height=$(json_field data.height "$AIRDROP_JSON") tx=$(json_field data.tx_hash "$AIRDROP_JSON")"

SECOND_DEPLOY_JSON="$(run_json "same-wallet deploy after airdrop" "$CLI" --json deploy)"
SECOND_CONTRACT_ADDRESS="$(json_field data.address "$SECOND_DEPLOY_JSON")"
[[ "$SECOND_CONTRACT_ADDRESS" != "null" && -n "$SECOND_CONTRACT_ADDRESS" ]] \
    || die "same-wallet deploy retry did not return a contract address"
[[ "$SECOND_CONTRACT_ADDRESS" != "$USER_CONTRACT_ADDRESS" ]] \
    || die "same-wallet deploy retry returned the first contract address"
log "same-wallet deploy retry result: address=$SECOND_CONTRACT_ADDRESS height=$(json_field data.height "$SECOND_DEPLOY_JSON") tx=$(json_field data.tx_hash "$SECOND_DEPLOY_JSON")"

USER_INITIALIZE_JSON="$(run_json "saved-wallet initialize" "$CLI" --json call "$PROJECT_NAME" initialize 11)"
assert_call_succeeded "saved-wallet initialize" "$USER_INITIALIZE_JSON"
USER_INCREMENT_JSON="$(run_json "saved-wallet increment" "$CLI" --json call "$PROJECT_NAME" increment 2)"
assert_call_succeeded "saved-wallet increment" "$USER_INCREMENT_JSON"
log "saved-wallet call result: status=$(json_field data.status_summary "$USER_INCREMENT_JSON") height=$(json_field data.height "$USER_INCREMENT_JSON") tx=$(json_field data.tx_hash "$USER_INCREMENT_JSON")"

unset GROUNDHOG_RUNTIME_MANIFEST
VIEW_FROM="dev-0"
PERSISTED_VIEW_BEFORE="$(run_view_json "view before persistent restart" "$PROJECT_NAME" get)"
PERSISTED_VALUE_BEFORE="$(json_field data.returnValue "$PERSISTED_VIEW_BEFORE")"
[[ "$PERSISTED_VALUE_BEFORE" != "null" ]] \
    || die "view before persistent restart returned null"

if [[ -n "$GATEWAY_PID" ]] && kill -0 "$GATEWAY_PID" >/dev/null 2>&1; then
    kill -TERM "$GATEWAY_PID" >/dev/null 2>&1 || true
    wait "$GATEWAY_PID" >/dev/null 2>&1 || true
    GATEWAY_PID=""
fi

log "Stopping persistent local node"
if ! stop_node "$NODE_PID"; then
    die "persistent local node did not stop within its shutdown grace"
fi
NODE_PID=""
NODE_DESCENDANTS=""

log "Restarting persistent local node"
: > "$WORK/node.json"
: > "$WORK/node.err"
"$CLI" "${NODE_ARGS[@]}" > "$WORK/node.json" 2> "$WORK/node.err" &
NODE_PID=$!
if ! wait_for_node_json "$WORK/node.json"; then
    cat "$WORK/node.err" >&2 || true
    die "restarted local node did not emit startup JSON"
fi
NODE_JSON="$(head -n 1 "$WORK/node.json")"
NODE_DESCENDANTS="$(collect_descendants "$NODE_PID" || true)"
RPC_URL="$(json_field data.rpc_url "$NODE_JSON")"
RUNTIME_MANIFEST="$(json_field data.runtime_manifest.path "$NODE_JSON")"
CHAIN_STATE="$(json_field data.chain_state "$NODE_JSON")"
REPORTED_DATA_DIR="$(json_field data.data_dir "$NODE_JSON")"
[[ "$REPORTED_DATA_DIR" == "$NODE_DATA_DIR" ]] \
    || die "restarted node data_dir was $REPORTED_DATA_DIR, expected $NODE_DATA_DIR"
[[ "$CHAIN_STATE" == "recovered" ]] \
    || die "restarted node chain_state was $CHAIN_STATE, expected recovered"
if [[ "${SMOKE_STANDALONE_GATEWAY:-0}" == "1" ]]; then
    update_rpc_url groundhog.toml "$SMOKE_PROFILE" "$RPC_URL"
fi

PERSISTED_VIEW_AFTER="$(run_view_json "view after persistent restart" "$PROJECT_NAME" get)"
PERSISTED_VALUE_AFTER="$(json_field data.returnValue "$PERSISTED_VIEW_AFTER")"
[[ "$PERSISTED_VALUE_AFTER" == "$PERSISTED_VALUE_BEFORE" ]] \
    || die "persisted counter changed across restart: before=$PERSISTED_VALUE_BEFORE after=$PERSISTED_VALUE_AFTER"
log "persistent restart recovered counter value: $PERSISTED_VALUE_AFTER"

log "Local DX smoke passed"
