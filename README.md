# Groundhog Developer Tools

Off-chain developer tooling for the Groundhog smart-contract platform:
the `groundhog` CLI, a JSON-RPC gateway, a reusable JSON-RPC server
layer, a transaction signing library, and a typed JSON-RPC client. The
canonical OpenRPC artifact for downstream SDKs is produced from this
monorepo.

## Quick orientation

Two audiences land here. Skip to whichever fits:

- **Building on Groundhog?** (writing contracts, building SDKs,
  integrating with the JSON-RPC API) → [Building on Groundhog](#building-on-groundhog).
- **Contributing to this repo?** → [Contributing](#contributing).

---

## Building on Groundhog

### Local dev path

The `groundhog` CLI runs an isolated Groundhog chain in a `hog-devnode`
subprocess and gives you a normal dev loop on top:

```bash
cargo install --path crates/tools/cli
groundhog init                   # one-time environment check
groundhog login                  # one-time; early-access registry auth
groundhog new my-counter && cd my-counter
groundhog build                   # RV32 artifact by default
groundhog test                   # executor-backed WASM tests
# in another terminal
groundhog node --hog-devnode-binary /path/to/hog-devnode
groundhog deploy
groundhog call my-counter get
# one-command local-node integration path; add --target wasm for WASM artifacts
GROUNDHOG_HOG_DEVNODE_BINARY=/path/to/hog-devnode groundhog test --integration
# differential gate: WASM and RV32 must both pass the same tests
GROUNDHOG_HOG_DEVNODE_BINARY=/path/to/hog-devnode groundhog test --integration --differential
```

`groundhog login` is currently required for the Groundhog contract SDK
to resolve from the private crate registry that `groundhog new` writes
into the project's `Cargo.toml`. This is an early-access constraint
and will relax when the SDK ships publicly.

`hog-devnode` is the single-process Groundhog node implementation used
for development — scheduler, indexer, and storage in one binary. The
`groundhog node` supervisor manages its lifecycle and serves the
public JSON-RPC on top, so every CLI command (`deploy`, `call`,
receipts, events, storage reads) talks to the same JSON-RPC surface
an SDK would.

`groundhog node` keeps stdout script-safe. In human mode stdout carries
status lines; in `--json` mode stdout carries the startup JSON payload.
Child-process logs are requested from `hog-devnode` as newline-delimited JSON on
stderr, parsed by `node-supervisor`, and re-emitted through Rust `tracing` with
`hogd_timestamp_ms`, `hogd_target`, `hogd_file`, `hogd_line`, and
`hogd_fields` metadata.
The default `groundhog node` output is the quiet golden path; use
`groundhog node -v` / `--verbose` for the normal debugging view, including
with `--json` because logs stay on stderr, and `RUST_LOG` for exact tracing
filters.

`groundhog test --integration` uses that same local-node path without a
separate terminal: it builds the selected artifact (`--target rv32` by default,
or `--target wasm`), starts an ephemeral hog-devnode node, deploys with
`dev-0`, runs `tests/integration.rs`, then shuts the node down. The test
process receives `GROUNDHOG_RPC_URL`, `GROUNDHOG_CONTRACT_ADDRESS`,
`GROUNDHOG_PROFILE=integration`, `GROUNDHOG_DEFAULT_KEY=dev-0`, and
`GROUNDHOG_RUNTIME_MANIFEST`.

`groundhog test --integration --differential` is the differential gate: it
builds both WASM and RV32 artifacts, runs the same integration target against
isolated ephemeral nodes, and fails unless both runs pass with matching parsed
test outcomes.

For gateway and benchmark runs, `hog-devnode` also writes a runtime manifest
describing the live x23 endpoints, bootstrapped dev wallets, and funding
state. `groundhog node --json` reports the manifest under
`data.runtime_manifest.path`; the standalone gateway and benchmark harness can
consume the same file:

```bash
# in one terminal
groundhog --json node --hog-devnode-binary /path/to/hog-devnode > /tmp/node.json

# in other terminals
RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/node.json)
hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction
```

When testing a local `hog-devnode` build whose `hogp/<hash>` is newer than the
published Rust protocol crates, pass the token explicitly with
`groundhog node --hogd-protocol hogp/<hash>`. This only overrides the intro
handshake string; if the XDR wire shape has actually changed, the generated
client still fails on the affected RPC.

CLI commands using local dev keys (`dev-0`, `dev-1`, ...) read the runtime
manifest for wallet-address discovery. `groundhog node` writes
`.groundhog/local-node.json` when launched from a project, so commands in
another terminal can discover the live RPC URL and manifest automatically, and
removes that state on shutdown. This project-local state only fills missing
dev-key config or replaces loopback RPC URLs; scripts and non-local profiles
can also set `GROUNDHOG_RUNTIME_MANIFEST` or
`profile.<name>.runtime_manifest` explicitly.

See [Runtime Manifest Quickstart](docs/gateway/runtime-manifest.md) for the
full CLI/gateway/bench loop,
[Benchmark Scenario Library](docs/gateway/bench-scenarios.md) for repeatable
local load shapes, and
[`docs/gateway/examples/runtime-manifest.hogd-dev.example.json`](docs/gateway/examples/runtime-manifest.hogd-dev.example.json)
for the v1 JSON shape.

### Releases

There are two release workflows, but only one human entrypoint:

- `.github/workflows/publish.yml` publishes the Rust crates to the private
  `groundhog` Cargo registry and owns release orchestration.
- `.github/workflows/release.yml` is dispatch-only and internal to
  `publish.yml`. It builds Linux `hog-gateway`, `groundhog`, `rv32-lower`, and
  `hogsim` binaries, packages each app as a per-target `.tar.gz`, and attaches
  the archives plus `SHA256SUMS` to a GitHub Release.

For a normal release, bump the workspace version in `Cargo.toml` and keep the
internal workspace dependency pins in sync. Then run the **Release package**
workflow manually from GitHub Actions. That workflow dry-runs publishing,
creates the matching local tag from the Cargo version (for example `v0.3.6`),
publishes the crates, pushes the tag after publishing succeeds, and explicitly
dispatches the binary release workflow on that same tag.

Do not run `.github/workflows/release.yml` directly for normal releases and do
not manually push `v*` tags. Direct human dispatch of the release workflow
fails; it must be dispatched by the publish workflow. The release workflow
verifies that the matching `groundhog-cli` and `groundhog-rv32-lower` crate
versions already exist in the private registry before it uploads binary assets.
`hog-gateway` is built from this repository for the GitHub Release, but its binary
crate is not published to the Cargo registry. `hogsim` is also a binary release
artifact only. `groundhog-rv32-lower`, by contrast, is published, so it is
consumable as a library (e.g. from sdk_rs) as well as via its `rv32-lower`
binary.

The result is one matching version across the Cargo crates, git tag, and
GitHub Release. Release assets follow this naming pattern:

```text
hog-gateway-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
groundhog-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
rv32-lower-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
hogsim-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

Each archive contains one app binary plus `README.md` and `LICENSE`. The
release repo can consume the `hog-gateway` archive by pinning `groundhog-rpc` in
`release-manifest.toml`; CLI packaging can consume the `groundhog` archive the
same way, and simulator packaging can consume the `hogsim` archive.

### The API surface

The JSON-RPC contract lives in the OpenRPC artifact at
[`crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json`](crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json),
also published as the `groundhog-open-rpc-spec` crate on the private
registry. The current public JSON-RPC surface uses one method namespace:

- `groundhog_*` — public surface intended for SDK consumption.

Local dev-key metadata for `dev-N` workflows is intentionally outside JSON-RPC;
the CLI, gateway, and bench harness discover it from the runtime manifest.

Wire-format conventions:

- `0x`-prefixed lowercase hex for all byte values (hashes, addresses,
  topics, event data).
- Decimal JSON numbers for heights, counts, and other quantities (no
  Ethereum-style hex quantities).
- `camelCase` JSON field names.
- Tagged unions with `kind` or `status` discriminators.
- Cursors are opaque server-authored `0x` strings — reflect verbatim.

See the "Gateway Conventions" section of [`CLAUDE.md`](CLAUDE.md) for
the full convention.

### Errors

Every method declares an `errors[]` array referencing
`components.errors` in the artifact. Error codes follow Substrate's
per-module range pattern; the canonical catalog (stable name, code,
short message, consumer-facing description) lives in
[`crates/json-rpc/json-rpc-types/src/error.rs`](crates/json-rpc/json-rpc-types/src/error.rs).

Codes worth knowing for retry policy:

- `Overloaded` (-32053) — request never reached the backend; retry safe.
- `BackendTimeout` (-32051) — outcome ambiguous; idempotent retries only.
- `TxRejected` (-32001) — permanent rejection; do not retry.

### TypeScript

XDR types, contract specs, and JSON-RPC primitives for TypeScript
consumers live in the `protocol-ts` peer repository as
`@groundhogchain/protocol`, `/contract-spec`, `/xdr`, and `/client`.
The canonical TS SDK that codegens against the OpenRPC artifact and
sits on top of those primitives is the right consumer-facing entry
point — link will land here once it ships.

### Public surface design notes

Recent and active design notes for the JSON-RPC surface:

- [View RPC and receipt return-data](docs/cli/specs/2026-05-07-view-rpc-and-receipt-return-data.md)
  — read-only contract calls plus structured `return_data` /
  `abort_data` on receipts.
- [View RPC protocol-rs follow-up](docs/cli/specs/2026-05-13-view-rpc-protocol-rs-follow-up.md)
  — protocol/protocol-rs service contract used to wire `groundhog_view`
  to hogd.
- [Transaction detail options](docs/cli/specs/2026-05-07-transaction-detail-options.md)
  — receipt response shape.

### Wire-format versioning

The OpenRPC artifact uses its own SemVer in `info.version`, sourced from
`groundhog_json_rpc_api::WIRE_FORMAT_VERSION`, rather than the Rust workspace
version. SDKs should pin on `wireFormat` from `groundhog_version`;
workspace releases such as `0.3.x` identify builds, not the JSON-RPC
contract.

### Mini-glossary

Terms you'll encounter in the CLI output, the OpenRPC artifact, or
this repo's docs:

- `hog-devnode` — single-process Groundhog node implementation used for
  local development.
- `indexer` — receipt / event / transaction-status query plane.
- `scheduler` — submit-tx + chain-tip plane.
- `scheduler_coord`, `indexer_block_explorer`, `db_gateway`,
  `view_gateway` — hog-devnode readiness endpoint keys. These name x23
  services, not public JSON-RPC methods or JSON-RPC server planes.
- `dev-N` — deterministic dev accounts (`dev-0` ... `dev-N`) the local
  dev backend funds and signs as.
- `x23` — wire protocol between the supervisor and the `hog-devnode`
  subprocess. Internal; SDKs don't touch it.

---

## Contributing

### Crate Layout

Workspace crates are grouped by boundary instead of kept flat under
`crates/`:

- [`crates/json-rpc/`](crates/json-rpc/) — public JSON-RPC API surface,
  gateway, typed client, shared server implementation, and OpenRPC spec
  generation.
- [`crates/x23/`](crates/x23/) — internal x23 transport helpers used to reach
  hogd services.
- [`crates/tools/`](crates/tools/) — developer-facing tools and supporting
  libraries.
- [`crates/dev/`](crates/dev/) — local development, simulation, benchmark, and
  process-level test harnesses.

| Group | Crate | Description |
|-------|-------|-------------|
| JSON-RPC | [`json-rpc-types`](crates/json-rpc/json-rpc-types/) | Shared JSON-RPC types, including the error catalog |
| JSON-RPC | [`json-rpc-api`](crates/json-rpc/json-rpc-api/) | Shared jsonrpsee trait definitions |
| JSON-RPC | [`json-rpc-server`](crates/json-rpc/json-rpc-server/) | Shared server-side JSON-RPC implementation and validation |
| JSON-RPC | [`json-rpc-gateway`](crates/json-rpc/json-rpc-gateway/) | JSON-RPC 2.0 gateway server |
| JSON-RPC | [`json-rpc-client`](crates/json-rpc/json-rpc-client/) | Typed JSON-RPC client |
| JSON-RPC | [`open-rpc`](crates/json-rpc/open-rpc/) | OpenRPC document generator |
| JSON-RPC | [`open-rpc-macros`](crates/json-rpc/open-rpc-macros/) | `#[open_rpc]` proc macro emitting OpenRPC method descriptors from jsonrpsee `#[rpc]` traits |
| JSON-RPC | [`open-rpc-spec`](crates/json-rpc/open-rpc-spec/) | Library + binary that assembles the canonical `groundhog-open-rpc-spec` artifact for downstream codegen |
| x23 | [`x23-transport`](crates/x23/x23-transport/) | Shared x23 connection-pool helpers used by gateway and node-supervisor |
| Tools | [`cli`](crates/tools/cli/) | `groundhog` CLI for the contract dev workflow |
| Tools | [`signing`](crates/tools/signing/) | Off-chain transaction signing and construction |
| Dev | [`node-supervisor`](crates/dev/node-supervisor/) | `groundhog node` facade: supervises `hog-devnode` and serves JSON-RPC via `json-rpc-server` |
| Dev | [`e2e-tests`](crates/dev/e2e-tests/) | Process-level JSON-RPC smoke tests |
| Dev | [`bench`](crates/dev/bench/) | Benchmark harnesses |
| Dev | [`sim`](crates/dev/sim/) | `hogsim` stateful simulation and soak-test harness |

### Local dev setup

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The standard contributor smoke is `scripts/smoke-local-dx.sh` — runs
the user-facing CLI loop (`new` → `build` → `node` → `deploy` →
`call` → view → storage) against a real `hog-devnode` binary. Set
`HOG_DEVNODE_BINARY=/path/to/hog-devnode` and invoke.

For the runtime-manifest gateway/bench path, run
`scripts/smoke-runtime-manifest-bench.sh`. It starts `groundhog node`,
extracts the generated runtime manifest, runs `groundhog-bench` against the
CLI node, starts `hog-gateway --runtime-manifest`, and repeats the bench
checks against the standalone gateway. Set `BENCH_SCENARIO=gateway-baseline`
or `BENCH_SCENARIO=submit-ramp` for the heavier local scenario library.

For deterministic stateful simulator runs, use `hogsim` from `crates/dev/sim`
against an already-started `groundhog node` target:

```bash
cargo run -p groundhog-sim --bin hogsim -- validate examples/sim/local-wallet-soak.json
cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-wallet-soak.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest "$RUNTIME_MANIFEST"
```

See [`docs/sim/README.md`](docs/sim/README.md) for the current simulation
scope and result-artifact contract.

The `test` subcommand requires `groundhog login` to authenticate
against the private crate registry that ships the Groundhog contract
SDK. See [`crates/tools/cli/README.md`](crates/tools/cli/README.md) for the full
CLI reference.

### Conventions

[`CLAUDE.md`](CLAUDE.md) is the convention reference for both human
contributors and LLM assistants. Highlights:

- Edition 2024, stable Rust, 100-column max, `thiserror` only (no
  `anyhow`), no `.unwrap()` outside tests.
- Schema ownership: XDR in `protocol-rs/groundhog-xdr` (Rust) and
  `protocol-ts` (TypeScript); JSON-RPC projections in
  `json-rpc-types`; method surface in `json-rpc-api`; OpenRPC
  artifact generated, never hand-authored.
- Wire-format and error-catalog conventions are summarized in the
  "Building on Groundhog" section above and detailed in CLAUDE.md
  "Gateway Conventions".
- CLI machine-readable output: typed report types in `crates/tools/cli`,
  `data.schemaVersion` from v1 onward, `CommandResult` envelope on a
  single stdout stream.

### Design rationale

Active design notes:

- [`docs/README.md`](docs/README.md) — map of living docs vs archived
  historical notes.
- [`docs/cli/specs/`](docs/cli/specs/) — CLI and developer-tooling specs.
- [`docs/cli/roadmap.md`](docs/cli/roadmap.md) — CLI roadmap.
- [`docs/gateway/`](docs/gateway/) — gateway architecture and roadmap.
- [`docs/sim/`](docs/sim/) — `hogsim` simulator design, implementation plan,
  and quickstart.

Shipped decisions live in the code; git history retains the design
context for anything not currently active.

### Cross-team

This repo is the off-chain half of the Groundhog dev surface. Adjacent
components:

- `protocol` — wire-protocol XDR definitions.
- `protocol-rs` — Rust bindings for the XDR types (the `groundhog-xdr`
  crate this repo depends on).
- `protocol-ts` — TypeScript bindings for the same. The peer for
  Rust↔TS parity.
- `hogd` — the canonical Groundhog node. `hog-devnode` is the developer-
  loop variant this repo's `groundhog node` supervises.

See [`docs/gateway/system-design.md`](docs/gateway/system-design.md)
for the full chain-level architecture and open questions.

---

## License

Apache-2.0
