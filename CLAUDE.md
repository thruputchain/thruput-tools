# Groundhog Developer Tools

Unified workspace for Groundhog blockchain developer tooling: a JSON-RPC
gateway, typed client, transaction building, key management, and CLI.

## Architecture

Workspace crates in `crates/` are grouped by boundary:

JSON-RPC public surface in `crates/json-rpc/`:

- **json-rpc-types** — shared JSON-RPC types (receipt, status, block info,
  events, errors) plus the runtime manifest JSON contract used by local and
  benchmark backends. Consumed by gateway, client, CLI, and external tools.
- **json-rpc-api** — shared jsonrpsee trait definitions used by the gateway,
  local node, and client crate.
- **json-rpc-server** — reusable server-side `GroundhogApiServer`
  implementation. Owns shared request validation, public error mapping, and
  submit/chain/read backend traits used by gateway and hogd-dev supervisor
  compositions.
- **json-rpc-gateway** — jsonrpsee server binary. Its backend adapter
  implements the `json-rpc-server` planes over hogd indexer and scheduler x23
  RPCs. For dev/bench runs it can load backend endpoints and protocol metadata
  from `--runtime-manifest <path>`.
- **json-rpc-client** — typed JSON-RPC client generated from the same
  `#[rpc(client)]` trait as the server. Wraps `json-rpc-types`.
- **open-rpc** — OpenRPC document generator. `Project` owns a single
  `schemars::SchemaGenerator` shared across modules so named Rust types land
  once in `components.schemas` and use sites carry `$ref`. `ModuleBuilder` is
  the threading point. Treated as an internal implementation detail; the
  external consumer is the generated JSON, not the Rust API.
- **open-rpc-macros** — `#[open_rpc]` proc macro. Generates a companion struct
  per jsonrpsee `#[rpc]` trait whose `module_doc(&mut ModuleBuilder)` appends
  OpenRPC method descriptors. Resolves the `groundhog-open-rpc` crate by
  package name via `proc_macro_crate` so downstream re-exports work.
- **open-rpc-spec** — library + binary that assembles the canonical artifact.
  The binary emits JSON to stdout; the library is the composition point for the
  workspace's RPC traits and owns the hand-maintained per-method `errors[]`
  mapping. The published `groundhog-open-rpc-spec` crate is consumed by
  `client-ts` via its regen script. A byte-equal snapshot at
  `crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json` guards artifact
  shape; `UPDATE_SNAPSHOTS=1` regenerates after intentional changes.

Internal x23 support in `crates/x23/`:

- **x23-transport** — shared x23 connection-pool helpers (`FixedSerialPool`
  setup, address parsing, typed `ConnectError`). Consumed by gateway and
  node-supervisor; backend-agnostic. Named for the x23 wire protocol to keep it
  distinct from the public JSON-RPC stack.

Developer-facing tools in `crates/tools/` and RV32-specific tooling in
`crates/rv32/`:

- **cli** — `groundhog` binary. Wraps the Rust/WASM build and test workflow
  plus deploy/call and hogd-dev local-node control. Lib/bin split: all logic in
  the library crate, `main.rs` is pure dispatch. Subcommand functions take plain
  Rust types, not clap structs.
- **signing** — off-chain transaction signing and construction. Signer trait,
  local Ed25519 signer, canonical address derivation, tx builders for deploy
  and call, XDR encoding.
- **rv32-artifact** — lib `groundhog_rv32_artifact`. The single Rust source of
  truth for the Groundhog v3 RV32 artifact format: constants, offsets, format
  fingerprint, writer, and validator. It is grouped with the lowerer under
  `crates/rv32/` because this artifact is bespoke RV32 tooling, not XDR.
