# hog-gateway

Standalone JSON-RPC 2.0 gateway for the [Groundhog](https://groundhog.dev) blockchain. Communicates with the validator stack (hogd indexer, hogd scheduler, chuckd coordinator) exclusively over x23 RPC.

JSON-RPC gateway crate within the Groundhog developer tools workspace.
Shapes the gateway architecture, backend boundaries, and public RPC types.

`groundhog_sendRawTransaction` is intentionally configurable to submit
through either `hogd` or `chuckd`. That choice is a deployment/topology
decision, not a consensus decision; the gateway remains the primary
public admission-control boundary in either mode.

## Crates

| Crate | Description |
|-------|-------------|
| `json-rpc-types` | Shared RPC types (receipts, status, block info, events, errors) |
| `json-rpc-gateway` | jsonrpsee server binary — handlers, middleware, config |
| `json-rpc-client` | Typed JSON-RPC client generated from the same trait as the server |

## Phase 1 API (7 methods)

```
groundhog_sendRawTransaction(bytes)            → TxHash
groundhog_getTransactionReceipt(hash)          → Option<TransactionReceipt>
groundhog_getTransactionStatus(hash)           → TransactionStatus
groundhog_chainTip()                           → ChainTip
groundhog_getBlockByNumber(height)             → Option<BlockInfo>
groundhog_pendingTransactionCount()            → u64
groundhog_getEvents(EventQuery)                → EventPage
```

## Building

Builds against the private Groundhog registry configured in
`.cargo/config.toml`. The gateway now shares its public JSON-RPC trait
definitions with `json-rpc-client` and the local node through the `json-rpc-api`
crate.

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Runtime Manifest Mode

For local benchmark runs, start a backend producer first. `groundhog node
--json` supervises `hog-devnode` and reports the generated manifest path:

```bash
groundhog --json node --hog-devnode-binary /path/to/hog-devnode > /tmp/node.json
RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/node.json)
```

Start the standalone gateway from that manifest:

```bash
hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
```

Then drive benchmark traffic through the gateway:

```bash
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction
```

The manifest supplies scheduler, indexer, DbGateway, ViewGateway, and hogd
protocol configuration. CLI flags such as `--scheduler` or `--view-gateway`
remain explicit overrides; TOML backend fields that conflict with the manifest
are rejected.

For repeatable local runs, use the scenario runner:

```bash
BENCH_SCENARIO=gateway-baseline HOG_DEVNODE_BINARY=/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

See [`docs/gateway/bench-scenarios.md`](../../docs/gateway/bench-scenarios.md)
for available scenarios and defaults.

## Documentation

- [System Design](../../docs/gateway/system-design.md) — architecture, data flows, open questions
- [API Sketch](../../docs/gateway/api-sketch.md) — trait definition, types, handler examples
- [Runtime Manifest Quickstart](../../docs/gateway/runtime-manifest.md) — local CLI/gateway/bench handoff
- [Transaction Ingress and Proposer-Local Races](../../docs/gateway/tx-ingress-and-proposer-race.md) — code-verified threat model framing
- [Roadmap](../../docs/gateway/roadmap.md) — v1 testnet scope and later ingress evolution
