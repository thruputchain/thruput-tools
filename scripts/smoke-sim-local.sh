#!/usr/bin/env bash
# Simulator local-target smoke tests.
#
# Runs the ignored hogsim process-level smokes against a real hog-devnode binary:
#   1. local provider launches thruput node itself
#   2. local provider restarts its managed node through restart-target chaos
#   3. sweeper reclaims an orphaned simulator-owned local target
#   4. existing provider connects to a temporary thruput node started here
#
# For the broader scenario ladder, run scripts/smoke-sim-scenarios.sh.
#
# Required environment:
#   THRUPUT_DEVNODE_BINARY=/path/to/hog-devnode
#
# Optional environment:
#   THRUPUT_BINARY=/path/to/thruput
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>
#   KEEP_WORKDIR=1
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

expand_user_path() {
    case "$1" in
        "~")
            printf '%s\n' "$HOME"
            ;;
        "~/"*)
            printf '%s/%s\n' "$HOME" "${1#"~/"}"
            ;;
        *)
            printf '%s\n' "$1"
            ;;
    esac
}

THRUPUT_BINARY="$(expand_user_path "${THRUPUT_BINARY:-$REPO_ROOT/target/debug/thruput}")"

log() {
    printf '==> %s\n' "$*" >&2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

json_field_from_file() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

path = sys.argv[1].split(".")
with open(sys.argv[2], "r", encoding="utf-8") as handle:
    payload = json.loads(handle.readline())
value = payload
for part in path:
    value = value.get(part) if isinstance(value, dict) else value[int(part)]
print(value)
PY
}

wait_for_node_json() {
    local path="$1"
    local deadline=$((SECONDS + 45))
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

terminate_node() {
    local pid="$1"
    local deadline

    if [[ -z "$pid" ]] || ! kill -0 "$pid" >/dev/null 2>&1; then
        return 0
    fi

    kill -TERM "$pid" >/dev/null 2>&1 || true
    deadline=$((SECONDS + 15))
    while kill -0 "$pid" >/dev/null 2>&1; do
        if (( SECONDS >= deadline )); then
            kill -KILL "$pid" >/dev/null 2>&1 || true
            break
        fi
        sleep 0.2
    done
    wait "$pid" >/dev/null 2>&1 || true
}

assert_rpc_port_closed() {
    python3 - "$1" <<'PY'
import socket
import sys
import time
from urllib.parse import urlparse

url = urlparse(sys.argv[1])
host = url.hostname
port = url.port
deadline = time.time() + 5
while time.time() < deadline:
    try:
        with socket.create_connection((host, port), timeout=0.1):
            pass
    except OSError:
        sys.exit(0)
    time.sleep(0.1)

raise SystemExit(f"RPC port still accepts connections: {sys.argv[1]}")
PY
}

find_hogd_dev_binary() {
    if [[ -n "${THRUPUT_DEVNODE_BINARY:-}" ]]; then
        expand_user_path "$THRUPUT_DEVNODE_BINARY"
        return
    fi

    die "set THRUPUT_DEVNODE_BINARY=/path/to/hog-devnode"
}

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/groundhog-sim-smoke.XXXXXX")"
NODE_PID=""
RPC_URL=""

cleanup() {
    terminate_node "$NODE_PID"
    if [[ -n "$RPC_URL" ]]; then
        assert_rpc_port_closed "$RPC_URL"
    fi
    if [[ "${KEEP_WORKDIR:-0}" != "1" ]]; then
        rm -rf "$WORKDIR"
    else
        log "kept workdir: $WORKDIR"
    fi
}
trap cleanup EXIT

DEVNODE_BINARY="$(find_hogd_dev_binary)"
[[ -x "$DEVNODE_BINARY" ]] || die "hog-devnode binary is not executable: $DEVNODE_BINARY"
HOGD_PROTOCOL="$(detect_hogd_protocol "$DEVNODE_BINARY")"
if [[ -n "$HOGD_PROTOCOL" ]]; then
    export GROUNDHOG_SIM_HOGD_PROTOCOL="$HOGD_PROTOCOL"
    log "using hogd protocol override: $HOGD_PROTOCOL"
fi

log "building thruput"
cargo build -p thruput-cli
[[ -x "$THRUPUT_BINARY" ]] || die "thruput binary is not executable: $THRUPUT_BINARY"

log "running local-provider smoke"
THRUPUT_DEVNODE_BINARY="$DEVNODE_BINARY" \
THRUPUT_SIM_CLI_BINARY="$THRUPUT_BINARY" \
    cargo test -p thruput-sim --test local_target_smoke local_target_chain_tip_smoke -- --ignored --nocapture

log "running local-provider restart smoke"
THRUPUT_DEVNODE_BINARY="$DEVNODE_BINARY" \
THRUPUT_SIM_CLI_BINARY="$THRUPUT_BINARY" \
    cargo test -p thruput-sim --test local_target_smoke local_restart_target_chain_tip_smoke -- --ignored --nocapture

log "running local-provider sweeper smoke"
THRUPUT_DEVNODE_BINARY="$DEVNODE_BINARY" \
THRUPUT_SIM_CLI_BINARY="$THRUPUT_BINARY" \
    cargo test -p thruput-sim --test local_target_smoke local_target_sweep_orphan_smoke -- --ignored --nocapture

NODE_STDOUT="$WORKDIR/groundhog-node.json"
NODE_STDERR="$WORKDIR/groundhog-node.stderr"

log "starting temporary thruput node for existing-provider smoke"
NODE_ARGS=(--json node --hog-devnode-binary "$DEVNODE_BINARY" --port 0)
if [[ -n "$HOGD_PROTOCOL" ]]; then
    NODE_ARGS+=(--hogd-protocol "$HOGD_PROTOCOL")
fi
"$THRUPUT_BINARY" "${NODE_ARGS[@]}" >"$NODE_STDOUT" 2>"$NODE_STDERR" &
NODE_PID="$!"

if ! wait_for_node_json "$NODE_STDOUT"; then
    cat "$NODE_STDERR" >&2 || true
    die "thruput node did not emit startup JSON"
fi

RPC_URL="$(json_field_from_file data.rpc_url "$NODE_STDOUT")"
RUNTIME_MANIFEST="$(json_field_from_file data.runtime_manifest.path "$NODE_STDOUT")"

log "running existing-provider smoke against $RPC_URL"
GROUNDHOG_SIM_RPC_URL="$RPC_URL" \
GROUNDHOG_SIM_RUNTIME_MANIFEST="$RUNTIME_MANIFEST" \
GROUNDHOG_SIM_EXISTING_ROOT="$WORKDIR/existing-provider" \
    cargo test -p thruput-sim --test existing_target_smoke -- --ignored --nocapture

terminate_node "$NODE_PID"
NODE_PID=""
assert_rpc_port_closed "$RPC_URL"
RPC_URL=""

log "sim local smokes passed"
