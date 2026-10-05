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

for tmpl in counter minimal; do
    echo "==> Scaffolding template: $tmpl"
    cd "$WORK"
    if [[ "$tmpl" == minimal ]]; then
        "$CLI" new "test-$tmpl"
    else
        "$CLI" new "test-$tmpl" --template "$tmpl"
    fi
    cd "test-$tmpl"

    echo "==> cargo check ($tmpl)"
    cargo check

    echo "==> cargo test ($tmpl)"
    cargo test

    echo "==> groundhog test ($tmpl)"
    "$CLI" test

    # Starts an ephemeral hog-devnode (installed from the embedded catalog on
    # first use), deploys the contract with a native local-user account, and
    # runs tests/integration.rs. This is the only path that exercises the
    # integration deploy end to end, so keep it even though it needs network.
    echo "==> groundhog test --integration ($tmpl)"
    "$CLI" test --integration

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

    # Wrapping a generated project into a parent workspace without adopting
    # its .cargo/config.toml makes cargo (run from the workspace root) drop
    # the Groundhog rustflags: no linker script means the rv32 build can
    # never produce a valid artifact, so the CLI must refuse with the
    # copy-the-config hint. The check asks `cargo config get` (nightly-only),
    # so the project's toolchain pin moves to the workspace root the way a
    # bootstrapper would. CARGO_HOME is pointed at an empty directory so a
    # developer's global cargo config cannot satisfy the check.
    echo "==> rv32 linker-script guard ($tmpl)"
    cd "$WORK"
    mkdir -p "wrapped-$tmpl" "empty-cargo-home"
    mv "test-$tmpl" "wrapped-$tmpl/test-$tmpl"
    printf '[workspace]\nmembers = ["test-%s"]\nresolver = "2"\n' "$tmpl" \
        > "wrapped-$tmpl/Cargo.toml"
    cp "wrapped-$tmpl/test-$tmpl/rust-toolchain.toml" "wrapped-$tmpl/"
    cd "wrapped-$tmpl/test-$tmpl"
    if CARGO_HOME="$WORK/empty-cargo-home" "$CLI" build > guard.log 2>&1; then
        echo "ERROR: groundhog build unexpectedly succeeded without the Groundhog rustflags" >&2
        cat guard.log >&2
        exit 1
    fi
    if ! grep -q 'no `-T` linker script' guard.log; then
        echo "ERROR: groundhog build failed for a reason other than the rv32 linker-script guard" >&2
        cat guard.log >&2
        exit 1
    fi
    rm -f guard.log

    echo "==> $tmpl OK"
done

echo "==> All scaffold smoke tests passed."