- **rv32-lower** — `rv32-lower` binary (+ lib `groundhog_rv32_lower`). The
  `wasm-opt` analog for the rv32 build path: lowers a `riscv32im-unknown-none-elf`
  contract ELF to a Groundhog v3 RV32 artifact (`.rv32a`). Published to the
  `groundhog` registry; consumed as a binary by scripts/hogd and as a library
  by `groundhog build` and other Rust callers. See its README and
  `hogd/docs/rv32/rv32-author-toolchain.md`.

Local dev and test harnesses in `crates/dev/`:

- **node-supervisor** — `groundhog node` facade. Supervises a hogd-owned
  `hogd-dev` subprocess and embeds `json-rpc-server` to serve the public
  JSON-RPC. `chainTip` and `sendRawTransaction` are backed by hogd-dev's
  `SchedulerCoord` endpoint; receipts, transaction status, and contract events
  are backed by hogd-dev's `IndexerBlockExplorer` endpoint when advertised in
  readiness (otherwise the read plane stays stubbed `NotImplemented` per the v1
  reduced contract). End-to-end coverage of the read plane is gated on the
  hogd-dev v1 binary actually exposing the `IndexerBlockExplorer` endpoint with
  full event-feed wiring. This is the only CLI local-node backend; the legacy
  native-exec prototype (`crates/dev-node`) was deleted in PR 5, with the git
  history retaining the source for any future reference. Startup consumes
  hogd-dev's readiness pointer, verifies the runtime manifest hash and
  owner-only file permissions, and exposes the manifest path/hash in
  `groundhog node --json`.
- **e2e-tests** — ignored process-level JSON-RPC lifecycle smoke tests.
- **bench** — benchmark harnesses. `groundhog-bench --runtime-manifest <path>`
  derives bootstrapped account signers and submits dev-wallet compatible
  `sendRawTransaction` traffic.
- **sim** — `hogsim` stateful simulation and soak-test harness. Runs
  deterministic actor schedules against an existing target, writes
  `result.jsonl` / `result.json`, and currently covers chain-tip, dev-wallet
  submit smoke, receipt polling, and Phase 1 assertions.

See `docs/gateway/system-design.md` for full gateway design and
`docs/gateway/api-sketch.md` for the jsonrpsee trait definition.
See `docs/gateway/runtime-manifest.md` for the local
CLI/gateway/bench startup handoff. See `docs/sim/README.md` for the
simulator quickstart and result-artifact contract.

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

## Schema Ownership

- `protocol/*.x` owns the canonical binary protocol structure. The
  Rust binding layer for those XDR types lives in
  `protocol-rs/groundhog-xdr`; the TypeScript peer lives in
  `protocol-ts` (`@groundhogchain/protocol`, `/contract-spec`,
  `/xdr`, `/client`). Generated TS bindings emitted by
  `groundhog codegen ts` import from these packages — the CLI never
  re-derives protocol primitives.
- `crates/json-rpc/json-rpc-types` owns shared JSON-RPC types and JSON
  projections of XDR types. If the JSON wire format differs from raw XDR (for
  example `0x`-prefixed hex wrappers), model that once here instead of
  redefining it in gateway, client, or CLI code.
- `crates/json-rpc/json-rpc-types/src/bootstrap_manifest.rs` owns the
  bootstrap manifest input schema for local and benchmark backend launchers.
  `crates/json-rpc/json-rpc-types/src/runtime_manifest.rs` owns the
  runtime manifest output schema shared by `hogd-dev` producers,
  `groundhog node`, `hogd-gateway --runtime-manifest`, and
  `groundhog-bench --runtime-manifest`. Public manifest structs are
  `#[non_exhaustive]`;
  additive fields are expected. Optional endpoints must be absent when
  unavailable, not `null`. Large numeric quantities in manifests are
  decimal strings.
- `crates/json-rpc/json-rpc-api` owns JSON-RPC method names, params, and
  results. Trait signatures in `crates/json-rpc/json-rpc-api/src/lib.rs` are
  the sole source of truth for the public RPC surface.
