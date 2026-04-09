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
groundhog_getContractEvents(addr, from, to)    → Vec<IdentifiedEvent>
```

## Building

The eventual build uses the private Groundhog registry configured in
`.cargo/config.toml`, but the repo is still being used primarily as a stubbing
exercise to settle architecture and types.

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Documentation

- [System Design](../../docs/gateway/system-design.md) — architecture, data flows, open questions
- [API Sketch](../../docs/gateway/api-sketch.md) — trait definition, types, handler examples
- [Transaction Ingress and Proposer-Local Races](../../docs/gateway/tx-ingress-and-proposer-race.md) — code-verified threat model framing
- [Roadmap](../../docs/gateway/roadmap.md) — v1 testnet scope and later ingress evolution
