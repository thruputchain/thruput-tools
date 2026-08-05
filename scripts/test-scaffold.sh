#!/usr/bin/env bash
# Scaffold smoke test — verify that both templates produce projects
# that compile and pass tests against the real SDK.
#
# Requires: CARGO_REGISTRIES_GROUNDHOG_TOKEN (set in CI env).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Building CLI..."
cargo build -p groundhog-cli --manifest-path "$REPO_ROOT/Cargo.toml"

CLI="$REPO_ROOT/target/debug/groundhog"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

for tmpl in counter stub; do
    echo "==> Scaffolding template: $tmpl"
    cd "$WORK"
    "$CLI" new "test-$tmpl" --template "$tmpl"
    cd "test-$tmpl"

    echo "==> cargo check ($tmpl)"
    cargo check

    echo "==> cargo test ($tmpl)"
    cargo test

    echo "==> groundhog test ($tmpl)"
    "$CLI" test

    # A user-level rustflags override makes cargo drop the project's
    # .cargo/config.toml target rustflags; the CLI must refuse to build.
    echo "==> RUSTFLAGS guard ($tmpl)"
    for probe in 'RUSTFLAGS=-C opt-level=2' 'CARGO_ENCODED_RUSTFLAGS='; do
        if env "$probe" "$CLI" build > guard.log 2>&1; then
            echo "ERROR: groundhog build unexpectedly succeeded with $probe set" >&2
            cat guard.log >&2
            exit 1
        fi
        if ! grep -q "is set in the environment" guard.log; then
            echo "ERROR: groundhog build failed for a reason other than the rustflags guard with $probe set" >&2
            cat guard.log >&2
            exit 1
        fi
    done
    rm -f guard.log

    echo "==> $tmpl OK"
done

echo "==> All scaffold smoke tests passed."
