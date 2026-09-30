# Local node and runtime manifests

The runtime manifest is the handoff from a running backend to the tools that
connect to it. Bootstrap describes requested initial state; the runtime manifest
describes what actually started. The supervisor verifies readiness before
serving the public JSON-RPC interface.

The process boundary keeps execution, auth, mempool, and chain state in
`hog-devnode`. Rust owns child lifecycle and the public RPC facade. This avoids
reimplementing node semantics in a development-only backend.

## Fast smoke

From the repository root, with private registry access and a compatible runtime:

```sh
GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

The script starts a local node, reads its runtime manifest, exercises the CLI
endpoint, starts a standalone gateway, and repeats the benchmark checks there.
See [benchmark scenarios](bench-scenarios.md) for heavier runs and saved results.

## Manual local loop

Use an installed CLI or build the binaries from this checkout. In one terminal:

```sh
groundhog --json node --hog-devnode-binary /absolute/path/to/hog-devnode \
  --block-driver interval --block-interval-ms 500 > /tmp/groundhog-node.json
```

Leave that process running. Interval block production matters when submitting
through a separate gateway: instant mode is triggered by the local node's own
RPC submit path. Wait for startup JSON before reading it in another terminal:

```sh
RUNTIME_MANIFEST=$(jq -er '.data.runtime_manifest.path' /tmp/groundhog-node.json)
hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
```

Leave the gateway running too. In a third terminal:

```sh
RUNTIME_MANIFEST=$(jq -er '.data.runtime_manifest.path' /tmp/groundhog-node.json)
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction --accounts 3 --rate 3 --duration 2 --force
```

This submits real test transactions. `--force` explicitly authorizes the submit
benchmark; JSON or non-interactive output does not provide consent. Read-only
benchmark methods do not need submission confirmation.

The node writes its endpoint and manifest path into global `local_runtime_state`.
Profiles with `network = "local"` consume that state; startup does not rewrite
the selected profile. Scripts can pass an explicit runtime manifest or use
`GROUNDHOG_RUNTIME_MANIFEST`. Resolution and override precedence live in
[CLI configuration](../../crates/cli/src/config/project.rs).

## Readiness and trust

`hog-devnode` reports a bounded stdout readiness pointer to a manifest and its
hash. The supervisor verifies the file and schema before connecting. Readiness
includes the producer's recovered/fresh chain state; directory existence alone
is not evidence of recovery. Ordinary child logs go to stderr and are forwarded
through Rust tracing, leaving stdout available for startup protocol.

The manifest carries bound endpoints, network identity, and bootstrap account
metadata. Do not construct a runnable manifest by copying an example's ports,
wallet addresses, or hashes. These are properties of the running producer.
The checked-in [runtime](examples/runtime-manifest.hogd-dev.example.json) and
[bootstrap](examples/bootstrap-manifest.local-dev.example.json) JSON examples
illustrate the schema; tests may also use their exact bytes as fixtures.

`schedulerGateway` names the public-submit service. `scheduler` names internal
coordinator control. A missing public-submit endpoint cannot be repaired by
substituting the coordinator endpoint.

A direct configured manifest path has no readiness-pointer hash to verify.
Gateway and benchmark tools treat it as trusted local configuration and validate
its bounded contents. Do not expose these files as a public discovery mechanism.

Schema definitions and parsing are in
[runtime_manifest.rs](../../crates/json-rpc/json-rpc-types/src/runtime_manifest.rs)
and [bootstrap_manifest.rs](../../crates/json-rpc/json-rpc-types/src/bootstrap_manifest.rs).
The supervisor's validation and process protocol are in
[hogd_dev.rs](../../crates/node-supervisor/src/hogd_dev.rs).

## State and shutdown

Without `--data-dir`, the node is ephemeral. With a persistent directory, startup
and shutdown must preserve exclusive ownership of that chain's state. The
supervisor records process ownership, requests graceful shutdown, and escalates
only when the child group does not exit. Dropping a handle is not equivalent to
awaiting shutdown. An interrupted shutdown must not release ownership as though
all descendants were confirmed stopped.

Use the [CLI node reference](../../crates/cli/README.md#groundhog-node) for
runtime selection, logging, persistence, and supported overrides. The lifecycle
implementation is in [process.rs](../../crates/node-supervisor/src/process.rs)
and [pid_file.rs](../../crates/node-supervisor/src/pid_file.rs).
