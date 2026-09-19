# Groundhog Developer Tools

Agent reference for the Groundhog off-chain tooling workspace. Keep this file
actionable: architecture boundaries, commands, style rules, and planning-doc
policy.

## Agent Guidance

For Groundhog programming-model or developer-workflow work, load the guidance
bundled with this checkout before relying on prose elsewhere:

```bash
cargo run -p groundhog-cli -- --skill
```

`groundhog --skill` is the public first-contact command. Its editable sources
are `crates/tools/cli/assets/groundhog/knowledge/bundle.toml` and the records
beside it. Files under `knowledge/generated/` are generated; keep other README
and agent-instruction files as concise pointers rather than parallel copies.
Regenerate and check them with:

```bash
cargo run -p groundhog-agent-knowledge -- build
cargo run -p groundhog-agent-knowledge -- check
```

Review the skill on every workspace or CLI version bump, and whenever the CLI,
scaffold, SDK, generated TypeScript bindings, TypeScript client, wallets, or
public RPC workflow changes. Check its claims against current public APIs,
released examples, and the versions a user can install. Dated design documents
are historical context when they conflict with those surfaces. Run
`cargo test -p groundhog-cli skill` and
`cargo test -p groundhog-agent-knowledge` after updating it. Confirm the skill
is present with
`cargo package -p groundhog-cli --list --allow-dirty`.

## Architecture

Workspace crates are grouped by boundary:

- **build-info** — dependency-free crate and packaging-platform version
  metadata shared by binaries and diagnostic outputs.

`crates/json-rpc/` owns the public RPC surface:

- **json-rpc-types** — shared RPC payloads, error catalog, hex wrappers, runtime
  and bootstrap manifest contracts.
- **json-rpc-api** — jsonrpsee trait definitions; source of truth for public
  method names, params, and results.
- **json-rpc-server** — reusable `GroundhogApiServer`, shared validation, error
  mapping, and submit/chain/read/view backend traits.
- **json-rpc-gateway** — production JSON-RPC gateway binary over hogd x23
  services; can load backend endpoints from `--runtime-manifest` for dev/bench.
- **json-rpc-client** — typed client generated from the same RPC trait.
- **open-rpc**, **open-rpc-macros**, **open-rpc-spec** — generated OpenRPC
  artifact pipeline. Snapshot:
  `crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json`.

`crates/x23/` owns internal transport helpers:

- **x23-transport** — connection pools, address parsing, and typed connect
  errors for gateway and node-supervisor. Internal; not the public JSON-RPC API.

`crates/tools/` and `crates/rv32/` own developer tooling:

- **agent-knowledge** — non-published deterministic assembler for repo-owned
  agent knowledge records, the embedded skill, and normalized bundle metadata.
- **cli** — `groundhog` binary. Build/test/deploy/call/local-node workflow.
  Keep logic in the library crate; `main.rs` should be dispatch only.
- **toolchain** — embedded trusted devnode catalog, bounded acquisition and
  extraction, and versioned cache management. Cache lookup remains local and
  non-mutating; only explicit acquisition receives an HTTP source.
- **signing** — off-chain transaction signing and construction. Signer trait,
  local Ed25519 signer, canonical address derivation, tx builders for deploy
  and call, XDR encoding.
- **tx-validation** — off-chain mirror of the bounds hogd's executor checks
  before running a transaction. `NetworkConfigParams` flattens a db tip's
  network config into per-parameter fields (`None` when unstated);
  `check_transaction` reports violations as `TxInvalidDetail`.
  `ConfigParamName` is the one Rust-side spelling of the parameter names
  hog-devnode's `--network-config` flag and its data-dir metadata key by;
  `groundhog node` parses the flag into it rather than passing a free string.
- **rv32-artifact** — Rust source of truth for the Groundhog RV32 artifact
  format: constants, offsets, fingerprint, writer, and validator.
- **rv32-lower** — `rv32-lower` binary (+ lib `groundhog_rv32_lower`). The
  RV32 build lowerer from `riscv32im-unknown-none-elf` ELF to `.rv32a`.

`crates/dev/` owns local dev and test harnesses:

