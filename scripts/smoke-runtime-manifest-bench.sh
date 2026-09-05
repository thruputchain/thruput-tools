#!/usr/bin/env bash
# Runtime-manifest gateway/bench smoke and scenario runner.
#
# Starts a real hog-devnode-backed `groundhog node`, extracts the runtime manifest
# path from the CLI startup JSON, starts a standalone gateway from the same
# manifest, then runs a named groundhog-bench scenario.
#
# Required environment:
#   GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode
#
# Optional environment:
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>  # intro token only; XDR still validates shape
#   NODE_STARTUP_TIMEOUT_SECS=60
#   NODE_VERBOSE=1
#   BENCH_SCENARIO=smoke|gateway-baseline|submit-ramp
#   BENCH_RATE=3
#   BENCH_DURATION=2
#   BENCH_WARMUP=0
#   BENCH_WORKERS=1
#   BENCH_ACCOUNTS=3
#   BENCH_REQUEST_TIMEOUT_MS=10000
#   BENCH_MAX_RESPONSE_BYTES=1048576
#   BENCH_BUILD_PROFILE=debug|release
#   BENCH_RESULTS_DIR=/path/to/results
#   SUBMIT_RAMP_RATES="250 500 1000"
#   KEEP_WORKDIR=1
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

BENCH_SCENARIO="${BENCH_SCENARIO:-smoke}"
NODE_STARTUP_TIMEOUT_SECS="${NODE_STARTUP_TIMEOUT_SECS:-60}"
BENCH_RATE="${BENCH_RATE:-3}"
BENCH_DURATION="${BENCH_DURATION:-2}"
BENCH_WARMUP="${BENCH_WARMUP:-0}"
BENCH_WORKERS="${BENCH_WORKERS:-1}"
BENCH_ACCOUNTS="${BENCH_ACCOUNTS:-3}"
BENCH_REQUEST_TIMEOUT_MS="${BENCH_REQUEST_TIMEOUT_MS:-10000}"
BENCH_MAX_RESPONSE_BYTES="${BENCH_MAX_RESPONSE_BYTES:-1048576}"
BENCH_BUILD_PROFILE="${BENCH_BUILD_PROFILE:-}"
BENCH_RESULTS_DIR="${BENCH_RESULTS_DIR:-}"
BENCH_RUN_DIR=""
BENCH_RESULT_TMP=""
BENCH_RESULT_SEQUENCE=0

GATEWAY_BASELINE_CHAIN_RATE="${GATEWAY_BASELINE_CHAIN_RATE:-250}"
GATEWAY_BASELINE_SUBMIT_RATE="${GATEWAY_BASELINE_SUBMIT_RATE:-100}"
GATEWAY_BASELINE_DURATION="${GATEWAY_BASELINE_DURATION:-30}"
GATEWAY_BASELINE_WARMUP="${GATEWAY_BASELINE_WARMUP:-2}"
GATEWAY_BASELINE_WORKERS="${GATEWAY_BASELINE_WORKERS:-4}"
GATEWAY_BASELINE_ACCOUNTS="${GATEWAY_BASELINE_ACCOUNTS:-10}"

SUBMIT_RAMP_RATES="${SUBMIT_RAMP_RATES:-250 500 1000}"
SUBMIT_RAMP_DURATION="${SUBMIT_RAMP_DURATION:-30}"
SUBMIT_RAMP_WARMUP="${SUBMIT_RAMP_WARMUP:-2}"
SUBMIT_RAMP_WORKERS="${SUBMIT_RAMP_WORKERS:-4}"
SUBMIT_RAMP_ACCOUNTS="${SUBMIT_RAMP_ACCOUNTS:-10}"

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
elif isinstance(value, bool):
    print("true" if value else "false")
elif isinstance(value, (dict, list)):
    print(json.dumps(value, sort_keys=True, separators=(",", ":")))
else:
    print(value)
PY
}

wait_for_node_json() {
    local path="$1"
    local deadline=$((SECONDS + NODE_STARTUP_TIMEOUT_SECS + 15))
    while (( SECONDS < deadline )); do
        if [[ -s "$path" ]]; then
            python3 - "$path" <<'PY' >/dev/null 2>&1 && return 0
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    json.loads(handle.readline())
PY
        fi
        sleep 0.2
    done
    return 1
}

