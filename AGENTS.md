# Agent guidance

This file governs work in the Groundhog off-chain tooling workspace.
Use current source, manifests, and tests to verify behavior; dated plans may
describe work that has shipped or changed. Start with [README.md](README.md)
for setup and [docs/README.md](docs/README.md) for focused documentation.

## Working rules

- Keep changes focused. Preserve unrelated work and use feature branches and
  PRs; never commit directly to `main`.
- Commit messages should explain what changed and why in a short title and
  body. Omit review-process narration and inventories of files or types.
- Keep this file about repository decisions and constraints. Put user workflows
  in the CLI reference and operator procedures in `docs/`; link instead of
  maintaining parallel explanations. Code, tests, generated schemas, and
  executable examples are the implementation authority. Retain design prose
  only for useful rationale and component relationships. Keep proposals labelled
  separately; after shipping, extract durable rationale and remove the plan and
  completion records. Git history preserves them; do not grow a docs archive.
- Write README and docs for readers without team context. State access and
  dependency prerequisites explicitly; separate contributor setup from
  maintainer-only publishing procedures.

## Find the owner

| Area | Owner and boundary |
| --- | --- |
| Public JSON-RPC methods | `crates/json-rpc/json-rpc-api/src/lib.rs`: method names, parameters, results, and wire version |
| RPC payloads and errors | `crates/json-rpc/json-rpc-types`: JSON projections, hex wrappers, error catalog, bootstrap/runtime manifests |
| Public server behavior | `crates/json-rpc/json-rpc-server`: shared validation, canonical transaction hashing, and backend traits |
| Production gateway | `crates/json-rpc/json-rpc-gateway`: backend connections, timeouts, metrics, and degraded-mode policy |
| Local node | `crates/node-supervisor`: supervises `hog-devnode`, verifies its manifest, embeds the shared RPC server |
| hogd result mapping | `crates/x23/hogd-backend`: shared decoding, submit packing, and transaction-status probe ordering; no connections, timeout/retry policy, or metrics |
| Internal transport | `crates/x23/x23-transport`: x23 pool and connection helpers; not a public SDK API |
| Embeddable client | `crates/client`: supplied-spec codecs and contract invocation with resolved inputs and caller-owned RPC clients; no CLI config, discovery, terminal output, compiler, or node supervision |
| CLI | `crates/cli`: command logic in the library; `main.rs` dispatches |
| Transaction bounds | `crates/tx-validation`: off-chain mirror of executor checks |
| Runtime acquisition | `crates/toolchain`: trusted catalog, acquisition, and cache; lookup is local and non-mutating, acquisition is explicit |
| RV32 format/build | `crates/rv32/artifact` owns the format and validation; `crates/rv32/lower` lowers ELF to `.rv32a` |

Use `Cargo.toml` for the complete crate list. The `hogd` repository owns the
runtime. Canonical XDR definitions live in `protocol`, with Rust bindings in
`protocol-rs` and TypeScript bindings in `protocol-ts`. Generated TypeScript
contract bindings import from `@groundhogchain/client` in `client-ts`; do not
re-derive protocol primitives in the CLI.

## RPC contracts

- Reuse `groundhog-xdr` types and shared `json-rpc-types` projections. Keep
  public method implementations in `json-rpc-server`; binaries supply backend
  adapters. Add hogd result decoding and composite status ordering to
  `hogd-backend`, not separate gateway/supervisor implementations.
- RPC fields use `camelCase`, tagged enums, decimal JSON quantities, and
  `0x`-prefixed lowercase hex bytes. Use `HexBytes` or `Topic` from
  `json-rpc-types/src/hex.rs`, not raw `Vec<u8>` in RPC payloads. Preserve
  opaque server-authored cursors verbatim.
- Request structs reject unknown fields with `deny_unknown_fields`. For
  internally tagged enums with unit variants, follow `BlockAnchor`'s
  struct-variant deserialization mirror; the attribute alone is insufficient.
- Enforce shared request bounds in `json-rpc-server`; the gateway supplies
  production limits. Mark extensible public enums and manifest structs
  `#[non_exhaustive]`. In manifests, omit unavailable optional endpoints and
  encode large quantities as decimal strings.
- `sendRawTransaction` succeeds only after authoritative mempool admission.
  Admission does not imply nomination or commitment. Keep advisory executor
  pre-checks in `tx-validation`, using the shared tip/config cache. Recheck
  cached bound violations against a fresh tip before rejecting; a missing
  reading must not block submission. Use only stated config values or defaults
  fixed by the protocol, never unstated hogd implementation fallbacks.
- `groundhog build` is offline and must not enforce a target chain's limits.
  Keep chain admission checks at submission.
- Generate OpenRPC/JSON Schema from Rust traits and types. Never hand-edit
  `crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json` or maintain
  parallel RPC schemas. The error catalog is `json-rpc-types/src/error.rs`;
  update the per-method error mapping in `open-rpc-spec/src/lib.rs` when adding
  methods/errors. Model structured error data as serde types and project it
  via `x-data-schema`.

### OpenRPC Wire Version

`WIRE_FORMAT_VERSION` in `json-rpc-api/src/lib.rs` supplies OpenRPC
`info.version`, independently of the workspace version. Bump major for
breaking method/parameter/result/error changes, minor for additions, and patch
for documentation or example refinements. Consumers branch on `wireFormat`;
implementation, commit, and release identity fields are diagnostic. Release
identity requires a validated installed bundle matching the serving build.

## Rust and CLI conventions

- Use the toolchain pinned in `rust-toolchain.toml`, edition 2024, and rustfmt
  (100-column width). Use `thiserror`; no `anyhow`.
