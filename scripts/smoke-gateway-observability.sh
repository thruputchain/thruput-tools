#!/usr/bin/env bash
# Gateway observability smoke against a real hog-devnode-backed local backend.
#
# Starts `groundhog node`, extracts its runtime manifest, starts a standalone
# `hog-gateway` from that manifest, then validates private health, metrics, and
# bounded tracing signals.
#
# Required environment:
#   GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode
#
# Optional environment:
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>
#   NODE_STARTUP_TIMEOUT_SECS=60
#   NODE_BLOCK_INTERVAL_MS=500
#   NODE_VERBOSE=1
#   GATEWAY_RUST_LOG=info,hog_gateway=debug,groundhog_json_rpc_gateway=debug
#   KEEP_WORKDIR=1
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLI="$REPO_ROOT/target/debug/groundhog"
GATEWAY="$REPO_ROOT/target/debug/hog-gateway"

NODE_STARTUP_TIMEOUT_SECS="${NODE_STARTUP_TIMEOUT_SECS:-60}"
NODE_BLOCK_INTERVAL_MS="${NODE_BLOCK_INTERVAL_MS:-500}"
GATEWAY_RUST_LOG="${GATEWAY_RUST_LOG:-info,hog_gateway=debug,groundhog_json_rpc_gateway=debug}"

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

wait_for_http_json_status() {
    local url="$1"
    local output_path="$2"
    local expected_status="$3"
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if python3 - "$url" "$output_path" "$expected_status" <<'PY' >/dev/null 2>&1; then
import json
import sys
import urllib.error
import urllib.request

url, output_path, expected_status = sys.argv[1:]
try:
    with urllib.request.urlopen(url, timeout=1) as response:
        status = response.status
        body = response.read().decode()
except urllib.error.HTTPError as err:
    status = err.code
    body = err.read().decode()

if str(status) != expected_status:
    raise SystemExit(1)
json.loads(body)
with open(output_path, "w", encoding="utf-8") as handle:
    handle.write(body)
PY
            return 0
        fi
        sleep 0.2
    done
    return 1
}

fetch_text_status() {
    local url="$1"
    local output_path="$2"
    local expected_status="$3"
    python3 - "$url" "$output_path" "$expected_status" <<'PY'
import sys
import urllib.error
import urllib.request

url, output_path, expected_status = sys.argv[1:]
try:
    with urllib.request.urlopen(url, timeout=5) as response:
        status = response.status
        body = response.read().decode()
except urllib.error.HTTPError as err:
    status = err.code
    body = err.read().decode()

if str(status) != expected_status:
    print(f"{url} returned HTTP {status}, expected {expected_status}", file=sys.stderr)
    raise SystemExit(1)
with open(output_path, "w", encoding="utf-8") as handle:
    handle.write(body)
PY
}

