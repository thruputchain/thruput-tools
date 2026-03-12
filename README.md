# groundhog-rpc

Standalone JSON-RPC 2.0 gateway for the [Groundhog](https://groundhog.dev) blockchain. Communicates with the validator stack (hogd indexer, hogd scheduler, chuckd coordinator) exclusively over x23 RPC.

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

Requires the private Groundhog registry configured in `.cargo/config.toml`.

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Documentation

- [System Design](docs/system-design.md) — architecture, data flows, open questions
- [API Sketch](docs/api-sketch.md) — trait definition, types, handler examples
