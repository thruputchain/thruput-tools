# Groundhog Developer Tools

Unified workspace for Groundhog blockchain developer tooling: a JSON-RPC
gateway, typed client, transaction building, key management, and CLI.

## Architecture

Workspace crates in `crates/`:

- **api-types** — shared RPC types (receipt, status, block info, events,
  errors). Consumed by gateway, client, CLI, and external tools.
- **rpc-api** — shared jsonrpsee trait definitions used by the gateway,
  dev node, and client crate.
- **rpc-server** — reusable server-side `GroundhogApiServer`
  implementation. Owns shared request validation, public error mapping,
  and submit/chain/read backend traits used by gateway, dev-node, and
  future hogd-dev compositions.
- **x23-transport** — shared x23 connection-pool helpers
  (`FixedSerialPool` setup, address parsing, typed `ConnectError`).
  Consumed by gateway and node-supervisor; backend-agnostic. Named for
  the x23 wire protocol to keep it distinct from the JSON-RPC `rpc-*`
  family of crates.
- **gateway** — jsonrpsee server binary. Its backend adapter implements
  the rpc-server planes over hogd indexer and scheduler x23 RPCs.
- **client** — typed JSON-RPC client generated from the same
  `#[rpc(client)]` trait as the server. Wraps api-types.
- **signing** — off-chain transaction signing and construction. Signer
  trait, local Ed25519 signer, canonical address derivation, tx builders
  for deploy and call, XDR encoding.
- **tx-builder** — thin CLI wrapper over signing (temporary).
- **cli** — `groundhog` binary. Wraps the Rust/WASM build and test
  workflow plus deploy/call and local dev-node control. Lib/bin split:
  all logic in the library crate, `main.rs` is pure dispatch.
  Subcommand functions take plain Rust types, not clap structs.
- **dev-node** — feature-gated experimental local JSON-RPC dev node
  backed by the SDK `native_exec` / hogd `test_ffi` prototype on a
  dedicated worker thread. It exposes the public Groundhog API through
  rpc-server and keeps `groundhog_dev_*` controls local to dev-node. It
  shares execution semantics with SDK tests, but the durable backend
  direction is a hogd-side embedded execution library; see the CLI
  planning docs.
- **node-supervisor** — `groundhog node` facade. Supervises a hogd-owned
  `hogd-dev` subprocess and embeds `rpc-server` to serve the public
  JSON-RPC. `chainTip` is backed by hogd-dev's scheduler endpoint;
  submit/read remain stubbed (NotImplemented) until PR 4b/4c land their
  x23 adapters. Durable local-node path; `dev-node` remains the
  native-exec prototype until hogd-dev covers the normal local
  development loop.
- **e2e-tests** — ignored process-level JSON-RPC lifecycle smoke tests.
- **bench** — benchmark harnesses.

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
cargo clippy --workspace --all-targets -- -D warnings   # lint (must be clean)
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
cargo run -p groundhog-cli -- deploy       # from inside a project
cargo run -p groundhog-cli -- call my-counter get
cargo run -p groundhog-cli -- keys generate alice
cargo run -p groundhog-cli -- keys ls
# Experimental local dev node is feature-gated
cargo run -p groundhog-cli --features dev-node -- node
# Typical local loop
# 1. cargo run -p groundhog-cli -- new my-counter && cd my-counter
# 2. cargo run -p groundhog-cli -- build
# 3. cargo run -p groundhog-cli --features dev-node -- node  # separate terminal
# 4. cargo run -p groundhog-cli -- deploy
# 5. cargo run -p groundhog-cli -- call my-counter get
```

## Code Style

- **Edition 2024**, stable Rust.
- **100-column max width** (rustfmt community default).
- **`thiserror`** for all error enums. **No `anyhow` in this project.**
- **No `.unwrap()` in library code.** Return `Result`. `.unwrap()` is
  fine in tests.
- **`.expect("bug: reason")` only** for true invariant violations (use
  `bug:` prefix for grep-ability).
- **No `.expect()` on untrusted input.** Return `Result` or log and
  drop instead.
- **No `dbg!()` or debug prints** in committed code.
- **`///` doc comments on public items.**  Lead with a sentence
  describing what the thing is or does for the consumer; follow with
  any non-obvious behavior (edge cases, contracts, drop semantics).
  Cut sentences whose entire content is the identifier's words —
  those are noise.  Skip the comment entirely only when the name and
  signature already say everything a consumer needs.
- Names express what something *does* or *is*, not its implementation
  shape.  Avoid shape adjectives like "permissive", "narrow",
  "strict".
- Until the codebase has external consumers locked into wire
  stability, delete unused code rather than preserving it as
  "reserved for future use".  Stale stubs invite new code to stack
  on semantics that are not there yet.
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
  (`TransactionStatus`, `ExecStatus`, `SupervisorStop`).
- The jsonrpsee trait in `rpc-api/src/lib.rs` is the source of truth
  for the RPC surface. `#[rpc(client, server)]` generates both sides.
- The public `GroundhogApiServer` implementation lives in `rpc-server`.
  Concrete binaries provide submit/chain/read backend adapters instead
  of implementing the public RPC trait directly.
- Shared transaction hex/XDR validation and canonical hash computation
  live in `rpc-server::tx`; do not duplicate that logic in gateway or
  dev-node adapters.
- Gateway handler modules are production backend helpers grouped by
  subsystem (receipt, blocks, events, parsing). Do not add new public
  JSON-RPC handler implementations there.
- `rpc-server` enforces shared request bounds — currently `getEvents`
  per-page limit and pagination cursor length.  Gateway supplies the
  production limits from `GatewayConfig`.
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
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `cargo fmt --check` — formatted
- [ ] No `.unwrap()` added outside of tests
- [ ] No `.expect()` on untrusted input
- [ ] New `.expect()` messages use `bug:` prefix
- [ ] No `dbg!()` or debug print statements
- [ ] All new public items have doc comments

## Before Opening a PR

Run the full CI-equivalent check locally before pushing:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must pass. CI runs clippy with `--all-targets` (which
includes test and bench code), so always use that flag locally too.

## Git Workflow

- Use feature branches and PRs — never commit directly to main.
- Commit messages: title plus a short body covering what changed and
  why.  No review-process narrative, no exhaustive file or type
  lists (the diff has those), no references to out-of-tree state.
  Title plus 2–6 lines of body is the right shape for most commits.

## Planning Docs

- `docs/cli/roadmap.md` — CLI backlog, sequencing, and completion state.
- `docs/cli/2026-04-25-hogd-dev-node-hybrid.md` — durable design for
  the hogd-owned subprocess local-node backend (subprocess over FFI).
- `docs/cli/plans/2026-04-29-local-node-hogd-dev-supervisor.md` —
  node-supervisor implementation plan and PR boundaries.
- `docs/cli/plans/2026-04-30-hogd-dev-binary-bootstrap.md` — hogd-side
  bootstrap plan for the smallest credible `hogd-dev` v1 binary.
- `docs/cli/specs/2026-04-29-hogd-dev-process-contract.md` — draft
  hogd-dev subprocess contract.
- `docs/cli/specs/2026-04-29-node-supervisor-x23-client-boundary.md` —
  node-supervisor x23 client wiring decision.
- `docs/cli/plans/2026-04-14-bundled-wasm-opt.md` — bundled wasm-opt
  rollout (dev-node done; CLI still shells out as of 2026-04-30).
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
