#!/usr/bin/env bash
# Make scaffolds under DIR build against this checkout's thruput-json-rpc-client.
#
# Scaffolds pin thruput-json-rpc-client to the CLI version, which is not on
# crates.io until the release publishes. Cargo applies a [patch] from a config
# file in any parent directory, so projects created under DIR use the local
# crate and the generated Cargo.toml stays as users get it. SDK and protocol
# crates still resolve from crates.io. The crates.io-only check runs after
# publication in publish.yml.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 DIR" >&2
    exit 2
fi
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Escape backslashes and double quotes for the TOML basic string.
client_path="$REPO_ROOT/crates/json-rpc/json-rpc-client"
client_path="${client_path//\\/\\\\}"
client_path="${client_path//\"/\\\"}"
mkdir -p "$1/.cargo"
cat > "$1/.cargo/config.toml" <<TOML
[patch.crates-io]
thruput-json-rpc-client = { path = "$client_path" }
TOML
