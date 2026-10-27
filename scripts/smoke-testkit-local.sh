#!/usr/bin/env bash
#
# thruput-testkit live-contract smoke-test
#
# Builds the caller-counter fixture from thruput-examples and runs the
# (usually ignored) testkit contract smoke-test against a real hog-devnode.
#
# (The test's `local` provider launches and owns the node itself, so this
# script just builds the fixture and sets the environment.)
#
# REQUIRES:
# - thruput CLI and installed toolchain
# - the thruput-examples repo, adjacent to this one

set -euo pipefail

log() { printf '==> %s\n' "$*" >&2; }
die() { printf '!! %s\n' "$*" >&2; exit 1; }

THRUPUT_DEVNODE_BINARY=$(thruput toolchain which)
export THRUPUT_DEVNODE_BINARY

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

REPO_VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")"
THRUPUT_VERSION="$(thruput --version | awk '{print $2}')"
if [ "$THRUPUT_VERSION" != "$REPO_VERSION" ]; then
    log "warning: thruput on PATH is $THRUPUT_VERSION, but this repo is $REPO_VERSION"
fi

EXAMPLES_ROOT="$(realpath "$REPO_ROOT/../thruput-examples")"
[ -d "$EXAMPLES_ROOT" ] || die "Couldn't find thruput-examples"

cleanup() {
    if [[ "${KEEP_WORKDIR:-0}" != "1" ]]; then
        log "removing workdir"
        rm -rf "$WORKDIR"
    else
        log "kept workdir: $WORKDIR"
    fi
}
if [ -z "${WORKDIR:-}" ]; then
    WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/thruput-testkit-local.XXXXXX")"
    trap cleanup EXIT
fi

TARGET_DIR="$WORKDIR"
export THRUPUT_TESTKIT_ARTIFACT_ROOT="${TARGET_DIR}/thruput"

log "building caller-counter fixture"
(cd "$EXAMPLES_ROOT" && thruput build -p caller-counter --target-dir "$TARGET_DIR")

run_test() {
    test_crate="$1"
    test_name="$2"
    cargo test \
        -p thruput-testkit \
        --test "$test_crate" "$test_name" \
        -- --ignored --nocapture
}

log "running native testkit smoke with explicit account funding and atomic initialization"
run_test contract_smoke caller_counter_deploy_call_read

log "running testkit manual block-production smoke"
run_test contract_smoke manual_block_production_stages_deploys_and_calls

log "running testkit cross-run registration smoke"
run_test contract_smoke later_run_does_not_recover_an_earlier_runs_pending_registration

log "running testkit repeated scenario smoke"
run_test contract_smoke multiple_runs_are_possible

log "running testkit two-agent live smoke"
run_test trivial_scenario local_two_agents_live_smoke

log "testkit live contract smoke passed"
