# Groundhog Developer Tools

Unified workspace for Groundhog blockchain developer tooling: a JSON-RPC
gateway, typed client, transaction building, key management, and CLI.

## Architecture

Six crates in `crates/`:

- **api-types** — shared RPC types (receipt, status, block info, events,
  errors). Consumed by gateway, client, CLI, and external tools.
- **gateway** — jsonrpsee server binary. Handlers call hogd indexer and
  scheduler over x23 RPC.
- **client** — typed JSON-RPC client generated from the same
  `#[rpc(client)]` trait as the server. Wraps api-types.
- **signing** — Ed25519 key management, XDG key storage, zeroize-backed
  secret handling.
- **tx-builder** — transaction construction and XDR serialization.
- **cli** — `groundhog` binary. Wraps the Rust/WASM build and test
  workflow. Lib/bin split: all logic in the library crate, `main.rs` is
  pure dispatch. Subcommand functions take plain Rust types, not clap
  structs.

See `docs/gateway/system-design.md` for full gateway design and
`docs/gateway/api-sketch.md` for the jsonrpsee trait definition.

### Gateway Data Flows

- **Tx submission** → configurable: hogd scheduler or chuckd coordinator (`submit_unvetted`) via x23
- **Receipts/events** → hogd indexer (`get_transaction_info`, `get_contract_events`) via x23
- **Chain state** → hogd scheduler (`get_tip`, `find_tx`, `size`) via x23
- **Block data** → coordinator storage (TBD — see docs/gateway/system-design.md)

The submit target is a topology choice, not a change in external
gateway semantics. `groundhog_sendRawTransaction` should only return
success once the authoritative downstream mempool path accepted the tx.

### Dependencies

All Groundhog crates come from the private registry (configured in
`.cargo/config.toml`). No git submodules.

Key private deps: `groundhog-xdr` (wire types), `xdrs` (XDR
serialization), `rpc_driver` (x23 RPC framework).

Key public deps: `jsonrpsee` (JSON-RPC), `axum` (HTTP), `tokio` (async
runtime), `tower`/`tower-http` (middleware), `clap` (CLI), `prometheus`
(metrics).

## Build and Test

### Workspace

```bash
cargo build --workspace          # build everything
cargo test --workspace           # run all tests
cargo clippy --workspace -- -D warnings   # lint (must be clean)
cargo fmt --check                # format check (must pass)
```

### Per-crate

```bash
cargo test -p groundhog-rpc-api-types    # test a specific crate
cargo test -p groundhog-cli              # CLI tests
```

### Running the CLI locally

```bash
cargo run -p groundhog-cli -- --help
cargo run -p groundhog-cli -- init
cargo run -p groundhog-cli -- new my-counter
cargo run -p groundhog-cli -- new my-stub --template stub
cargo run -p groundhog-cli -- build        # from inside a project
cargo run -p groundhog-cli -- test         # requires login first
cargo run -p groundhog-cli -- keys generate alice
cargo run -p groundhog-cli -- keys ls
```

## Code Style

- **Edition 2024**, stable Rust.
- **79-column max width** (`rustfmt.toml` enforces this).
- **`thiserror`** for all error enums. **No `anyhow` in this project.**
- **No `.unwrap()` in library code.** Return `Result`. `.unwrap()` is
  fine in tests.
- **`.expect("bug: reason")` only** for true invariant violations (use
  `bug:` prefix for grep-ability).
- **No `.expect()` on untrusted input.** Return `Result` or log and
  drop instead.
- **No `dbg!()` or debug prints** in committed code.
- **`///` doc comments on public items.** Obvious one-liners are exempt.
- Derive `Debug` on all public types.
- Add `Serialize, Deserialize` for types that cross the RPC boundary.
- Prefer `&str` and `&[u8]` over owned types in function parameters.
- When two crates have structurally similar types, use `From` impls so
  the compiler catches field additions instead of silently dropping data.
- Propagate transient errors with `?`. Add context via error enum
  variants, not string chaining.
- **`--json` on all CLI commands.** Commands should produce structured
  JSON via `print.json()` when `--json` is active.
- Functions that shell out to external tools should check for the tool
  on `PATH` first with `which` and return a helpful error.

### Gateway Observability

- Use `#[tracing::instrument(skip_all, fields(...))]` on public async
  methods at handler and subsystem boundaries.
- Use `tracing::error!` / `tracing::warn!` for diagnostics, never
  `println!`.

### CLI Output

- Use the `Print` struct (`print.status()`, `print.warn()`,
  `print.prompt()`), not raw `eprintln!`.

## Gateway Conventions

- Types that cross the RPC boundary live in `api-types`, not in handler
  code.
- `api-types` imports from `groundhog-xdr` for wire types and adds
  RPC-layer types (receipts, status enums, error codes). Don't duplicate
  XDR types — re-export them.
- JSON field names use `camelCase` (serde `rename_all`). Enums use
  tagged serialization (`#[serde(tag = "...")]`).
