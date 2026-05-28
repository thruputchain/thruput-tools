#!/usr/bin/env bash
# Simulator scenario catalog smoke tests.
#
# Runs the fast local scenario ladder plus the existing-provider baseline
# example against a real hogd-dev binary. The longer local soak is opt-in with
# GROUNDHOG_SIM_RUN_SOAKS=1.
#
# Optional environment:
#   HOGD_DEV_BINARY=/path/to/hogd-dev
#   GROUNDHOG_HOGD_DEV_BINARY=/path/to/hogd-dev
#   GROUNDHOG_BINARY=/path/to/groundhog
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>
#   GROUNDHOG_SIM_RUN_SOAKS=1
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

GROUNDHOG_BINARY="$(expand_user_path "${GROUNDHOG_BINARY:-$REPO_ROOT/target/debug/groundhog}")"

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
    if [[ -n "${HOGD_DEV_BINARY:-}" ]]; then
        expand_user_path "$HOGD_DEV_BINARY"
        return
    fi
    if [[ -n "${GROUNDHOG_HOGD_DEV_BINARY:-}" ]]; then
        expand_user_path "$GROUNDHOG_HOGD_DEV_BINARY"
        return
    fi

    local sibling="$REPO_ROOT/../hogd/build/hogd/hogd_dev/hogd-dev"
    if [[ -x "$sibling" ]]; then
        printf '%s\n' "$sibling"
        return
    fi

    if command -v hogd-dev >/dev/null 2>&1; then
        command -v hogd-dev
        return
    fi

    die "set HOGD_DEV_BINARY or GROUNDHOG_HOGD_DEV_BINARY to /path/to/hogd-dev"
}

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/groundhog-sim-scenarios.XXXXXX")"
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

HOGD_DEV_BINARY="$(find_hogd_dev_binary)"
[[ -x "$HOGD_DEV_BINARY" ]] || die "hogd-dev binary is not executable: $HOGD_DEV_BINARY"
HOGD_PROTOCOL="$(detect_hogd_protocol "$HOGD_DEV_BINARY")"
if [[ -n "$HOGD_PROTOCOL" ]]; then
    export GROUNDHOG_SIM_HOGD_PROTOCOL="$HOGD_PROTOCOL"
    log "using hogd protocol override: $HOGD_PROTOCOL"
fi

log "building groundhog with dev-node feature"
cargo build -p groundhog-cli --features dev-node
[[ -x "$GROUNDHOG_BINARY" ]] || die "groundhog binary is not executable: $GROUNDHOG_BINARY"

log "running fast local scenario catalog"
GROUNDHOG_SIM_HOGD_DEV_BINARY="$HOGD_DEV_BINARY" \
GROUNDHOG_SIM_GROUNDHOG_BINARY="$GROUNDHOG_BINARY" \
GROUNDHOG_SIM_CATALOG_ROOT="$WORKDIR/local-catalog-fast" \
    cargo test -p groundhog-sim --test local_target_smoke local_scenario_catalog_smoke -- --ignored --nocapture

if [[ "${GROUNDHOG_SIM_RUN_SOAKS:-0}" == "1" ]]; then
    log "running gated local scenario soak"
    GROUNDHOG_SIM_HOGD_DEV_BINARY="$HOGD_DEV_BINARY" \
    GROUNDHOG_SIM_GROUNDHOG_BINARY="$GROUNDHOG_BINARY" \
    GROUNDHOG_SIM_CATALOG_ROOT="$WORKDIR/local-catalog-soak" \
    GROUNDHOG_SIM_RUN_SOAKS=1 \
        cargo test -p groundhog-sim --test local_target_smoke local_scenario_catalog_soak -- --ignored --nocapture
else
    log "skipping local scenario soak; set GROUNDHOG_SIM_RUN_SOAKS=1 to run it"
fi

NODE_STDOUT="$WORKDIR/groundhog-node.json"
NODE_STDERR="$WORKDIR/groundhog-node.stderr"

log "starting temporary groundhog node for existing-provider baseline scenario"
NODE_ARGS=(--json node --hogd-dev-binary "$HOGD_DEV_BINARY" --port 0)
if [[ -n "$HOGD_PROTOCOL" ]]; then
    NODE_ARGS+=(--hogd-protocol "$HOGD_PROTOCOL")
fi
"$GROUNDHOG_BINARY" "${NODE_ARGS[@]}" >"$NODE_STDOUT" 2>"$NODE_STDERR" &
NODE_PID="$!"

if ! wait_for_node_json "$NODE_STDOUT"; then
    cat "$NODE_STDERR" >&2 || true
    die "groundhog node did not emit startup JSON"
fi

RPC_URL="$(json_field_from_file data.rpc_url "$NODE_STDOUT")"
RUNTIME_MANIFEST="$(json_field_from_file data.runtime_manifest.path "$NODE_STDOUT")"

log "running existing-provider baseline scenario against $RPC_URL"
GROUNDHOG_SIM_RPC_URL="$RPC_URL" \
GROUNDHOG_SIM_RUNTIME_MANIFEST="$RUNTIME_MANIFEST" \
GROUNDHOG_SIM_EXISTING_ROOT="$WORKDIR/existing-provider" \
    cargo test -p groundhog-sim --test existing_target_smoke existing_target_chain_tip_baseline_smoke -- --ignored --nocapture

terminate_node "$NODE_PID"
NODE_PID=""
assert_rpc_port_closed "$RPC_URL"
RPC_URL=""

log "sim scenario catalog smokes passed"
