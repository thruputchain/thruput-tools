# Groundhog RPC Gateway

Standalone JSON-RPC 2.0 gateway for the Groundhog blockchain. Separate
service — does NOT embed or depend on the coordinator (chuckd/chuckd-rs).
Communicates with the validator stack exclusively over x23 RPC.

## Architecture

See `docs/system-design.md` for full design and `docs/api-sketch.md`
for the jsonrpsee trait definition and type sketches.

Three crates:
- **api-types** — shared RPC types (receipt, status, block info, events, errors).
  Consumed by gateway, client, SDK, CLI, block explorer.
- **gateway** — jsonrpsee server binary. Handlers call hogd indexer and
  scheduler over x23 RPC.
- **client** — typed JSON-RPC client generated from the same `#[rpc(client)]`
  trait as the server. Wraps api-types.

Data flows:
- **Tx submission** → hogd scheduler or chuckd validator (tbd) (`submit_unvetted`) via x23
- **Receipts/events** → hogd indexer (`get_transaction_info`, `get_contract_events`) via x23
- **Chain state** → hogd scheduler (`get_tip`, `find_tx`, `size`) via x23
- **Block data** → coordinator storage (TBD — open question in system-design.md)

## Dependencies

All Groundhog crates come from the private registry (configured in
`.cargo/config.toml`). No git submodules.

Key private deps: `groundhog-xdr` (wire types), `xdrs` (XDR serialization),
`rpc_driver` (x23 RPC framework).

Key public deps: `jsonrpsee` (JSON-RPC), `axum` (HTTP), `tokio` (async
runtime), `tower`/`tower-http` (middleware), `clap` (CLI), `prometheus`
(metrics).

## Conventions

- Types that cross the RPC boundary live in `api-types`, not in handler code.
- `api-types` imports from `groundhog-xdr` for wire types and adds
  RPC-layer types (receipts, status enums, error codes). Don't duplicate
  XDR types — re-export them.
- JSON field names use `camelCase` (serde `rename_all`). Enums use
  tagged serialization (`#[serde(tag = "...")]`).
- The jsonrpsee trait in `gateway/src/api.rs` is the single source of truth
  for the RPC surface. `#[rpc(client, server)]` generates both sides.
- Handler implementations go in `gateway/src/handlers/`, one file per
  logical group (submit, receipt, blocks, events).
- Error codes follow Substrate's per-module range pattern. Constants
  live in `api-types/src/error.rs`.
- Method prefix is `groundhog_` (may change — see open questions in
  system-design.md).

## Building

Won't compile until the private registry is configured and `groundhog-xdr`
is available. The `api.rs` trait definition is commented out for this reason.

To get started:
1. Set the registry URL in `.cargo/config.toml`
2. Uncomment the trait in `gateway/src/api.rs`
3. `cargo check` to verify deps resolve

## Testing

No tests yet. When adding:
- Unit tests for type conversions (XDR ↔ API types) in `api-types`
- Integration tests for handlers using mock x23 backends in `gateway`
- The `client` crate should have roundtrip tests against a test server

## Context

- Groundhog is an L1 blockchain (SCP consensus, WASM execution, NNInt
  storage). Instant finality, no forks, no gas fees.
- Transaction lifecycle is simpler than Ethereum/Substrate: Pending →
  Committed (or Invalid/Expired). No reorgs, no "included but not
  finalized".
- The hogd indexer (C++, RocksDB) already has working query APIs. The
  gateway's job is putting a standard JSON-RPC interface in front of them.
- This repo is Phase 1 of the developer-facing API effort. Phase 2 adds
  WebSocket subscriptions, Phase 3 adds SDK client + CLI, Phase 4 adds
  production hardening (rate limiting, pagination, DenyUnsafe).
