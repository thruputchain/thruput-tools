#!/usr/bin/env bash
#
# groundhog-testkit live-contract smoke-test
#
# Builds the caller-counter fixture from groundhog-examples and runs the
# (usually ignored) testkit contract smoke-test against a real hog-devnode.
#
# (The test's `local` provider launches and owns the node itself, so this
# script just builds the fixture and sets the environment.)
#
# Required environment:
#   GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode (must support native deploy funding + initialization)
#
# Optional environment:
#   GROUNDHOG_BINARY=/path/to/groundhog
#   GROUNDHOG_EXAMPLES_ROOT=/path/to/groundhog-examples
#   ARTIFACT_ROOT=/path/to/prebuilt/target/groundhog
#   HOGD_PROTOCOL_OVERRIDE=hogp/<hash>
#   GROUNDHOG_TESTKIT_CONTRACT_ARTIFACTS_ROOT=/path/to/devnode/built_contracts
#   KEEP_WORKDIR=1

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

expand_user_path() {
    case "$1" in
        "~") printf '%s\n' "$HOME" ;;
        "~/"*) printf '%s/%s\n' "$HOME" "${1#"~/"}" ;;
        *) printf '%s\n' "$1" ;;
    esac
}

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

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

detect_hogd_protocol() {
    local binary="$1"
    if [[ -n "${HOGD_PROTOCOL_OVERRIDE:-}" ]]; then
        printf '%s\n' "$HOGD_PROTOCOL_OVERRIDE"
        return
    fi
    local output
    output="$("$binary" --help 2>&1 || true)"
    python3 -c 'import re, sys
text = re.sub(r"\x1b\[[0-9;]*m", "", sys.stdin.read())
matches = re.findall(r"hogd protocol: (hogp/[A-Za-z0-9]+)", text)
if matches:
    print(matches[-1])
' <<< "$output"
}

[[ -n "${GROUNDHOG_DEVNODE_BINARY:-}" ]] || die "set GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode"
DEVNODE_BINARY="$(expand_user_path "$GROUNDHOG_DEVNODE_BINARY")"
[[ -x "$DEVNODE_BINARY" ]] || die "hog-devnode binary is not executable: $DEVNODE_BINARY"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/groundhog-testkit-local.XXXXXX")"
cleanup() {
    if [[ "${KEEP_WORKDIR:-0}" != "1" ]]; then
        rm -rf "$WORKDIR"
    else
        log "kept workdir: $WORKDIR"
    fi
}
trap cleanup EXIT

if [[ -n "${ARTIFACT_ROOT:-}" ]]; then
    ARTIFACT_ROOT="$(expand_user_path "$ARTIFACT_ROOT")"
    log "using artifact root: $ARTIFACT_ROOT"
else
    EXAMPLES_ROOT="$(find_examples_root)"
    [[ -n "$EXAMPLES_ROOT" ]] || die "set GROUNDHOG_EXAMPLES_ROOT or provide a prebuilt ARTIFACT_ROOT"
    if [[ -n "${GROUNDHOG_BINARY:-}" ]]; then
        GROUNDHOG_BINARY="$(expand_user_path "${GROUNDHOG_BINARY}")"
        log "using groundhog binary: $GROUNDHOG_BINARY"
    else
        log "building groundhog"
        cargo build -p groundhog-cli
        GROUNDHOG_BINARY="$REPO_ROOT/target/debug/groundhog"
    fi
    [[ -x "$GROUNDHOG_BINARY" ]] || die "groundhog binary is not executable: $GROUNDHOG_BINARY"

    EXAMPLES_TARGET="$WORKDIR/groundhog-examples-target"
    log "building groundhog-examples caller-counter fixture"
    (
        cd "$EXAMPLES_ROOT"
        "$GROUNDHOG_BINARY" build -p caller-counter --target-dir "$EXAMPLES_TARGET"
    )
    ARTIFACT_ROOT="$EXAMPLES_TARGET/groundhog"
fi
[[ -f "$ARTIFACT_ROOT/caller_counter.rv32a" ]] || die "caller_counter.rv32a not found under $ARTIFACT_ROOT"

HOGD_PROTOCOL="$(detect_hogd_protocol "$DEVNODE_BINARY")"
ENV_ARGS=(
    "GROUNDHOG_DEVNODE_BINARY=$DEVNODE_BINARY"
    "GROUNDHOG_TESTKIT_ARTIFACT_ROOT=$ARTIFACT_ROOT"
)
if [[ -n "$HOGD_PROTOCOL" ]]; then
    ENV_ARGS+=("HOGD_PROTOCOL_OVERRIDE=$HOGD_PROTOCOL")
    log "using hogd protocol override: $HOGD_PROTOCOL"
fi

log "running native testkit smoke with explicit account funding and atomic initialization"
env "${ENV_ARGS[@]}" \
    cargo test -p groundhog-testkit --test contract_smoke caller_counter_deploy_call_read -- --ignored --nocapture

log "running testkit manual block-production smoke"
env "${ENV_ARGS[@]}" \
    cargo test -p groundhog-testkit --test contract_smoke manual_block_production_stages_deploys_and_calls -- --ignored --nocapture

log "running testkit cross-run registration smoke"
env "${ENV_ARGS[@]}" \
    cargo test -p groundhog-testkit --test contract_smoke later_run_does_not_recover_an_earlier_runs_pending_registration -- --ignored --nocapture

log "testkit live contract smoke passed"