- **node-supervisor** — `groundhog node` facade. Supervises `hogd-dev`, embeds
  `json-rpc-server`, verifies runtime-manifest handoff, and exposes manifest
  path/hash in `groundhog node --json`. This is the only CLI local-node backend.
- **e2e-tests** — ignored process-level JSON-RPC lifecycle smoke tests.
- **bench** — benchmark harnesses. `groundhog-bench --runtime-manifest <path>`
  derives bootstrapped account signers and submits dev-wallet compatible
  `sendRawTransaction` traffic.
- **sim** — `hogsim` stateful simulation and soak-test harness. Runs
  deterministic actor schedules, writes `result.jsonl` / `result.json`, and
  covers local/existing targets, contract workloads, and soak diagnostics.

See `docs/gateway/system-design.md` for full gateway design. The
jsonrpsee trait in `crates/json-rpc/json-rpc-api/src/lib.rs` and the
OpenRPC snapshot in
`crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json` are the
RPC-surface references.
See `docs/gateway/runtime-manifest.md` for the local
CLI/gateway/bench startup handoff. See `docs/sim/README.md` for the
simulator quickstart and result-artifact contract.

### Gateway Data Flows

- **Tx submission** → hogd `SchedGateway` (`schedgateway_submit_tx_batch`) via x23
- **Receipts/events** → hogd indexer (`get_transaction_info`, `get_contract_events`) via x23
- **Chain state** → hogd `SchedGateway` (`schedgateway_get_tip`, `schedgateway_lookup_tx`, `schedgateway_pending_tx_count`) via x23
- **Block data** → coordinator storage (TBD — see docs/gateway/system-design.md)

`groundhog_sendRawTransaction` should only return success once the
authoritative downstream mempool path admitted the transaction. Admission does
not imply nomination or inclusion in a committed block.

### Dependencies

All Groundhog crates come from the private registry (configured in
`.cargo/config.toml`). No git submodules.

Key private deps: `groundhog-xdr` (wire types), `xdrs` (XDR
serialization), `rpc_driver` (x23 RPC framework).

Key public deps: `jsonrpsee`, `axum`, `tokio`, `tower`/`tower-http`, `clap`,
`prometheus`.

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
  `groundhog node`, `hog-gateway --runtime-manifest`, and
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

`make build` builds the workspace with `PROFILE=release` by default.
`make build-native` builds a local-only, host-tuned workspace; `make check-native`
counts hardware SHA-256 instructions. ARM hardware SHA-256 is enabled by
`sha2/asm` with runtime detection, not by the native build. See `make help`.

### Per-crate

```bash
cargo test -p groundhog-json-rpc-types    # test a specific crate
cargo test -p groundhog-cli              # CLI tests
```

## Release Process

Normal release entrypoint: manually run **Release package**. It reads the Cargo
workspace version, creates the matching `vX.Y.Z` tag, publishes crates, pushes
the tag after publish succeeds, then dispatches `release.yml` for binary assets.
Do not manually push `v*` tags and do not run `release.yml` directly.

Keep the workspace version, internal `=X.Y.Z` dependency pins, git tag, and
GitHub Release tag aligned. Linux release assets are per-app/per-target archives
for `hog-gateway`, `groundhog`, `rv32-lower`, `hogsim`, and
`groundhog-faucet`. macOS ARM64 release assets contain `groundhog`, which links
the RV32 lowerer in-process. Releases also include aggregate `SHA256SUMS`.

### Runtime Manifest Smoke

Use this for the local gateway/bench path once a real `hog-devnode` binary
is available:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode scripts/smoke-runtime-manifest-bench.sh
```

The script starts `groundhog node`, reads `data.runtime_manifest.path`
from the CLI startup JSON, runs `groundhog-bench` against the CLI node,
starts `hog-gateway --runtime-manifest <path>`, and runs the same
bench checks against the standalone gateway. `BENCH_RATE`,
`BENCH_DURATION`, and `BENCH_ACCOUNTS` can tune the run.
Use `BENCH_SCENARIO=gateway-baseline` or `BENCH_SCENARIO=submit-ramp`
for the repeatable local benchmark scenarios documented in
`docs/gateway/bench-scenarios.md`.

The broader local-DX smoke remains:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode scripts/smoke-local-dx.sh
```

