# groundhog-rpc

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
| `api-types` | Shared RPC types (receipts, status, block info, events, errors) |
| `gateway` | jsonrpsee server binary — handlers, middleware, config |
| `client` | Typed JSON-RPC client generated from the same trait as the server |

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
definitions with the client and local node through the `rpc-api`
crate.

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Runtime Manifest Mode

For local benchmark runs, start a backend producer first. `groundhog node
--json` supervises `hogd-dev` and reports the generated manifest path:

```bash
groundhog --json node --hogd-dev-binary /path/to/hogd-dev > /tmp/node.json
RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/node.json)
```

Start the standalone gateway from that manifest:

```bash
groundhog-rpc --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
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
BENCH_SCENARIO=gateway-baseline HOGD_DEV_BINARY=/path/to/hogd-dev \
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
