# Connect a gateway to a local node

A runtime manifest describes a running backend's service endpoints, network
identity, and public bootstrap accounts. The local node emits its path after
startup. A standalone gateway can use it to connect to the same backend.

For CLI installation and a first contract, use the [repository README](../../README.md).

## Start the node and gateway

Build the gateway from the repository root:

```sh
cargo build --locked -p thruput-json-rpc-gateway --bin hog-gateway
```

In one terminal, start a local node and save its startup report:

```sh
thruput --json node --ramdb-bytes 1073741824 \
  --block-driver interval --block-interval-ms 500 > /tmp/thruput-node.json
```

This uses an installed runtime bundle. For a runtime built from source, pass
`--hog-devnode-binary /absolute/path/to/hog-devnode`. Leave the node running.
Interval block production allows submissions through the separate gateway;
instant mode is triggered by submissions through the local node's own RPC.

After the startup report appears, run this from the repository root in another
terminal. It requires `jq`:

```sh
RUNTIME_MANIFEST=$(jq -er '.data.runtimeManifest.path' /tmp/thruput-node.json)
target/debug/hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" \
  --listen 127.0.0.1:9546
```

The gateway serves public RPC at `http://127.0.0.1:9546`. Use the
[CLI reference](../../crates/cli/README.md#thruput-node) for persistence, runtime
selection, and bootstrap allocations. Stop both foreground processes with Ctrl-C.

## Run a smoke test

With a compatible runtime, run:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

The script creates disposable benchmark accounts, starts a node and gateway, and
checks both endpoints. See [benchmark scenarios](bench-scenarios.md) for longer
runs and saved results. Write benchmarks need fixture keys with matching public
bootstrap allocations; the manifest contains no private keys. Use the script to
create these fixtures.

For history and duplicate-submission checks through both servers, build the
gateway as above, then run:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
THRUPUT_GATEWAY_BINARY="$PWD/target/debug/hog-gateway" \
  cargo test --locked -p thruput-node-supervisor --test real_block_queries \
  -- --ignored --test-threads=1
```

The [tests](../../crates/node-supervisor/tests/real_block_queries.rs) define the
cases covered. These checks need a real runtime and local process access; see
the [README prerequisites](../../README.md#contributing).

## Manifest ownership

Bootstrap input requests initial state; the runtime manifest reports what
started. Use the producer's manifest. Copying example ports, addresses, or hashes
does not produce a working configuration.

The supervisor verifies the child's readiness pointer and manifest before
serving RPC. A manifest passed directly to a gateway is trusted local
configuration; it has no readiness-pointer checksum to verify. Keep it private.
The scheduler submit endpoint and coordinator control endpoint are distinct.

For schemas and validation, read
[runtime_manifest.rs](../../crates/json-rpc/json-rpc-types/src/runtime_manifest.rs),
[bootstrap_manifest.rs](../../crates/json-rpc/json-rpc-types/src/bootstrap_manifest.rs),
and the [readiness implementation](../../crates/node-supervisor/src/hogd_dev.rs).
