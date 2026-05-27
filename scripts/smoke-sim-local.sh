#!/usr/bin/env bash
# Simulator local-target smoke tests.
#
# Runs the ignored hoglab process-level smokes against a real hogd-dev binary:
#   1. local provider launches groundhog node itself
#   2. existing provider connects to a temporary groundhog node started here
#
# Optional environment:
#   HOGD_DEV_BINARY=/path/to/hogd-dev
#   GROUNDHOG_HOGD_DEV_BINARY=/path/to/hogd-dev
#   GROUNDHOG_BINARY=/path/to/groundhog
#   KEEP_WORKDIR=1
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GROUNDHOG_BINARY="${GROUNDHOG_BINARY:-$REPO_ROOT/target/debug/groundhog}"

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

find_hogd_dev_binary() {
    if [[ -n "${HOGD_DEV_BINARY:-}" ]]; then
        printf '%s\n' "$HOGD_DEV_BINARY"
        return
    fi
    if [[ -n "${GROUNDHOG_HOGD_DEV_BINARY:-}" ]]; then
        printf '%s\n' "$GROUNDHOG_HOGD_DEV_BINARY"
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

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/groundhog-sim-smoke.XXXXXX")"
NODE_PID=""

cleanup() {
    if [[ -n "$NODE_PID" ]] && kill -0 "$NODE_PID" >/dev/null 2>&1; then
        kill -TERM "$NODE_PID" >/dev/null 2>&1 || true
        wait "$NODE_PID" >/dev/null 2>&1 || true
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

log "building groundhog with dev-node feature"
cargo build -p groundhog-cli --features dev-node
[[ -x "$GROUNDHOG_BINARY" ]] || die "groundhog binary is not executable: $GROUNDHOG_BINARY"

log "running local-provider smoke"
GROUNDHOG_SIM_HOGD_DEV_BINARY="$HOGD_DEV_BINARY" \
GROUNDHOG_SIM_GROUNDHOG_BINARY="$GROUNDHOG_BINARY" \
    cargo test -p groundhog-sim --test local_target_smoke -- --ignored --nocapture

NODE_STDOUT="$WORKDIR/groundhog-node.json"
NODE_STDERR="$WORKDIR/groundhog-node.stderr"

log "starting temporary groundhog node for existing-provider smoke"
"$GROUNDHOG_BINARY" --json node \
    --hogd-dev-binary "$HOGD_DEV_BINARY" \
    --port 0 \
    >"$NODE_STDOUT" 2>"$NODE_STDERR" &
NODE_PID="$!"

if ! wait_for_node_json "$NODE_STDOUT"; then
    cat "$NODE_STDERR" >&2 || true
    die "groundhog node did not emit startup JSON"
fi

RPC_URL="$(json_field_from_file data.rpc_url "$NODE_STDOUT")"
RUNTIME_MANIFEST="$(json_field_from_file data.runtime_manifest.path "$NODE_STDOUT")"

log "running existing-provider smoke against $RPC_URL"
GROUNDHOG_SIM_RPC_URL="$RPC_URL" \
GROUNDHOG_SIM_RUNTIME_MANIFEST="$RUNTIME_MANIFEST" \
    cargo test -p groundhog-sim --test existing_target_smoke -- --ignored --nocapture

log "sim local smokes passed"