- OpenRPC / JSON Schema documents for the RPC surface are generated
  artifacts from Rust traits and Rust types. Do not hand-author or
  maintain parallel schema files for RPC methods or RPC payload types.
- The canonical JSON-RPC error catalog lives in
  `crates/json-rpc/json-rpc-types/src/error.rs`. See "Gateway Conventions"
  below for the OpenRPC projection, the per-method mapping in
  `crates/json-rpc/open-rpc-spec/src/lib.rs`, and the snapshot-enforced rule
  that adding a method requires updating that mapping.
- CLI-only machine-readable output types live in `crates/tools/cli` by
  default. If a CLI schema embeds chain or runtime data, reuse
  `json-rpc-types` nested types instead of copying them.
- Extract a shared CLI schema crate only after a CLI output type is
  reused across commands or crates, or becomes a documented external
  contract broader than a single command.

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
cargo test -p groundhog-json-rpc-types    # test a specific crate
cargo test -p groundhog-cli              # CLI tests
```

## Release Process

`publish.yml` and `release.yml` intentionally split crate publishing from binary
asset publishing, but **Release package** is the only normal human entrypoint.
Run **Release package** manually to publish crates. It reads the Cargo workspace
version, creates the matching `vX.Y.Z` tag, publishes the crate set, pushes the
tag only after crate publish succeeds, then explicitly dispatches `release.yml`
on that tag. Do not manually push `v*` tags and do not run `release.yml`
directly for normal releases.

`release.yml` is dispatch-only and direct human dispatch fails; it must be
dispatched by the publish workflow. It verifies that the matching
`groundhog-cli` crate version already exists in the private registry before
uploading binary assets. It builds `hogd-gateway` from this repository and builds the
`groundhog` CLI from the published crate; the CLI release build uses `--features
dev-node` so the published binary includes the local `groundhog node` path. The
GitHub Release assets are per-app, per-target archives:
`hogd-gateway-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` and
`groundhog-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`, plus `SHA256SUMS`.

Keep the Cargo workspace version, internal `=X.Y.Z` workspace dependency pins,
git tag, and GitHub Release tag aligned. The release repo downloads component
assets from the pinned `groundhog-rpc` tag.

### Runtime Manifest Smoke

Use this for the local gateway/bench path once a real `hogd-dev` binary
is available:

```bash
HOGD_DEV_BINARY=/path/to/hogd-dev scripts/smoke-runtime-manifest-bench.sh
```

The script starts `groundhog node`, reads `data.runtime_manifest.path`
from the CLI startup JSON, runs `groundhog-bench` against the CLI node,
starts `hogd-gateway --runtime-manifest <path>`, and runs the same
bench checks against the standalone gateway. `BENCH_RATE`,
`BENCH_DURATION`, and `BENCH_ACCOUNTS` can tune the run.
Use `BENCH_SCENARIO=gateway-baseline` or `BENCH_SCENARIO=submit-ramp`
for the repeatable local benchmark scenarios documented in
`docs/gateway/bench-scenarios.md`.

The broader local-DX smoke remains:

```bash
HOGD_DEV_BINARY=/path/to/hogd-dev scripts/smoke-local-dx.sh
```

With `SMOKE_STANDALONE_GATEWAY=1`, that script also starts the standalone
gateway from the runtime manifest; do not rehydrate gateway TOML by hand
from individual endpoint fields unless you are testing config overrides.

For simulator local-target work, use the script so the local-provider and
existing-provider process smokes stay in sync:

```bash
GROUNDHOG_HOGD_DEV_BINARY=/path/to/hogd-dev scripts/smoke-sim-local.sh
```

These smokes start `groundhog node`, run `hogd-dev`, connect to localhost, and
write the node PID file under `~/.cache/groundhog`; Codex should run them
outside the sandbox. If they fail with permission, localhost-connect, network,
or cache-write errors, rerun the same command with escalation rather than
rewriting the workflow. Use absolute paths for `hogd-dev` because quoted `~`
does not expand.

For simulator existing-target work, `hogsim` can still run against an
already-started target:

```bash
cargo run -p groundhog-sim --bin hogsim -- validate examples/sim/local-wallet-soak.json
cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-wallet-soak.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest "$RUNTIME_MANIFEST"
```

The current simulator contract and open implementation checklist live under
`docs/sim/`.

### Running the CLI locally

```bash
cargo run -p groundhog-cli -- --help
cargo run -p groundhog-cli -- init
cargo run -p groundhog-cli -- new my-counter
cargo run -p groundhog-cli -- new my-stub --template stub
cargo run -p groundhog-cli -- build        # from inside a project
cargo run -p groundhog-cli -- test         # requires login first
cargo run -p groundhog-cli -- test --json  # structured TestReport on stdout
cargo run -p groundhog-cli -- deploy       # from inside a project
cargo run -p groundhog-cli -- call my-counter get
cargo run -p groundhog-cli -- keys generate alice
cargo run -p groundhog-cli -- keys ls
# Experimental local node backed by hogd-dev (Cargo feature is still dev-node)
cargo run -p groundhog-cli --features dev-node -- node --hogd-dev-binary /path/to/hogd-dev
# Machine-readable startup metadata includes data.rpc_url and
# data.runtime_manifest.path/hash.
cargo run -p groundhog-cli --features dev-node -- --json node --hogd-dev-binary /path/to/hogd-dev
# Typical local loop
# 1. cargo run -p groundhog-cli -- new my-counter && cd my-counter
# 2. cargo run -p groundhog-cli -- build
# 3. cargo run -p groundhog-cli --features dev-node -- node --hogd-dev-binary /path/to/hogd-dev  # separate terminal
# 4. cargo run -p groundhog-cli -- deploy
# 5. cargo run -p groundhog-cli -- call my-counter get
```

For manual gateway/bench runs, start `groundhog node --json`, extract
`data.runtime_manifest.path`, then pass it to both consumers:

```bash
hogd-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction
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
- When a command's `data` payload is intended to be a stable
  machine-readable contract, include a command-specific
  `schemaVersion` field inside `data` from v1 onward. Do not version
  the shared `CommandResult` envelope per command.