post_rpc() {
    local url="$1"
    local method="$2"
    local output_path="$3"
    python3 - "$url" "$method" "$output_path" <<'PY'
import json
import sys
import urllib.request

url, method, output_path = sys.argv[1:]
payload = json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": method,
    "params": [],
}).encode()
request = urllib.request.Request(
    url,
    data=payload,
    headers={"content-type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(request, timeout=5) as response:
    data = json.loads(response.read().decode())
if data.get("error"):
    print(json.dumps(data, sort_keys=True), file=sys.stderr)
    raise SystemExit(1)
with open(output_path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, sort_keys=True)
PY
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

assert_file_contains() {
    local path="$1"
    local pattern="$2"
    if command -v rg >/dev/null 2>&1; then
        rg -q --fixed-strings "$pattern" "$path" || die "$path did not contain: $pattern"
    else
        grep -F -q "$pattern" "$path" || die "$path did not contain: $pattern"
    fi
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

log "Building CLI and gateway"
cargo build -p groundhog-cli -p groundhog-json-rpc-gateway \
    --manifest-path "$REPO_ROOT/Cargo.toml"

WORK="$(mktemp -d)"
NODE_PID=""
GATEWAY_PID=""
NODE_DESCENDANTS=""

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
mkdir -p "$WORK/cache"

NODE_ARGS=(
    --json
    node
    --port
    0
    --startup-timeout-secs
    "$NODE_STARTUP_TIMEOUT_SECS"
    --hog-devnode-binary
    "$DEVNODE_BINARY"
    --block-driver
    interval
    --block-interval-ms
    "$NODE_BLOCK_INTERVAL_MS"
)
if [[ -n "$HOGD_PROTOCOL" ]]; then
    NODE_ARGS+=(--hogd-protocol "$HOGD_PROTOCOL")
    log "Using hogd protocol override: $HOGD_PROTOCOL"
fi
if [[ "${NODE_VERBOSE:-0}" == "1" ]]; then
    NODE_ARGS+=(--verbose)
fi

log "Starting CLI node with interval block driver (${NODE_BLOCK_INTERVAL_MS}ms)"
XDG_CACHE_HOME="$WORK/cache" \
    "$CLI" "${NODE_ARGS[@]}" \
    > "$WORK/node.json" 2> "$WORK/node.err" &
NODE_PID=$!

if ! wait_for_node_json "$WORK/node.json"; then
    sed -n '1,120p' "$WORK/node.err" >&2 || true
    die "local node did not emit startup JSON"
fi

NODE_JSON="$(sed -n '1p' "$WORK/node.json")"
NODE_OK="$(json_field ok "$NODE_JSON")"
if [[ "$NODE_OK" != "true" ]]; then
    sed -n '1,120p' "$WORK/node.err" >&2 || true
    die "local node startup command reported failure"
fi
NODE_DESCENDANTS="$(collect_descendants "$NODE_PID" || true)"
RUNTIME_MANIFEST="$(json_field data.runtime_manifest.path "$NODE_JSON")"
[[ "$RUNTIME_MANIFEST" != "null" && -n "$RUNTIME_MANIFEST" ]] \
    || die "groundhog node did not report a runtime manifest path"
[[ -r "$RUNTIME_MANIFEST" ]] || die "runtime manifest is not readable: $RUNTIME_MANIFEST"
log "Runtime manifest: $RUNTIME_MANIFEST"

GATEWAY_PORT="$(free_tcp_port)"
OBSERVABILITY_PORT="$(free_tcp_port)"
GATEWAY_RPC_URL="http://127.0.0.1:$GATEWAY_PORT"
OBSERVABILITY_URL="http://127.0.0.1:$OBSERVABILITY_PORT"
log "Starting standalone gateway: $GATEWAY_RPC_URL"
log "Gateway observability: $OBSERVABILITY_URL"
RUST_LOG="$GATEWAY_RUST_LOG" \
    "$GATEWAY" \
    --runtime-manifest "$RUNTIME_MANIFEST" \
    --listen "127.0.0.1:$GATEWAY_PORT" \
    --observability-listen "127.0.0.1:$OBSERVABILITY_PORT" \
    --health-probe-interval-ms 500 \
    > "$WORK/gateway.out" 2> "$WORK/gateway.err" &
GATEWAY_PID=$!

log "Waiting for readiness"
if ! wait_for_http_json_status "$OBSERVABILITY_URL/readyz" "$WORK/readyz.json" 200; then
    sed -n '1,160p' "$WORK/gateway.err" >&2 || true
    die "standalone gateway did not become ready"
fi
READY_STATUS="$(json_field status "$(<"$WORK/readyz.json")")"
case "$READY_STATUS" in
    ok|degraded) ;;
    *) die "readyz status was $READY_STATUS, expected ok or degraded" ;;
esac
log "readyz status: $READY_STATUS"

fetch_text_status "$OBSERVABILITY_URL/livez" "$WORK/livez.json" 200
fetch_text_status "$OBSERVABILITY_URL/health" "$WORK/health.json" 200
post_rpc "$GATEWAY_RPC_URL" groundhog_version "$WORK/rpc-version.json"
post_rpc "$GATEWAY_RPC_URL" groundhog_chainTip "$WORK/rpc-chain-tip.json"
fetch_text_status "$OBSERVABILITY_URL/metrics" "$WORK/metrics.txt" 200

assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_health_status{status=\"$READY_STATUS\"} 1"
assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_backend_up{backend=\"scheduler\"} 1"
assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_backend_up{backend=\"indexer\"} 1"
assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_rpc_requests_total{method=\"groundhog_version\",outcome=\"ok\"}"
assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_rpc_requests_total{method=\"groundhog_chainTip\",outcome=\"ok\"}"
assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_http_requests_total{endpoint=\"readyz\",status_class=\"2xx\"}"
assert_file_contains "$WORK/metrics.txt" "groundhog_gateway_http_requests_total{endpoint=\"metrics\",status_class=\"2xx\"}"

assert_file_contains "$WORK/gateway.err" "gateway readiness status changed"
assert_file_contains "$WORK/gateway.err" "gateway readiness endpoint completed"
assert_file_contains "$WORK/gateway.err" "gateway metrics endpoint completed"
assert_file_contains "$WORK/gateway.err" "gateway JSON-RPC call completed"

check_process_logs

log "Gateway observability smoke passed"
