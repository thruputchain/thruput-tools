#!/usr/bin/env bash
# Real local-DX smoke test.
#
# Exercises the user-facing CLI path against a real hog-devnode binary:
#   groundhog new -> build -> node -> deploy -> call -> view -> storage
#
# Optional environment:
#   HOG_DEVNODE_BINARY=/path/to/hog-devnode
#   HOGD_DEV_BINARY=/path/to/hogd-dev  # legacy
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>  # intro token only; XDR still validates shape
#   SMOKE_STANDALONE_GATEWAY=1  # route view/storage checks through hog-gateway
#   SMOKE_SDK_PATH=/path/to/sdk_rs/crates/sdk  # test scaffold against local SDK
#   KEEP_WORKDIR=1
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLI="$REPO_ROOT/target/debug/groundhog"
GATEWAY="$REPO_ROOT/target/debug/hog-gateway"
PROJECT_NAME="smoke-counter"
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

counter_value_key() {
    python3 - <<'PY'
import hashlib

# #[sdk::storage] assigns struct fields by sequence index. Counter.value is
# the first field, and CXS encodes u64 index 0 as the single byte 0x00.
print("0x" + hashlib.sha256(bytes([0])).hexdigest())
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
    if [[ -n "${HOG_DEVNODE_BINARY:-}" ]]; then
        printf '%s\n' "$HOG_DEVNODE_BINARY"
        return
    fi
    if [[ -n "${HOGD_DEV_BINARY:-}" ]]; then
        printf '%s\n' "$HOGD_DEV_BINARY"
        return
    fi

    for sibling in \
        "$REPO_ROOT/../hogd/build/hogd/hogd_dev/hog-devnode" \
        "$REPO_ROOT/../hogd/build/hogd/hogd_dev/hogd-dev"
    do
        if [[ -x "$sibling" ]]; then
            printf '%s\n' "$sibling"
            return
        fi
    done

    for binary in hog-devnode hogd-dev; do
        if command -v "$binary" >/dev/null 2>&1; then
            command -v "$binary"
            return
        fi
    done

    die "set HOG_DEVNODE_BINARY=/path/to/hog-devnode"
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
    local rpc_url="$2"
    python3 - "$config" "$rpc_url" <<'PY'
import sys

path, rpc_url = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as handle:
    text = handle.read()
lines = []
replaced = False
in_default = False
for line in text.splitlines():
    stripped = line.strip()
    if stripped.startswith("[profile."):
        in_default = stripped == "[profile.default]"
    if in_default and stripped.startswith("rpc_url"):
        lines.append(f'rpc_url = "{rpc_url}"')
        replaced = True
    else:
        lines.append(line)
if not replaced:
    raise SystemExit("profile.default rpc_url not found")
with open(path, "w", encoding="utf-8") as handle:
    handle.write("\n".join(lines) + "\n")
PY
}

override_sdk_path() {
    local manifest="$1"
    local sdk_path="$2"
    [[ -d "$sdk_path" ]] || die "SMOKE_SDK_PATH is not a directory: $sdk_path"

    python3 - "$manifest" "$sdk_path" <<'PY'
import json
import pathlib
import sys

manifest = pathlib.Path(sys.argv[1])
sdk_path = pathlib.Path(sys.argv[2]).resolve()
replacement = f'sdk = {{ path = {json.dumps(str(sdk_path))}, features = ["storage"] }}'
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
    [[ "$status_summary" == "succeeded" ]] || die "$label status was $status_summary"
}

collect_descendants() {
    local pid="$1"
    local child
    pgrep -P "$pid" 2>/dev/null | while read -r child; do
        printf '%s\n' "$child"
        collect_descendants "$child"
    done
}

if [[ "${SMOKE_STANDALONE_GATEWAY:-0}" == "1" ]]; then
    log "Building CLI and standalone gateway"
    cargo build -p groundhog-cli -p groundhog-json-rpc-gateway --features dev-node --manifest-path "$REPO_ROOT/Cargo.toml"
else
    log "Building CLI with dev-node feature"
    cargo build -p groundhog-cli --features dev-node --manifest-path "$REPO_ROOT/Cargo.toml"
fi

HOGD_DEV_BINARY="$(find_hogd_dev_binary)"
[[ -x "$HOGD_DEV_BINARY" ]] || die "hog-devnode binary is not executable: $HOGD_DEV_BINARY"

HOGD_PROTOCOL="$(detect_hogd_protocol "$HOGD_DEV_BINARY")"
NODE_ARGS=(--json node --port 0 --hog-devnode-binary "$HOGD_DEV_BINARY")
if [[ -n "$HOGD_PROTOCOL" ]]; then
    NODE_ARGS+=(--hogd-protocol "$HOGD_PROTOCOL")
    log "Using hogd protocol override: $HOGD_PROTOCOL"
fi

WORK="$(mktemp -d)"
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
        # shellcheck disable=SC2086
        kill "$NODE_PID" $descendants >/dev/null 2>&1 || true
        wait "$NODE_PID" >/dev/null 2>&1 || true
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

if [[ -n "${SMOKE_SDK_PATH:-}" ]]; then
    log "Using local SDK path: $SMOKE_SDK_PATH"
    override_sdk_path Cargo.toml "$SMOKE_SDK_PATH"
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
log "Node RPC: $RPC_URL (read plane: $READ_PLANE, view plane: $VIEW_PLANE)"
[[ "$VIEW_PLANE" == "view_gateway" ]] || die "node view plane was $VIEW_PLANE, expected view_gateway"
[[ "$RUNTIME_MANIFEST" != "null" && -n "$RUNTIME_MANIFEST" ]] \
    || die "groundhog node did not report a runtime manifest path"
[[ -r "$RUNTIME_MANIFEST" ]] || die "runtime manifest is not readable: $RUNTIME_MANIFEST"
LOCAL_NODE_STATE=".groundhog/local-node.json"
[[ -r "$LOCAL_NODE_STATE" ]] || die "local node state was not written: $LOCAL_NODE_STATE"
STATE_RPC_URL="$(json_field rpcUrl "$(cat "$LOCAL_NODE_STATE")")"
STATE_RUNTIME_MANIFEST="$(json_field runtimeManifest "$(cat "$LOCAL_NODE_STATE")")"
[[ "$STATE_RPC_URL" == "$RPC_URL" ]] \
    || die "local node state rpcUrl was $STATE_RPC_URL, expected $RPC_URL"
[[ "$STATE_RUNTIME_MANIFEST" == "$RUNTIME_MANIFEST" ]] \
    || die "local node state runtimeManifest was $STATE_RUNTIME_MANIFEST, expected $RUNTIME_MANIFEST"
unset GROUNDHOG_RUNTIME_MANIFEST

GATEWAY_RPC_URL=""
if [[ "${SMOKE_STANDALONE_GATEWAY:-0}" == "1" ]]; then
    GATEWAY_PORT="$(free_tcp_port)"
    GATEWAY_RPC_URL="http://127.0.0.1:$GATEWAY_PORT"

    log "Starting standalone gateway from runtime manifest: $GATEWAY_RPC_URL"
    "$GATEWAY" --runtime-manifest "$RUNTIME_MANIFEST" --listen "127.0.0.1:$GATEWAY_PORT" \
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
log "deploy result: address=$CONTRACT_ADDRESS height=$(json_field data.height "$DEPLOY_JSON") tx=$(json_field data.tx_hash "$DEPLOY_JSON")"

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
    update_rpc_url groundhog.toml "$GATEWAY_RPC_URL"
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

log "Local DX smoke passed"
