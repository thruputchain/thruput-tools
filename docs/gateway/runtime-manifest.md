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
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

The script starts a local node, reads its runtime manifest, exercises the CLI
endpoint, starts a standalone gateway, and repeats the benchmark checks there.
See [benchmark scenarios](bench-scenarios.md) for heavier runs and saved results.

## Manual local loop

Use an installed CLI or build the binaries from this checkout. A write benchmark
must own fixture keys and supply their public initial allocations. Prepare
`bench-accounts.json` with keys derived from the benchmark namespace
`groundhog-bench:benchmark-fixtures-v1`; each account seed is SHA-256 of that
namespace followed by `:account-N`. The smoke script shows the executable setup.
These deterministic keys are disposable fixtures. In one terminal:

```sh
thruput --json node --bootstrap-accounts ./bench-accounts.json --hog-devnode-binary /absolute/path/to/hog-devnode \
  --block-driver interval --block-interval-ms 500 > /tmp/groundhog-node.json
```

Leave that process running. Interval block production matters when submitting
through a separate gateway: instant mode is triggered by the local node's own
RPC submit path. Wait for startup JSON before reading it in another terminal:

```sh
RUNTIME_MANIFEST=$(jq -er '.data.runtimeManifest.path' /tmp/groundhog-node.json)
hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
```

Leave the gateway running too. In a third terminal:

```sh
RUNTIME_MANIFEST=$(jq -er '.data.runtimeManifest.path' /tmp/groundhog-node.json)
thruput-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" --account-seed benchmark-fixtures-v1 \
  --method sendTransaction --accounts 3 --rate 3 --duration 2 --force
```

This submits real test transactions. `--force` explicitly authorizes the submit
benchmark; JSON or non-interactive output does not provide consent. Read-only
benchmark methods do not need submission confirmation.

The node writes its endpoint and manifest path into global `local_runtime_state`.
Profiles with `network = "local"` consume that state; startup does not rewrite
the selected profile. Scripts can pass an explicit runtime manifest or use
`THRUPUT_RUNTIME_MANIFEST`. Resolution and override precedence live in
[CLI configuration](../../crates/cli/src/config/project.rs).

## Readiness and trust

`hog-devnode` reports a bounded stdout readiness pointer to a manifest and its
hash. The supervisor verifies the file and schema before connecting. Readiness
includes the producer's recovered/fresh chain state; directory existence alone
is not evidence of recovery. Ordinary child logs go to stderr and are forwarded
through Rust tracing, leaving stdout available for startup protocol.

Local runtime and bootstrap manifests use schema 5; the readiness pointer uses
schema 4. Network hashes, public keys, addresses, storage-cost model hashes and
the readiness manifest checksum use unprefixed, case-sensitive base58. Selectors
and opaque payloads retain hex. Local data-directory metadata uses schema 5;
older directories require their original runtime and tools or a fresh directory.
Local accounts have public keys, addresses and initial raw allocations,
with no derivation seed or account indices. The supervisor owns the private
faucet key; users and harnesses own their signing keys. Hosted Devnet retains its
separate schema-4 deterministic adapter. Every native account has nonce zero and
an address matching its public key. Bootstrap metadata contains the storage cost
model; native wallets need no RV32 artifact.

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

## Native RPC qualification

The example starts an ephemeral node with the native wire-14 API. It requires a
compatible real runtime and the private registry access described in the README:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  cargo run -p thruput-node-supervisor --locked --example native_rpc_qualification
```

The endpoint uses loopback and an operating-system assigned port. Query
`getVersionInfo` for the configured methods. History requires runtime and bindings
from the same protocol revision, including typed cursor outcomes. Ctrl-C stops the
listener and runtime. The example checks API behavior, not production capacity;
if the runtime exits early, stop the example with Ctrl-C.

Build the CLI with `cargo build -p thruput-cli`. Use the printed URL for reads:

```sh
target/debug/thruput tx list ADDRESS --rpc-url RPC_URL --json
target/debug/thruput events list ADDRESS --rpc-url RPC_URL --json
```

Each command fetches one newest-first page. Continue with `--cursor` and the same
address and filters. RPC pages use nullable `nextCursor`; CLI v2 page reports use
an empty cursor as their terminal marker. An empty event page with a nonempty
cursor is not exhausted.

## State and shutdown

Without `--data-dir`, the node is ephemeral. With a persistent directory, startup
and shutdown must preserve exclusive ownership of that chain's state. The
supervisor records process ownership, requests graceful shutdown, and escalates
only when the child group does not exit. Dropping a handle is not equivalent to
awaiting shutdown. An interrupted shutdown must not release ownership as though
all descendants were confirmed stopped.

Use the [CLI node reference](../../crates/cli/README.md#thruput-node) for
runtime selection, logging, persistence, and supported overrides. The lifecycle
implementation is in [process.rs](../../crates/node-supervisor/src/process.rs)
and [pid_file.rs](../../crates/node-supervisor/src/pid_file.rs).

`thruput node --init-network-hash` selects a new chain's bootstrap signing domain.
Distinct ports alone do not create distinct networks. Omission initializes zero for
a new chain; a restart loads the identity from the existing chain-bound funding
journal. Initialized directories reject the initialization flag, including a matching
hash. The supervisor checks runtime and scheduler identities against the initialized
or recovered identity. Use the emitted manifest with its matching RPC URL
for signing/funding consumers; see the CLI reference for the two-node example.

Older local runtimes and state directories require their original runtime and tooling. A native
runtime refuses old metadata before opening the database and preserves the data.
Use a new data directory for an ordinary-account chain; saved legacy wallet identities remain
explicit CLI bindings. Consumer and catalog promotion must follow publication of
the matching native producer.

The chain directory also owns a private faucet credential and retained funding
journal. Missing, corrupt or mismatched private state fails startup. Dump/restore
preserves both files and their private permissions. The supervisor and runtime
retain one exclusive chain-directory lock through startup and shutdown.