With `SMOKE_STANDALONE_GATEWAY=1`, that script also starts the standalone
gateway from the runtime manifest; do not rehydrate gateway TOML by hand
from individual endpoint fields unless you are testing config overrides.

For simulator local-target work, use the script so the local-provider and
existing-provider process smokes stay in sync:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode scripts/smoke-sim-local.sh
```

These smokes start `groundhog node`, run `hog-devnode`, and connect to localhost.
Nodes started without `--data-dir` write their PID file under
`~/.cache/groundhog`; persistent nodes write `<data-dir>/hog-devnode.pid`. The
local-DX smoke uses a per-run persistent directory and verifies stop/restart
recovery. Codex should run these smokes outside the sandbox. If they fail with
permission, localhost-connect, network, or cache-write errors, rerun the same
command with escalation rather than rewriting the workflow. Use absolute paths
for `hog-devnode`; `--data-dir` itself expands a leading `~` and reports the
resolved absolute path.

For the `groundhog-testkit` live contract smoke, build the `caller-counter`
fixture from a `groundhog-examples` checkout and run the ignored contract test.
Its `local` provider launches and owns `hog-devnode` in-process through the
supervisor (not `groundhog node`) and keeps no PID file. The script covers both
the default submit-and-wait flow and manual block production with staged
deployments/calls:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
GROUNDHOG_EXAMPLES_ROOT=/path/to/groundhog-examples \
scripts/smoke-testkit-local.sh
```

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
cargo run -p groundhog-cli -- test
cargo run -p groundhog-cli -- test --json  # structured TestReport on stdout
cargo run -p groundhog-cli -- deploy       # from inside a project
cargo run -p groundhog-cli -- call my-counter get
cargo run -p groundhog-cli -- keys generate alice
cargo run -p groundhog-cli -- keys ls
# Local node backed by hog-devnode
cargo run -p groundhog-cli -- node --hog-devnode-binary /path/to/hog-devnode
# Persist state across clean stop/restart cycles
cargo run -p groundhog-cli -- node --hog-devnode-binary /path/to/hog-devnode \
  --data-dir .groundhog/node
# Machine-readable startup metadata includes data.rpc_url and
# data.runtime_manifest.path/hash, data.chain_state, and resolved data.data_dir.
cargo run -p groundhog-cli -- --json node --hog-devnode-binary /path/to/hog-devnode
# Typical local loop
# 1. cargo run -p groundhog-cli -- new my-counter && cd my-counter
# 2. cargo run -p groundhog-cli -- build
# 3. cargo run -p groundhog-cli -- node --hog-devnode-binary /path/to/hog-devnode  # separate terminal
# 4. cargo run -p groundhog-cli -- deploy
# 5. cargo run -p groundhog-cli -- call my-counter get
```

For manual gateway/bench runs, start `groundhog node --json`, extract
`data.runtime_manifest.path`, then pass it to both consumers:

```bash
hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
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

- Commands in `groundhog-cli` use the `Print` struct (`print.status()`,
  `print.warn()`, `print.prompt()`), not raw `eprintln!`.
  Standalone build tools such as `groundhog-agent-knowledge` may report errors
  directly to stderr and exit nonzero; do not depend on the product CLI solely
  for its output helper.
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
  reserved for future typed runtime payloads. When an error carries a
  structured `data` payload, model the payload as a serde type in
  `json-rpc-types` (currently `TxInvalidDetail` for `TxInvalid`) and let
  `open-rpc-spec` project its schema onto the error component as
  `x-data-schema`; never hand-author the shape in the spec.