## Gateway Conventions

- Types that cross the RPC boundary live in `json-rpc-types`, not in
  handler code.
- `json-rpc-types` imports from `groundhog-xdr` for wire types and adds
  RPC-layer types (receipts, status enums, error codes). Don't duplicate XDR
  types; re-export them.
- JSON field names use `camelCase` (serde `rename_all`). Enums use
  tagged serialization (`#[serde(tag = "...")]`).
- Wire format: `0x`-prefixed lowercase hex for all byte values (hashes,
  addresses, topics, event data). Decimal JSON numbers for heights,
  counts, and other quantities. See docs/gateway/system-design.md
  "Wire Format Conventions".
- Byte fields use `HexBytes` (variable-length) or `Topic` (32-byte fixed) from
  `crates/json-rpc/json-rpc-types/src/hex.rs`; never raw `Vec<u8>` in
  RPC-facing types.
- `#[non_exhaustive]` on public enums that will gain variants
  (`TransactionStatus`, `ExecStatus`, `SupervisorStop`).
- The jsonrpsee trait in `crates/json-rpc/json-rpc-api/src/lib.rs` is the
  source of truth for the RPC surface. `#[rpc(client, server)]` generates both
  sides.
- The public `GroundhogApiServer` implementation lives in `json-rpc-server`.
  Concrete binaries provide submit/chain/read backend adapters instead of
  implementing the public RPC trait directly.
- Shared transaction hex/XDR validation and canonical hash computation
  live in `groundhog_json_rpc_server::tx`; do not duplicate that logic in
  gateway or node-supervisor adapters.
