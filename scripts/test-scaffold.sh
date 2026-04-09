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

    echo "==> $tmpl OK"
done

echo "==> All scaffold smoke tests passed."