- No `.unwrap()` outside tests. Reserve `.expect("bug: ...")` for true
  invariants, never untrusted input. Propagate typed errors and add context
  through enum variants.
- Document public items' behavior and non-obvious contracts; skip comments
  that only repeat names/signatures. Derive `Debug` on public types without
  exposing secrets. Prefer borrowed inputs and explicit `From` conversions
  between related types so field additions are caught by the compiler.
- Remove unused code instead of reserving speculative stubs. Name things for
  their role or behavior. Check external tools on `PATH` and return useful
  errors when absent.
- Use `tracing` at service boundaries, including
  `#[tracing::instrument(skip_all, fields(...))]` on public async handlers and
  subsystem methods. No temporary production debug prints. Keep useful test
  diagnostics free of secrets.
- CLI interaction and rendering belong in `Print` (`src/print.rs`). Use its
  prompt/confirmation/input and terminal-style helpers. `--non-interactive`
  disables prompts and terminal rendering without granting consent or hiding
  ordinary output. Child runners must not inherit terminal stdin or force
  color in this mode. Explicit piped input is distinct from prompting.
- Card borders and titles use the terminal accent; standalone outcome messages
  use semantic colors. Progress must not query the cursor or change input modes.
  Explicit live-output modes (verbose builds/nodes and test output passthrough)
  stream immediately without animation or deferred diagnostic collection.
- Support `--json` through `Print` and `CommandResult`. Stable command reports
  use typed serde/JsonSchema payloads with `camelCase` fields and
  `data.schemaVersion` from v1. Do not version the shared envelope per command.
  Emit typed success and failure records on stdout, with no status text mixed
  in; watch output is newline-delimited JSON. Existing generic preflight
  errors may keep their stderr envelope. Test the binary's success and failure
  shapes. Standalone build tools need not depend on the CLI for stderr output.
- Declare `CliError` code/hint/details metadata beside each variant using
  `ErrorMeta` attributes. Avoid `CliError::Other` for routine user failures.
  Regenerate intentional metadata changes with
  `UPDATE_SNAPSHOTS=1 cargo test -p groundhog-cli --test error_metadata`,
  inspect the diff, then rerun without the update flag. Computed metadata
  needs value-specific tests; record deliberately shared codes in `SHARED_CODES`.
- Never print or log private signing material. Do not pass credentials in
  subprocess arguments; use environment variables or stdin. Private-key files
  use `0o600`, secret bytes use `zeroize::Zeroizing`, and public-key inspection
  uses `load_key_info` without loading the private key.

## Validation

Start with tests for the changed behavior. Before pushing Rust changes, run
the workspace checks used by [CI](.github/workflows/ci.yml):

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

These are the core Rust checks, not the entire CI pipeline. For documentation
only, check links, paths, command claims against source, and `git diff --check`.
Report checks actually run and any environment blockers.

- Unit tests live inline; integration tests in each crate's `tests/`. Use
  `#[tokio::test]` for async tests and serialize environment-mutating tests
  with `serial_test`. CLI binary tests use `env!("CARGO_BIN_EXE_groundhog")`.
- Keep generated artifacts under snapshot tests plus structural invariants.
  Regenerate intentional changes with the relevant `UPDATE_SNAPSHOTS=1` test,
  inspect the diff, and rerun without that variable so the comparison runs.
- CLI scaffold/codegen changes also need the relevant generated-project or
  TypeScript checks in CI. Compilation alone does not validate generated
  consumers. Build/test workflows need private SDK registry access.
- Process smokes require a compatible real `hog-devnode`, registry access,
  localhost access, and writable runtime/cache directories. Use absolute
  binary paths. Run the applicable script with
  `GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode`:

  | Change | Script |
  | --- | --- |
  | CLI contract loop and persistent restart | `scripts/smoke-local-dx.sh` |
  | Gateway/runtime manifest/bench | `scripts/smoke-runtime-manifest-bench.sh` |
  | Simulator local/existing providers | `scripts/smoke-sim-local.sh` |
  | Testkit contract flow | `scripts/smoke-testkit-local.sh` (also set `GROUNDHOG_EXAMPLES_ROOT`) |

  Sandbox permission failures are not product failures: obtain the necessary
  execution permissions instead of changing the workflow to bypass checks.
  Pass the emitted runtime manifest to gateway/bench consumers rather than
  reconstructing backend configuration. See
  [runtime-manifest docs](docs/gateway/runtime-manifest.md) and
  [simulator docs](docs/sim/README.md) for details.

## Bundled knowledge and releases

For Groundhog programming-model or developer-workflow changes, load the
checkout's bundled guidance with `cargo run -p groundhog-cli -- --skill`.
If building is unavailable, read
`crates/cli/assets/groundhog/knowledge/generated/agent-guide.md`.
Editable sources are `knowledge/bundle.toml` and its records; generated files
must be assembled, not edited. Review the bundle on workspace/CLI version
bumps and changes to CLI, scaffold, SDK, bindings, client, wallets, or public
RPC workflows. Verify claims against current APIs, examples, and installable
versions, then run:

```sh
cargo run -p groundhog-agent-knowledge -- build
cargo run -p groundhog-agent-knowledge -- check
cargo test -p groundhog-cli skill
cargo test -p groundhog-agent-knowledge
cargo package -p groundhog-cli --list --allow-dirty --locked
```

Inspect the package list for the bundled knowledge files. Keep the selected
developer-site snapshot explicit; follow
[bundled documentation maintenance](docs/bundled-documentation.md).

For releases, keep workspace versions and internal exact dependency pins
aligned. Use **Release package** (`publish.yml`); do not manually push `v*`
tags or dispatch `release.yml` directly. See [release instructions](docs/releases.md)
for artifact and CPU-target constraints.
