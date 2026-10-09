#!/usr/bin/env bash
# Simulator scenario catalog smoke tests.
#
# Runs the fast local scenario ladder plus the existing-provider baseline
# example against a real hog-devnode binary. The longer local soak is opt-in with
# GROUNDHOG_SIM_RUN_SOAKS=1.
#
# Required environment:
#   THRUPUT_DEVNODE_BINARY=/path/to/hog-devnode
#
# Optional environment:
#   THRUPUT_BINARY=/path/to/thruput
#   GROUNDHOG_EXAMPLES_ROOT=/path/to/groundhog-examples
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

find_examples_root() {
    if [[ -n "${GROUNDHOG_EXAMPLES_ROOT:-}" ]]; then
        expand_user_path "$GROUNDHOG_EXAMPLES_ROOT"
        return
    fi

    local sibling="$REPO_ROOT/../groundhog-examples"
    if [[ -d "$sibling" ]]; then
        printf '%s\n' "$sibling"
    fi
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

EXAMPLES_ROOT="$(find_examples_root)"
CONTRACT_ARTIFACT_ROOT=""
if [[ -n "$EXAMPLES_ROOT" ]]; then
    EXAMPLES_TARGET="$WORKDIR/groundhog-examples-target"
    log "building groundhog-examples fibonacci contract"
    (
        cd "$EXAMPLES_ROOT"
        "$THRUPUT_BINARY" build -p fibonacci --target-dir "$EXAMPLES_TARGET"
    )
    # Required by the fast read-after-write smoke scenario. Keep this a hard
    # failure so stale fixture checkouts do not silently skip core smoke coverage.
    log "building groundhog-examples caller-counter simulator fixture"
    (
        cd "$EXAMPLES_ROOT"
        "$THRUPUT_BINARY" build -p caller-counter --target-dir "$EXAMPLES_TARGET"
    )
    if [[ -d "$EXAMPLES_ROOT/sim-fixtures/calldata" ]]; then
        log "building groundhog-examples calldata simulator fixture"
        (
            cd "$EXAMPLES_ROOT"
            "$THRUPUT_BINARY" build -p calldata --target-dir "$EXAMPLES_TARGET"
        )
    else
        log "skipping groundhog-examples calldata simulator fixture; fixture not present"
    fi
    if [[ -d "$EXAMPLES_ROOT/sim-fixtures/nnint-stress" ]]; then
        log "building groundhog-examples nnint-stress simulator fixture"
        (
            cd "$EXAMPLES_ROOT"
            "$THRUPUT_BINARY" build -p nnint-stress --target-dir "$EXAMPLES_TARGET"
        )
    else
        log "skipping groundhog-examples nnint-stress simulator fixture; fixture not present"
    fi
    CONTRACT_ARTIFACT_ROOT="$EXAMPLES_TARGET/thruput"
else
    log "skipping contract scenario; set GROUNDHOG_EXAMPLES_ROOT to groundhog-examples"
fi

log "running fast local scenario catalog"
ENV_ARGS=(
    "THRUPUT_DEVNODE_BINARY=$DEVNODE_BINARY"
    "THRUPUT_SIM_CLI_BINARY=$THRUPUT_BINARY"
    "GROUNDHOG_SIM_CATALOG_ROOT=$WORKDIR/local-catalog-fast"
)
if [[ -n "$CONTRACT_ARTIFACT_ROOT" ]]; then
    ENV_ARGS+=("GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT=$CONTRACT_ARTIFACT_ROOT")
fi
env "${ENV_ARGS[@]}" \
    cargo test -p groundhog-sim --test local_target_smoke local_scenario_catalog_smoke -- --ignored --nocapture

if [[ "${GROUNDHOG_SIM_RUN_SOAKS:-0}" == "1" ]]; then
    log "running gated local scenario soak"
    SOAK_ENV_ARGS=(
        "THRUPUT_DEVNODE_BINARY=$DEVNODE_BINARY"
        "THRUPUT_SIM_CLI_BINARY=$THRUPUT_BINARY"
        "GROUNDHOG_SIM_CATALOG_ROOT=$WORKDIR/local-catalog-soak"
        "GROUNDHOG_SIM_RUN_SOAKS=1"
    )
    if [[ -n "$CONTRACT_ARTIFACT_ROOT" ]]; then
        SOAK_ENV_ARGS+=("GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT=$CONTRACT_ARTIFACT_ROOT")
    fi
    env "${SOAK_ENV_ARGS[@]}" \
        cargo test -p groundhog-sim --test local_target_smoke local_scenario_catalog_soak -- --ignored --nocapture
else
    log "skipping local scenario soak; set GROUNDHOG_SIM_RUN_SOAKS=1 to run it"
fi

NODE_STDOUT="$WORKDIR/groundhog-node.json"
NODE_STDERR="$WORKDIR/groundhog-node.stderr"

log "starting temporary thruput node for existing-provider baseline scenario"
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
