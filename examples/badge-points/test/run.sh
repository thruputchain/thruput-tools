#!/usr/bin/env bash
# Runs the badge app's tests on a desktop, with the badge firmware's own Lua.
#
#   test/run.sh                      encoding tests only
#   POINTS_CONTRACT=<address> POINTS_RPC=http://127.0.0.1:9545 test/run.sh
#                                    also drives the app against that node
#
# LUA_VENDOR is the firmware's Lua sources (solana-os/main/lua/vendor). Without
# it, the firmware repository is cloned into test/build/.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="$here/build"
mkdir -p "$build"

vendor="${LUA_VENDOR:-}"
if [ -z "$vendor" ]; then
    if [ ! -d "$build/solana-os" ]; then
        git clone --depth 1 https://github.com/solana-foundation/solana-os.git "$build/solana-os"
    fi
    vendor="$build/solana-os/main/lua/vendor"
fi

if [ ! -x "$build/host" ] || [ "$here/host.c" -nt "$build/host" ]; then
    cc -O1 -w -I"$vendor" -o "$build/host" "$here/host.c" "$vendor"/*.c -lm
fi

"$build/host" "$here/test.lua" "$here/../app" "$here"
