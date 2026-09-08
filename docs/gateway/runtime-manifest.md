# Runtime Manifest Quickstart

The runtime manifest is the handoff from a running backend to Rust tools that
need to connect to it. `hog-devnode` writes the manifest after genesis,
dev-wallet installation, account funding, and endpoint binding are complete.

Use it for local benchmark runs:

- `groundhog node` consumes the manifest internally, reports its path in
  `--json` startup output, reports whether the chain was fresh or recovered,
  and writes the matching RPC URL and absolute manifest path into the selected
  profile.
- CLI commands that use `dev-N` keys resolve manifest paths in this order:
  `GROUNDHOG_RUNTIME_MANIFEST`, then `profile.<name>.runtime_manifest`.
- `hog-gateway --runtime-manifest <path>` starts the standalone gateway from
  the same backend endpoints.
- `groundhog-bench --runtime-manifest <path>` derives the bootstrapped account
  signers and submits auth-valid `sendRawTransaction` traffic. Submit benchmarks
  ask for confirmation after validating their local inputs. Pass `--force` for
  JSON output, scripts, or other non-interactive runs.

## Fast Smoke

Run the end-to-end local benchmark smoke:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode scripts/smoke-runtime-manifest-bench.sh
```

If the local `hog-devnode` binary advertises a newer `hogp/<hash>` than the Rust
protocol crates linked into `groundhog node`, set
`HOGD_PROTOCOL_OVERRIDE=hogp/<hash>`. The script passes it through as
`--hogd-protocol`; incompatible XDR shape changes still fail through the
generated clients.

The script:

1. Builds `groundhog`, `hog-gateway`, and `groundhog-bench`.
2. Starts `groundhog node` against the `hog-devnode` binary.
3. Reads `data.runtime_manifest.path` from the CLI startup JSON.
4. Runs `groundhog-bench` against the CLI node for `chainTip` and
   `sendRawTransaction`.
5. Starts `hog-gateway --runtime-manifest <path>`.
6. Runs the same benchmark checks against the standalone gateway.

Useful knobs:

```bash
NODE_STARTUP_TIMEOUT_SECS=90 \
NODE_VERBOSE=1 \
BENCH_RATE=100 BENCH_DURATION=30 BENCH_ACCOUNTS=10 \
  GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

Named benchmark scenarios live in the same runner:

```bash
BENCH_SCENARIO=gateway-baseline GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh

BENCH_SCENARIO=submit-ramp GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

See [`bench-scenarios.md`](bench-scenarios.md) for the scenario library,
defaults, and the current local reference run.

## Manual CLI Node Loop

Start the local node and capture the startup JSON:

```bash
cargo run -p groundhog-cli -- \
  --json node \
  --port 9545 \
  --hog-devnode-binary /path/to/hog-devnode \
  > /tmp/groundhog-node.json
```

Add `--hogd-protocol hogp/<hash>` when the local `hog-devnode` binary advertises a
newer protocol token than the currently published Rust protocol crates. The
override is only used for x23 intro handshakes. Real XDR shape mismatches still
surface as generated-client decode or RPC failures.

Extract the RPC URL and manifest path:

```bash
RPC_URL=$(jq -r '.data.rpc_url' /tmp/groundhog-node.json)
RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/groundhog-node.json)
```

When `groundhog node` starts, it writes the local RPC URL and runtime manifest
path into global `local_runtime_state`. Normal CLI commands in another terminal
can use a profile with `network = "local"` while the node is running:

```bash
groundhog deploy
groundhog call my-contract get
```

For shell scripts or commands outside the project tree, export the manifest
path explicitly:

```bash
export GROUNDHOG_RUNTIME_MANIFEST="$RUNTIME_MANIFEST"
```

Push read traffic:

```bash
cargo run -p groundhog-bench -- \
  --url "$RPC_URL" \
  --method chainTip \
  --rate 100 \
  --duration 30
```

Push submit traffic using the manifest-funded bootstrap accounts:

```bash
cargo run -p groundhog-bench -- \
  --url "$RPC_URL" \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction \
  --accounts 10 \
  --rate 100 \
  --duration 30
```

This prompts before generating or submitting transactions and includes warmup
traffic in the confirmation. Add `--force` when running non-interactively.

## Manual Gateway Loop

Start the standalone gateway from the same manifest:

```bash
cargo run -p groundhog-json-rpc-gateway -- \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --listen 127.0.0.1:9546
```

Then point bench at the gateway:

```bash
cargo run -p groundhog-bench -- \
  --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction \
  --accounts 10 \
  --rate 100 \
  --duration 30
```

This submit benchmark follows the same confirmation behavior.

CLI flags on `hog-gateway` are still explicit overrides. For example,
`--scheduler` can intentionally replace the manifest `schedulerGateway`
endpoint. TOML configuration conflicts with manifest backend fields are rejected
so a benchmark run does not silently target a mixed topology.

## Example Manifest

See
[`examples/runtime-manifest.hogd-dev.example.json`](examples/runtime-manifest.hogd-dev.example.json)
for a complete v2 shape.

That file is a shape example, not a runnable manifest. A runnable manifest must
come from a live producer because these fields are runtime state:

- `chainState` is `fresh` when this process initialized the chain and
  `recovered` only when hog-devnode actually recovered an existing committed
  chain. Data-directory presence alone does not make a chain recovered.
- `endpoints` contain bound ports.
- `bootstrapAccounts[].walletAddress` must match the deployed dev wallets.
- `bootstrap.devWallet.*Hash` and `bootstrap.storageCostModel.hash` must match
  the backend that produced the chain.
- `manifestHash` lives in the readiness pointer and is computed over the exact
  manifest file bytes.

## Contract

Readiness stdout is a single bounded JSON pointer line:

```json
{
  "schemaVersion": 2,
  "status": "ready",
  "manifestPath": "/tmp/hog-devnode/runtime-manifest.json",
  "manifestHash": "0x..."
}
```

Consumers reading a manifest through the readiness pointer verify
`manifestHash` before parsing the file. On Unix, the Rust supervisor also
requires an absolute manifest path, rejects symlinks, requires the manifest
file to be owner-only, and requires the parent directory to be owner-only.

Consumers configured with a direct `--runtime-manifest`,
`GROUNDHOG_RUNTIME_MANIFEST`, or `profile.<name>.runtime_manifest` path do not
have a readiness pointer hash to verify. The CLI direct loader rejects
symlink/non-file manifest paths before reading a bounded JSON file and
validating the schema. Gateway and bench direct paths perform bounded JSON
schema validation and treat the configured path as trusted local developer
configuration.

Optional endpoints are absent when unavailable. They are never emitted as
`null`.

The endpoint roles are deliberately separate: `scheduler` is the internal
`SchedulerCoord` service used for genesis and local block driving, while
`schedulerGateway` is the public `SchedGateway` service used for transaction
submission and scheduler reads. Gateway consumers require `schedulerGateway`;
they must not fall back to `scheduler`.

Runtime manifest schema 2 permits additive optional fields. `chainState` was
added under that rule, and `schedulerGateway` followed the same rule, so schema
2 is preserved. Current hog-devnode emits both and the local node supervisor
requires them for authoritative startup readiness.