wait_for_gateway_rpc() {
    local url="$1"
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if python3 - "$url" <<'PY' >/dev/null 2>&1; then
import json
import sys
import urllib.request

payload = json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "groundhog_chainTip",
    "params": [],
}).encode()
request = urllib.request.Request(
    sys.argv[1],
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

free_tcp_port() {
    python3 - <<'PY'
import socket

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
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
match = re.search(r'protocol_commit\[\]\s*=\s*(?:u8)?"([0-9a-fA-F]+)"', text)
if match:
    token = f"hogp/{match.group(1)[:15]}"
    # hogd derives the same 20 bytes; anything else is rejected outright now
    # that ProtocolCommit is a fixed-width array.
    if len(token) != 20:
        sys.exit(f"derived protocol token {token!r} is {len(token)} bytes, want 20")
    print(token)
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

collect_descendants() {
    local pid="$1"
    local child
    pgrep -P "$pid" 2>/dev/null | while read -r child; do
        printf '%s\n' "$child"
        collect_descendants "$child"
    done
}

benchmark_slug() {
    printf '%s' "$1" | tr '[:upper:] /' '[:lower:]--'
}

run_bench_command() {
    local filename="$1"
    shift

    if [[ -n "$BENCH_RESULTS_DIR" ]]; then
        local sequence
        BENCH_RESULT_SEQUENCE=$((BENCH_RESULT_SEQUENCE + 1))
        printf -v sequence '%03d' "$BENCH_RESULT_SEQUENCE"
        local output="$BENCH_RUN_DIR/$sequence-$filename"
        BENCH_RESULT_TMP="$output.tmp"
        "$BENCH" "$@" \
            --request-timeout-ms "$BENCH_REQUEST_TIMEOUT_MS" \
            --max-response-bytes "$BENCH_MAX_RESPONSE_BYTES" \
            --json > "$BENCH_RESULT_TMP"
        python3 - "$BENCH_RESULT_TMP" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    result = json.load(handle)
if result.get("schemaVersion") != 1 or result.get("kind") != "groundhog-bench-result":
    raise SystemExit("invalid groundhog-bench result envelope")
PY
        mv "$BENCH_RESULT_TMP" "$output"
        BENCH_RESULT_TMP=""
        log "Wrote benchmark result: $output"
    else
        "$BENCH" "$@" \
            --request-timeout-ms "$BENCH_REQUEST_TIMEOUT_MS" \
            --max-response-bytes "$BENCH_MAX_RESPONSE_BYTES"
    fi
}

bench_chain_tip() {
    local label="$1"
    local url="$2"
    local manifest="$3"
    local rate="$4"
    local duration="$5"
    local warmup="$6"
    local workers="$7"

    log "Bench $label chainTip: ${rate} rps, ${duration}s, ${warmup}s warmup, ${workers} workers"
    run_bench_command "$(benchmark_slug "$label")-chain-tip-${rate}rps.json" \
        --scenario "$BENCH_SCENARIO" --target-label "$label" \
        --url "$url" --runtime-manifest "$manifest" --method chainTip \
        --rate "$rate" --duration "$duration" --warmup "$warmup" --workers "$workers"
}

bench_send_raw() {
    local label="$1"
    local url="$2"
    local manifest="$3"
    local rate="$4"
    local duration="$5"
    local warmup="$6"
    local workers="$7"
    local accounts="$8"

    log "Bench $label sendRawTransaction: ${rate} rps, ${duration}s, ${warmup}s warmup, ${workers} workers, ${accounts} accounts"
    run_bench_command "$(benchmark_slug "$label")-send-raw-transaction-${rate}rps.json" \
        --scenario "$BENCH_SCENARIO" --target-label "$label" \
        --url "$url" --runtime-manifest "$manifest" --method sendRawTransaction \
        --accounts "$accounts" --rate "$rate" --duration "$duration" --warmup "$warmup" --workers "$workers" \
        --force
}

run_bench_pair() {
    local label="$1"
    local url="$2"
    local manifest="$3"

    bench_chain_tip "$label" "$url" "$manifest" "$BENCH_RATE" "$BENCH_DURATION" "$BENCH_WARMUP" "$BENCH_WORKERS"
    bench_send_raw "$label" "$url" "$manifest" "$BENCH_RATE" "$BENCH_DURATION" \
        "$BENCH_WARMUP" "$BENCH_WORKERS" "$BENCH_ACCOUNTS"
}

run_smoke() {
    local node_url="$1"
    local gateway_url="$2"
    local manifest="$3"

    run_bench_pair "CLI node" "$node_url" "$manifest"
    run_bench_pair "standalone gateway" "$gateway_url" "$manifest"
}

run_gateway_baseline() {
    local gateway_url="$1"
    local manifest="$2"

    bench_chain_tip "standalone gateway baseline" "$gateway_url" "$manifest" \
        "$GATEWAY_BASELINE_CHAIN_RATE" "$GATEWAY_BASELINE_DURATION" \
        "$GATEWAY_BASELINE_WARMUP" "$GATEWAY_BASELINE_WORKERS"
    bench_send_raw "standalone gateway baseline" "$gateway_url" "$manifest" \
        "$GATEWAY_BASELINE_SUBMIT_RATE" "$GATEWAY_BASELINE_DURATION" \
        "$GATEWAY_BASELINE_WARMUP" "$GATEWAY_BASELINE_WORKERS" \
        "$GATEWAY_BASELINE_ACCOUNTS"
}

run_submit_ramp() {
    local gateway_url="$1"
    local manifest="$2"
    local rate

    for rate in $SUBMIT_RAMP_RATES; do
        bench_send_raw "standalone gateway submit ramp" "$gateway_url" "$manifest" \
            "$rate" "$SUBMIT_RAMP_DURATION" "$SUBMIT_RAMP_WARMUP" \
            "$SUBMIT_RAMP_WORKERS" "$SUBMIT_RAMP_ACCOUNTS"
    done
}

check_process_logs() {
    local log_path

    for log_path in "$WORK/node.err" "$WORK/gateway.err"; do
        [[ -f "$log_path" ]] || continue
        if command -v rg >/dev/null 2>&1; then
            if rg -i 'panic|abort|fatal|critical|segmentation fault' "$log_path" >&2; then
                die "fatal process log pattern found in $log_path"
            fi
        elif grep -E -i 'panic|abort|fatal|critical|segmentation fault' "$log_path" >&2; then
            die "fatal process log pattern found in $log_path"
        fi
    done
}

DEVNODE_BINARY="$(find_hogd_dev_binary)"
[[ -x "$DEVNODE_BINARY" ]] || die "hog-devnode binary is not executable: $DEVNODE_BINARY"
HOGD_PROTOCOL="$(detect_hogd_protocol "$DEVNODE_BINARY")"

if [[ -z "$BENCH_BUILD_PROFILE" ]]; then
    if [[ "$BENCH_SCENARIO" == "smoke" ]]; then
        BENCH_BUILD_PROFILE=debug
    else
        BENCH_BUILD_PROFILE=release
    fi
fi
case "$BENCH_BUILD_PROFILE" in
    debug)
        # Keep a nonempty array: Bash 3.2 treats an empty array as unset under -u.
        BENCH_BUILD_ARGS=(--profile dev)
        ;;
    release)
        BENCH_BUILD_ARGS=(--release)
        ;;
    *)
        die "unknown BENCH_BUILD_PROFILE=$BENCH_BUILD_PROFILE (expected debug or release)"
        ;;
