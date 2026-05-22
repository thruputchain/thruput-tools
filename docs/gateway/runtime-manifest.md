# Runtime Manifest Quickstart

The runtime manifest is the handoff from a running backend to Rust tools that
need to connect to it. `hogd-dev` writes the manifest after genesis,
dev-wallet installation, account funding, and endpoint binding are complete.

Use it for local benchmark runs:

- `groundhog node` consumes the manifest internally and reports its path in
  `--json` startup output.
- `groundhog-rpc --runtime-manifest <path>` starts the standalone gateway from
  the same backend endpoints.
- `groundhog-bench --runtime-manifest <path>` derives the bootstrapped account
  signers and submits auth-valid `sendRawTransaction` traffic.

## Fast Smoke

Run the end-to-end local benchmark smoke:

```bash
HOGD_DEV_BINARY=/path/to/hogd-dev scripts/smoke-runtime-manifest-bench.sh
```

The script:

1. Builds `groundhog`, `groundhog-rpc`, and `groundhog-bench`.
2. Starts `groundhog node` against the `hogd-dev` binary.
3. Reads `data.runtime_manifest.path` from the CLI startup JSON.
4. Runs `groundhog-bench` against the CLI node for `chainTip` and
   `sendRawTransaction`.
5. Starts `groundhog-rpc --runtime-manifest <path>`.
6. Runs the same benchmark checks against the standalone gateway.

Useful knobs:

```bash
NODE_STARTUP_TIMEOUT_SECS=90 \
NODE_VERBOSE=1 \
BENCH_RATE=100 BENCH_DURATION=30 BENCH_ACCOUNTS=10 \
  HOGD_DEV_BINARY=/path/to/hogd-dev \
  scripts/smoke-runtime-manifest-bench.sh
```

Named benchmark scenarios live in the same runner:

```bash
BENCH_SCENARIO=gateway-baseline HOGD_DEV_BINARY=/path/to/hogd-dev \
  scripts/smoke-runtime-manifest-bench.sh

BENCH_SCENARIO=submit-ramp HOGD_DEV_BINARY=/path/to/hogd-dev \
  scripts/smoke-runtime-manifest-bench.sh
```

See [`bench-scenarios.md`](bench-scenarios.md) for the scenario library,
defaults, and the current local reference run.

## Manual CLI Node Loop

Start the local node and capture the startup JSON:

```bash
cargo run -p groundhog-cli --features dev-node -- \
  --json node \
  --port 9545 \
  --hogd-dev-binary /path/to/hogd-dev \
  > /tmp/groundhog-node.json
```

Extract the RPC URL and manifest path:

```bash
RPC_URL=$(jq -r '.data.rpc_url' /tmp/groundhog-node.json)
RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/groundhog-node.json)
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

## Manual Gateway Loop

Start the standalone gateway from the same manifest:

```bash
cargo run -p groundhog-rpc-gateway -- \
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

CLI flags on `groundhog-rpc` are still explicit overrides. For example,
`--scheduler` can intentionally replace the manifest scheduler endpoint. TOML
configuration conflicts with manifest backend fields are rejected so a benchmark
run does not silently target a mixed topology.

## Example Manifest

See
[`examples/runtime-manifest.hogd-dev.example.json`](examples/runtime-manifest.hogd-dev.example.json)
for a complete v1 shape.

That file is a shape example, not a runnable manifest. A runnable manifest must
come from a live producer because these fields are runtime state:

- `endpoints` contain bound ports.
- `genesisHash` identifies the materialized genesis.
- `bootstrapAccounts[].walletAddress` must match the deployed dev wallets.
- `bootstrap.devWallet.*Hash` and `bootstrap.storageCostModel.hash` must match
  the backend that produced the chain.
- `manifestHash` lives in the readiness pointer and is computed over the exact
  manifest file bytes.

## Contract

Readiness stdout is a single bounded JSON pointer line:

```json
{
  "schemaVersion": 1,
  "status": "ready",
  "manifestPath": "/tmp/hogd-dev/runtime-manifest.json",
  "manifestHash": "0x..."
}
```

Consumers verify `manifestHash` before parsing the file. On Unix, the Rust
supervisor also requires an absolute manifest path, rejects symlinks, requires
the manifest file to be owner-only, and requires the parent directory to be
owner-only.

Optional endpoints are absent when unavailable. They are never emitted as
`null`.