- `sendRawTransaction` pre-checks (expiry against the tip, attached-contract
  count and per-contract size from the tip's network config, duplicate
  attachments, and each attached RV32 artifact's declared guest memory and
  code size against the config's `max_rv32_memory_bytes` and
  `max_rv32_code_bytes`) mirror what hogd's executor rejects before running a
  transaction. The bound logic lives in `groundhog-tx-validation`;
  `json-rpc-server` applies it from one cached db-tip reading
  (`ChainPlane::chain_tip` returns `TipReading`, tip plus network config)
  that every tip fetch refreshes: a submit that finds it stale, the public
  `chainTip` method, and a background task `GroundhogRpcServer::new` starts
  on `tip_refresh_interval_ms`. A bound violation found against a cached
  reading is re-checked against a freshly fetched tip before it is
  returned, so a reading that lags a config upgrade never rejects what the
  current config admits. The checks are advisory, not a trust
  boundary: a missing reading never blocks submission. A bound is enforced
  only with a value the protocol defines: the one the tip's config states,
  or a constant the protocol fixes for the unstated case (today
  `kDefaultMaxContractSizeBytes`, `kDefaultMaxRv32CodeBytes`, and
  `kDefaultMaxRv32MemoryBytes`). Never copy hogd's own fallbacks for
  parameters the protocol leaves unstated.
- `groundhog build` enforces no chain bound: it is offline and does not know
  the target chain. The lowerer stamps the guest data memory the package's
  `[package.metadata.groundhog] initial_memory_bytes` declares (the protocol
  default 64 KiB otherwise), warns when the code section is over the
  protocol-default code limit, and leaves admission to the deploying chain,
  whose `sendRawTransaction` pre-check names the violated bound. Do not add
  build-time flags that gate on a chain's limits.
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

The `groundhog_version` RPC exposes `wireFormat`, `implementation`, optional
`releaseVersion`, optional `bundleManifestDigest`, and `commitHash`. It still
accepts the legacy optional `platformVersion` field from older servers, but the
gateway no longer emits it. SDKs and other consumers may branch on `wireFormat`
only; the other fields are diagnostic. `implementation` and `commitHash`
identify the serving component build. The release fields are present only when
that build matches a validated installed bundle manifest.

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
  helpers such as `find_project` and RV32 artifact-path construction.

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
- `docs/cli/specs/2026-05-07-transaction-detail-options.md` —
  transaction detail / receipt response shape.
- `docs/cli/specs/2026-06-25-remote-devnet-faucet-design.md` —
  remote devnet network UX, faucet funding, and wallet-create flow.
- `docs/cli/specs/2026-08-17-wasm-removal-design.md` — WASM removal
  audit and slice plan for the tooling workspace.
- `docs/gateway/roadmap.md` — gateway backlog and sequencing.
- `docs/gateway/system-design.md` — gateway architecture.
- `docs/gateway/runtime-manifest.md` — runtime manifest quickstart for
  CLI, gateway, and benchmark runs.
- `docs/gateway/examples/runtime-manifest.hogd-dev.example.json` —
  example v2 runtime manifest shape. It is not runnable because endpoint
  ports, wallet addresses, and artifact hashes are runtime state.
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
- `docs/archive/cli/2026-05-03-ts-codegen-design.md` — shipped
  TypeScript binding codegen design.
- `docs/archive/cli/2026-05-07-view-rpc-and-receipt-return-data.md` —
  shipped view RPC and structured receipt return-data wire design.
- `docs/archive/cli/2026-05-11-openrpc-wire-version-design.md` — shipped
  OpenRPC wire-version policy and `groundhog_version` design.
- `docs/archive/cli/2026-05-13-view-rpc-protocol-rs-follow-up.md` —
  shipped protocol/protocol-rs service contract for view execution.
- `docs/archive/cli/foundry.md` — Foundry DX analysis kept as design
  reference; not a current task list.

## Context

- Groundhog is an L1 blockchain (SCP consensus, RV32 execution, NNInt
  storage). Instant finality, no forks, no gas fees.
- Transaction lifecycle is simpler than Ethereum/Substrate: Pending →
  Committed (or Invalid/Expired). No reorgs, no "included but not
  finalized".
- The hogd indexer (C++, RocksDB) already has working query APIs. The
  gateway's job is putting a standard JSON-RPC interface in front of
  them.
- The CLI wraps the Rust/RV32 build workflow and integrates with
  the gateway client for deploy, call, and status commands.
- The gateway submits directly to hogd scheduler; submission through
  chuckd is not a target architecture.