esac
CLI="$REPO_ROOT/target/$BENCH_BUILD_PROFILE/groundhog"
GATEWAY="$REPO_ROOT/target/$BENCH_BUILD_PROFILE/hog-gateway"
BENCH="$REPO_ROOT/target/$BENCH_BUILD_PROFILE/groundhog-bench"

unset GROUNDHOG_BENCH_GIT_COMMIT GROUNDHOG_BENCH_GIT_DIRTY
if GROUNDHOG_BENCH_GIT_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null)"; then
    export GROUNDHOG_BENCH_GIT_COMMIT
fi
if git_status="$(git -C "$REPO_ROOT" status --porcelain --untracked-files=normal 2>/dev/null)"; then
    if [[ -n "$git_status" ]]; then
        GROUNDHOG_BENCH_GIT_DIRTY=true
    else
        GROUNDHOG_BENCH_GIT_DIRTY=false
    fi
    export GROUNDHOG_BENCH_GIT_DIRTY
fi

log "Building $BENCH_BUILD_PROFILE CLI, gateway, and bench"
cargo build "${BENCH_BUILD_ARGS[@]}" \
    -p groundhog-cli -p groundhog-json-rpc-gateway -p groundhog-bench \
    --manifest-path "$REPO_ROOT/Cargo.toml"

