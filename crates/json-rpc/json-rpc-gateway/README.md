# hog-gateway

Standalone JSON-RPC 2.0 gateway for the [Groundhog](https://groundhog.dev) blockchain. Communicates with the validator stack (hogd scheduler, hogd indexer, and optional DbGateway/ViewGateway endpoints) exclusively over x23 RPC.

JSON-RPC gateway crate within the Groundhog developer tools workspace.
Shapes the gateway architecture, backend boundaries, and public RPC types.

`groundhog_sendRawTransaction` submits directly to hogd scheduler; the
gateway is the primary public admission-control boundary.

## Crates

| Crate | Description |
|-------|-------------|
| `json-rpc-types` | Shared RPC types (receipts, status, block info, events, errors) |
| `json-rpc-gateway` | jsonrpsee server binary — handlers, middleware, config |
| `json-rpc-client` | Typed JSON-RPC client generated from the same trait as the server |

## Public API

```
groundhog_sendRawTransaction(bytes)            → TxHash
groundhog_getTransactionReceipt(hash)          → Option<TransactionReceipt>
groundhog_getTransactionStatus(hash)           → TransactionStatus
groundhog_chainTip()                           → ChainTip
groundhog_getBlockByNumber(height)             → Option<BlockInfo>
groundhog_pendingTransactionCount()            → u64
groundhog_getEvents(EventQuery)                → EventPage
groundhog_getTransactionsByAddress(query)      → TransactionRefPage
groundhog_getStorageAt(address, key)            → StorageItemRead
groundhog_getBalance(address)                   → BalanceResponse
groundhog_view(request)                         → ViewResponse
groundhog_networkInfo()                         → NetworkInfo
groundhog_version()                             → VersionInfo
groundhog_requestAirdrop(request)               → AirdropAttemptResponse (optional)
```

Hosted deployments must configure immutable identity with `network_id` and
`genesis_hash`, or the equivalent `--network-id` and `--genesis-hash` flags.
Runtime-manifest mode derives local identity from the validated manifest.

Setting `faucet_url` or `--faucet-url` registers `groundhog_requestAirdrop`.
The gateway validates `GET /v1/network` against its configured identity before
forwarding requests. A reachable malformed or mismatched descriptor fails
startup. An unreachable faucet starts degraded, keeps readiness failed, and
rejects airdrops until validation succeeds.

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
BENCH_SCENARIO=gateway-baseline GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

See [`docs/gateway/bench-scenarios.md`](../../docs/gateway/bench-scenarios.md)
for available scenarios and defaults.

## Documentation

- [System Design](../../docs/gateway/system-design.md) — architecture, data flows, open questions
- [OpenRPC Snapshot](../open-rpc-spec/snapshots/groundhog-spec.json) — generated RPC-surface reference
- [Runtime Manifest Quickstart](../../docs/gateway/runtime-manifest.md) — local CLI/gateway/bench handoff
- [Transaction Ingress and Proposer-Local Races](../../docs/gateway/tx-ingress-and-proposer-race.md) — code-verified threat model framing
- [Roadmap](../../docs/gateway/roadmap.md) — v1 testnet scope and later ingress evolution