- Gateway handler modules are production backend helpers grouped by
  subsystem (receipt, blocks, events, parsing). Do not add new public
  JSON-RPC handler implementations there.
- `json-rpc-server` enforces shared request bounds — currently `getEvents`
  per-page limit and pagination cursor length.  Gateway supplies the
  production limits from `GatewayConfig`.
- Error codes follow Substrate's per-module range pattern. The
  canonical catalog is `ERROR_INFOS` (stable name, code, short
  message, consumer-facing description) plus `codes::ALL` in
  `crates/json-rpc/json-rpc-types/src/error.rs`. The OpenRPC artifact
  projects this into `components.errors`; methods carry `errors[]` arrays of
  `$ref`s. The per-method mapping lives in
  `crates/json-rpc/open-rpc-spec/src/lib.rs` and is hand-maintained — adding a
  method requires updating that mapping or
  the snapshot test fails. Error component `name` / `message` /
  `description` are sibling fields on the error object; `data` stays
  reserved for future typed runtime payloads.
- Method prefix is `groundhog_` (may change — see open questions in
  docs/gateway/system-design.md).

### OpenRPC Wire Version

OpenRPC `info.version` is the JSON-RPC wire-format SemVer, not the
Rust workspace version. The source of truth is `WIRE_FORMAT_VERSION`
in `crates/json-rpc/json-rpc-api/src/lib.rs`.

Bump major for breaking changes to method shape, params, results, or
errors. Bump minor for additive changes such as new methods, optional
fields, or errors. Bump patch for documentation, description, or
example refinements only.

The `groundhog_version` RPC exposes `wireFormat`, `implementation`,
and `commitHash`. SDKs and other consumers may branch on `wireFormat`
only; `implementation` and `commitHash` are diagnostic fields.

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

### Adding machine-readable output to a CLI command

When a command's `--json` output is intended as a stable contract for
downstream tooling (CI, watch consumers, automation):

1. Define the typed report in `crates/tools/cli/src/<cmd>_json.rs` with
   `serde::{Serialize, Deserialize}` and `schemars::JsonSchema` derives.
   Use `#[serde(rename_all = "camelCase")]` so the JSON boundary
   matches Groundhog's wire convention.
2. Include a `schema_version: u32` field inside `data` from v1
   onward, plus a `pub const <CMD>_JSON_SCHEMA_VERSION: u32 = 1`
   referenced once at construction. Do not version the shared
   `CommandResult` envelope per command.
3. Emit success and failure on the same stream (stdout) via
   `print.json(&CommandResult::new(...))` and
   `print.json(&CommandResult::failure(...))` so `--watch --json`
   produces newline-delimited records that consumers can parse off a
   single stream. Generic preflight errors that never produce the
   typed payload may keep the existing stderr error envelope.
4. Suppress non-JSON status output via `print.status` / `print.warn`
   when `print.is_json()` so stdout stays clean.
5. Add a binary-level integration test that shells the CLI against a
   fixture (e.g. fake-cargo on `PATH`) and asserts the JSON shape:
   `data.schemaVersion`, success / failure paths, structural fields
   (not just compile-pass). `groundhog test --json` and its
   integration tests under `crates/tools/cli/tests/` are the reference
   shape.

### Credential security

Never pass credentials as command-line arguments to subprocesses. They
are visible via `ps`. Use env vars or stdin piping instead.

### Key security

Private keys are stored with `0o600` permissions. Secret bytes use
`zeroize::Zeroizing`. `load_key_info` reads public key data without
loading the private key.

## Testing

- Unit tests: inline `#[cfg(test)] mod tests` in the source file.
- Integration tests: each crate's `tests/*.rs` directory.
- Async tests use `#[tokio::test]`.
- Follow Arrange-Act-Assert pattern.
- Tests that mutate env vars use `#[serial]` from `serial_test`.
- `crates/json-rpc/json-rpc-types/src/hex.rs` has serde roundtrip tests for
  `HexBytes` and `Topic`.