- Wire format: `0x`-prefixed lowercase hex for all byte values (hashes,
  addresses, topics, event data). Decimal JSON numbers for heights,
  counts, and other quantities. See docs/gateway/system-design.md
  "Wire Format Conventions".
- Byte fields use `HexBytes` (variable-length) or `Topic` (32-byte
  fixed) from `api-types/src/hex.rs` — never raw `Vec<u8>` in
  RPC-facing types.
- `#[non_exhaustive]` on public enums that will gain variants
  (`TransactionStatus`, `ExecStatus`).
- The jsonrpsee trait in `gateway/src/api.rs` is the source of truth
  for the RPC surface. `#[rpc(client, server)]` generates both sides.
- Handler implementations go in `gateway/src/handlers/`, one file per
  logical group (submit, receipt, blocks, events).
- The gateway enforces query bounds (e.g., max height range for
  `getContractEvents`) since hogd has none. Limits are in
  `GatewayConfig`.
- Error codes follow Substrate's per-module range pattern. Constants
  live in `api-types/src/error.rs`.
- Method prefix is `groundhog_` (may change — see open questions in
  docs/gateway/system-design.md).

## CLI Patterns

### Adding a new subcommand

1. Add a variant to `Command` in `main.rs`.
2. Create `src/my_cmd.rs` with `pub fn run(...) -> Result<(), CliError>`.
3. Add `pub mod my_cmd;` to `lib.rs`.
4. Add dispatch in `main()` calling `groundhog_cli::my_cmd::run`.
5. Add integration coverage in `tests/`.

### Adding a new template

1. Create `templates/mytemplate/lib.rs.tmpl`,
   `implementation.rs.tmpl`, and `test.rs.tmpl`.
2. Add `include_str!` constants in `scaffold.rs`.
3. Add a match arm in `generate()` returning the template triple.
4. Add `"mytemplate"` to the `value_parser` list in `main.rs`.

### Adding a new error variant

1. Add the variant to `CliError` in `src/error.rs`.
2. Add a `code()` arm.
3. Add a `hint()` arm.
4. Use the variant at call sites. Avoid `CliError::Other` for common
   user-facing failures.

### Credential security

Never pass credentials as command-line arguments to subprocesses. They
are visible via `ps`. Use env vars or stdin piping instead.

### Key security

Private keys are stored with `0o600` permissions. Secret bytes use
`zeroize::Zeroizing`. `load_key_info` reads public key data without
loading the private key.

## Testing

- Unit tests: inline `#[cfg(test)] mod tests` in the source file.
- Integration tests: `crates/{name}/tests/*.rs`.
- Async tests use `#[tokio::test]`.
- Follow Arrange-Act-Assert pattern.
- Tests that mutate env vars use `#[serial]` from `serial_test`.
- `api-types/src/hex.rs` has serde roundtrip tests for `HexBytes`
  and `Topic`.
- Unit tests for type conversions (XDR ↔ API types) in `api-types`.
- Integration tests for handlers using mock x23 backends in `gateway`.
- The `client` crate should have roundtrip tests against a test server.
- CLI integration tests run the binary via
  `env!("CARGO_BIN_EXE_groundhog")`.
- The `build` and `test` CLI commands cannot be fully
  integration-tested without the SDK crate registry. Unit tests cover
  helpers such as `find_project` and `wasm_output_path`.

## Before Committing

- [ ] `cargo test --workspace` — all tests pass
- [ ] `cargo build --workspace` — no compiler warnings
- [ ] `cargo clippy --workspace -- -D warnings` — clean
- [ ] `cargo fmt --check` — formatted
- [ ] No `.unwrap()` added outside of tests
- [ ] No `.expect()` on untrusted input
- [ ] New `.expect()` messages use `bug:` prefix
- [ ] No `dbg!()` or debug print statements
- [ ] All new public items have doc comments

## Git Workflow

- Use feature branches and PRs — never commit directly to main.

## Planning Docs

- `docs/cli/roadmap.md` — CLI backlog, sequencing, and completion state.
- `docs/gateway/roadmap.md` — gateway backlog and sequencing.
- `docs/gateway/system-design.md` — full gateway architecture and open
  questions.
- `docs/cli/foundry.md` — Foundry DX analysis for CLI design reference.

## Context

- Groundhog is an L1 blockchain (SCP consensus, WASM execution, NNInt
  storage). Instant finality, no forks, no gas fees.
- Transaction lifecycle is simpler than Ethereum/Substrate: Pending →
  Committed (or Invalid/Expired). No reorgs, no "included but not
  finalized".
- The hogd indexer (C++, RocksDB) already has working query APIs. The
  gateway's job is putting a standard JSON-RPC interface in front of
  them.
- The CLI wraps the Rust/WASM build workflow and will integrate with
  the gateway client for deploy, call, and status commands.
- Submission through chuckd's current wallet stub is not a target
  architecture. If a `chuckd` backend remains configurable, it should
  mean a real private-network transport contract, not reuse of the old
  wallet path.
