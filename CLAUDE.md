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
- Wire format: `0x`-prefixed lowercase hex for all byte values
  (hashes, addresses, topics, event data). Decimal JSON numbers for
  heights, counts, and other quantities. See system-design.md
  "Wire Format Conventions".
- Byte fields use `HexBytes` (variable-length) or `Topic` (32-byte
  fixed) from `api-types/src/hex.rs` — never raw `Vec<u8>` in
  RPC-facing types.
- `#[non_exhaustive]` on public enums that will gain variants
  (`TransactionStatus`, `ExecStatus`).
- The jsonrpsee trait in `gateway/src/api.rs` is the source of truth
  for the RPC surface. `#[rpc(client, server)]` generates both sides.
  (May move to `api-types` — see system-design.md "Client Crate Trait
  Location".)
- Handler implementations go in `gateway/src/handlers/`, one file per
  logical group (submit, receipt, blocks, events).
- The gateway enforces query bounds (e.g., max height range for
  `getContractEvents`) since hogd has none. Limits are in
  `GatewayConfig`.
- Error codes follow Substrate's per-module range pattern. Constants
  live in `api-types/src/error.rs`.
- Method prefix is `groundhog_` (may change — see open questions in
  system-design.md).

## Build Commands

```bash
cargo build                     # Build the full workspace
cargo test                      # Run all tests
cargo test -p groundhog-rpc-api-types  # Run tests for a specific crate
cargo check                     # Check without building
cargo clippy -- -D warnings     # Lint — must be clean
cargo fmt --check               # Format check — must pass
```

Won't compile until the private registry is configured and `groundhog-xdr`
is available. The `api.rs` trait definition is commented out for this reason.

## Code Style

### Error Handling

- `thiserror` for all per-crate error enums. **No `anyhow` in this project.**
- Never `.unwrap()` in library code. `.unwrap()` is fine in tests.
- `.expect("bug: reason")` only for true invariant violations (use `bug:`
  prefix for grep-ability).
- Propagate transient errors with `?`. Add context via error enum variants,
  not string chaining.
- Untrusted input (user-supplied data, peer responses) must never trigger
  a panic. Return `Result` or log and drop instead.

### Observability

- Use `#[tracing::instrument(skip_all, fields(...))]` on public async
  methods at handler and subsystem boundaries. Simple delegation and
  test helpers can skip it.
- Use `tracing::error!` / `tracing::warn!` for diagnostics, never
  `println!`.
- No `dbg!` or debug `println!` in committed code.

### Types and APIs

- Derive `Debug` on all public types.
- Add `Serialize, Deserialize` for types that cross the RPC boundary.
- Prefer `&str` and `&[u8]` over owned types in function parameters.
- When two crates have structurally similar types, use `From` impls so the
  compiler catches field additions instead of silently dropping data.
- All public items get `///` doc comments.

## Testing

- `api-types/src/hex.rs` has serde roundtrip tests for `HexBytes`
  and `Topic`.
- Unit tests for type conversions (XDR ↔ API types) in `api-types`
- Integration tests for handlers using mock x23 backends in `gateway`
- The `client` crate should have roundtrip tests against a test server
- Unit tests: inline `#[cfg(test)] mod tests` in the source file.
- Integration tests: `crates/{name}/tests/*.rs`.
- Async tests use `#[tokio::test]`.
- Follow Arrange-Act-Assert pattern.

## Git Workflow

- Use feature branches and PRs — never commit directly to main.
- `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check`
  must all pass before committing.

## Before Committing

- [ ] `cargo test` — all tests pass
- [ ] `cargo build` — no compiler warnings
- [ ] `cargo clippy -- -D warnings` — clean
- [ ] `cargo fmt --check` — formatted
- [ ] No `.unwrap()` added outside of tests
- [ ] No `.expect()` on untrusted input
- [ ] New `.expect()` messages use `bug:` prefix
- [ ] No debug print statements
- [ ] All new public items have doc comments

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
