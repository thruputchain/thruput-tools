#!/usr/bin/env bash
# Make scaffolds under DIR build against this checkout's thruput-json-rpc-client.
#
# Scaffolds pin thruput-json-rpc-client to the CLI version, which is not in the
# registry until the release publishes. Cargo applies a [patch] from a config
# file in any parent directory, so projects created under DIR use the local
# crate and the generated Cargo.toml stays as users get it. SDK and protocol
# crates still resolve from the registry. The registry-only check runs after
# publication in publish.yml.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 DIR" >&2
    exit 2
fi
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$1/.cargo"
cat > "$1/.cargo/config.toml" <<TOML
[patch.thruput]
thruput-json-rpc-client = { path = "$REPO_ROOT/crates/json-rpc/json-rpc-client" }
TOML