if [[ -n "$BENCH_RESULTS_DIR" ]]; then
    mkdir -p "$BENCH_RESULTS_DIR"
    BENCH_RUN_DIR="$BENCH_RESULTS_DIR/${BENCH_SCENARIO}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
    mkdir "$BENCH_RUN_DIR"
    log "Benchmark result directory: $BENCH_RUN_DIR"
fi

WORK="$(mktemp -d)"
NODE_PID=""
GATEWAY_PID=""
NODE_DESCENDANTS=""

cleanup() {
    if [[ -n "$BENCH_RESULT_TMP" ]]; then
        rm -f "$BENCH_RESULT_TMP"
    fi
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
mkdir -p "$WORK/cache"
log "Starting CLI node"
NODE_ARGS=(
    --json
    node
    --port
    0
    --startup-timeout-secs
    "$NODE_STARTUP_TIMEOUT_SECS"
    --hog-devnode-binary
    "$DEVNODE_BINARY"
)
if [[ -n "$HOGD_PROTOCOL" ]]; then
    NODE_ARGS+=(--hogd-protocol "$HOGD_PROTOCOL")
    log "Using hogd protocol override: $HOGD_PROTOCOL"
fi
if [[ "${NODE_VERBOSE:-0}" == "1" ]]; then
    NODE_ARGS+=(--verbose)
fi
XDG_CACHE_HOME="$WORK/cache" \
    "$CLI" "${NODE_ARGS[@]}" \
    > "$WORK/node.json" 2> "$WORK/node.err" &
NODE_PID=$!

if ! wait_for_node_json "$WORK/node.json"; then
    cat "$WORK/node.json" >&2 || true
    cat "$WORK/node.err" >&2 || true
    die "local node did not emit startup JSON"
fi

NODE_JSON="$(head -n 1 "$WORK/node.json")"
NODE_OK="$(json_field ok "$NODE_JSON")"
if [[ "$NODE_OK" != "true" ]]; then
    cat "$WORK/node.json" >&2 || true
    cat "$WORK/node.err" >&2 || true
    die "local node startup command reported failure"
fi
NODE_DESCENDANTS="$(collect_descendants "$NODE_PID" || true)"
NODE_RPC_URL="$(json_field data.rpc_url "$NODE_JSON")"
RUNTIME_MANIFEST="$(json_field data.runtime_manifest.path "$NODE_JSON")"
[[ "$RUNTIME_MANIFEST" != "null" && -n "$RUNTIME_MANIFEST" ]] \
    || die "groundhog node did not report a runtime manifest path"
[[ -r "$RUNTIME_MANIFEST" ]] || die "runtime manifest is not readable: $RUNTIME_MANIFEST"
log "Node RPC: $NODE_RPC_URL"
log "Runtime manifest: $RUNTIME_MANIFEST"

GATEWAY_PORT="$(free_tcp_port)"
GATEWAY_RPC_URL="http://127.0.0.1:$GATEWAY_PORT"
log "Starting standalone gateway: $GATEWAY_RPC_URL"
"$GATEWAY" --runtime-manifest "$RUNTIME_MANIFEST" --listen "127.0.0.1:$GATEWAY_PORT" \
    --scheduler-reconnect-grace-ms 0 \
    > "$WORK/gateway.out" 2> "$WORK/gateway.err" &
GATEWAY_PID=$!

if ! wait_for_gateway_rpc "$GATEWAY_RPC_URL"; then
    cat "$WORK/gateway.err" >&2 || true
    die "standalone gateway did not answer chainTip"
fi

case "$BENCH_SCENARIO" in
    smoke)
        run_smoke "$NODE_RPC_URL" "$GATEWAY_RPC_URL" "$RUNTIME_MANIFEST"
        ;;
    gateway-baseline)
        run_gateway_baseline "$GATEWAY_RPC_URL" "$RUNTIME_MANIFEST"
        ;;
    submit-ramp)
        run_submit_ramp "$GATEWAY_RPC_URL" "$RUNTIME_MANIFEST"
        ;;
    *)
        die "unknown BENCH_SCENARIO=$BENCH_SCENARIO (expected smoke, gateway-baseline, or submit-ramp)"
        ;;
esac

check_process_logs

log "Runtime-manifest bench scenario '$BENCH_SCENARIO' passed"