- Unit tests for type conversions (XDR to API types) live in
  `json-rpc-types`.
- Integration tests for handlers using mock x23 backends live in
  `json-rpc-gateway`.
- The `json-rpc-client` crate should have roundtrip tests against a test
  server.
- CLI integration tests run the binary via
  `env!("CARGO_BIN_EXE_groundhog")`.
- The `build` and `test` CLI commands cannot be fully
  integration-tested without the SDK crate registry. Unit tests cover
  helpers such as `find_project` and `wasm_output_path`.

### Snapshot testing for generated artifacts

For generated artifacts that consumers depend on (currently the
OpenRPC spec; in future, any wire-format-stable codegen output):

- Check the canonical artifact into the repo (e.g.
  `crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json`).
- The test asserts byte-equal match against the committed file using
  `pretty_assertions::assert_eq` so failure diffs are readable.
- `UPDATE_SNAPSHOTS=1 cargo test ...` regenerates the snapshot on
  intentional changes. The test returns early in that mode so it
  never asserts against a freshly-written file.
- Pair the snapshot with structural invariants in the same test file
  that document intent and produce clearer failures than a multi-
  thousand-line diff (e.g. "every method documents ≥1 error",
  "every `$ref` resolves", "no schema key starts with `VecM<`").
  The snapshot catches regressions; the invariants document the
  contract.

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

Active design notes only. Shipped decisions live in the code; git
history retains the design context for anything not currently active.

- `docs/README.md` — index distinguishing living docs from archived
  historical notes.
- `docs/cli/roadmap.md` — CLI backlog, sequencing, and completion state.
- `docs/cli/specs/2026-04-29-hogd-dev-process-contract.md` — cross-team
  hogd-dev subprocess contract.
- `docs/cli/specs/2026-04-14-bundled-wasm-opt-design.md` — deferred
  note for possible future bundled wasm-opt work.
- `docs/cli/specs/2026-05-03-ts-codegen-design.md` — `groundhog codegen
  ts` TypeScript binding codegen.
- `docs/cli/specs/2026-05-07-view-rpc-and-receipt-return-data.md` —
  view RPC and structured receipt return-data wire design.
- `docs/cli/specs/2026-05-07-transaction-detail-options.md` —
  transaction detail / receipt response shape.
- `docs/cli/specs/2026-05-11-openrpc-wire-version-design.md` — OpenRPC
  wire-version policy and `groundhog_version` exposure.
- `docs/cli/specs/2026-05-13-view-rpc-protocol-rs-follow-up.md` —
  protocol/protocol-rs service contract needed to wire view execution.
- `docs/gateway/roadmap.md` — gateway backlog and sequencing.
- `docs/gateway/system-design.md` — gateway architecture.
- `docs/gateway/runtime-manifest.md` — runtime manifest quickstart for
  CLI, gateway, and benchmark runs.
- `docs/gateway/examples/runtime-manifest.hogd-dev.example.json` —
  example v1 runtime manifest shape. It is not runnable because endpoint
  ports, genesis hash, wallet addresses, and hashes are runtime state.
- `docs/gateway/open-questions.md` — cross-team open and resolved
  questions for the gateway design.
- `docs/sim/README.md` — `hogsim` quickstart, existing-provider contract,
  result-directory layout, and compare semantics.
- `docs/sim/sim-design.md` — simulator design: provider boundary,
  workload model, chaos taxonomy, result/comparison artifacts.
- `docs/sim/sim-implementation-plan.md` — Phase 1 execution checklist
  and Open Hardening Follow-Ups for the simulator crate.
- `docs/archive/cli/2026-04-25-hogd-dev-node-hybrid.md` — historical
  durable-local-node design context, retained after the hogd-dev
  supervisor path shipped.
- `docs/archive/cli/foundry.md` — Foundry DX analysis kept as design
  reference; not a current task list.

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
