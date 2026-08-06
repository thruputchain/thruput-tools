# Groundhog Developer Tools

Off-chain developer tooling for Groundhog smart contracts: the `groundhog` CLI,
JSON-RPC gateway, hosted faucet, reusable server layer, signing library, typed
client, simulator, benchmark harness, and canonical OpenRPC artifact.

## Quick Orientation

- **Building on Groundhog?** (writing contracts, building SDKs,
  integrating with the JSON-RPC API) → [Building on Groundhog](#building-on-groundhog).
- **Contributing to this repo?** → [Contributing](#contributing).

---

## Building on Groundhog

### Install the CLI from this checkout

From the repository root:

```bash
cargo install --path crates/tools/cli --locked --force
groundhog --version
```

Cargo installs the binary under `~/.cargo/bin` by default. `--force` replaces
an older checkout build if one is already installed.

During early access, installing from this checkout and building generated
projects both require access to the private Groundhog Cargo registry. Follow
the invite instructions to authenticate Cargo first. This constraint will
relax when the SDK ships publicly.

### Local Dev Path

The CLI supervises `hog-devnode`, serves the public JSON-RPC surface, and runs
the contract loop locally:

```bash
export GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode

groundhog init                   # one-time guided setup
groundhog new my-counter && cd my-counter
groundhog status                 # verify the project toolchain and local runtime artifacts
groundhog build                  # RV32 artifact by default
groundhog test                   # executor-backed RV32 tests
```

From the generated project directory, start the local node in one terminal:

```bash
groundhog node
```

By default the node is ephemeral. Pass `--data-dir <dir>` to persist its
committed chain and recover it on the next launch with the same bootstrap
options:

```bash
groundhog node --data-dir .groundhog/node
```

With the node stopped, the persistent directory can be saved and restored as
a single compressed JSON archive:

```bash
groundhog chain dump --data-dir .groundhog/node --output chain-backup.json
groundhog chain restore --data-dir .groundhog/node --input chain-backup.json --force
```

Then deploy and call it from the same project directory in another terminal:

```bash
groundhog deploy
groundhog call my-counter get
```

The integration path manages its own ephemeral local node:

```bash
# one-command local-node integration path; add --target wasm for WASM artifacts
groundhog test --integration

# differential gate: WASM and RV32 must both pass the same tests
groundhog test --integration --differential
```

`groundhog node` is script-safe: human status or startup JSON goes to stdout;
child logs stay on stderr. Use `-v` / `--verbose` for normal debugging and
`RUST_LOG` for exact tracing filters. Startup output reports `chain state` for
humans and `data.chain_state` (`fresh` or `recovered`) in JSON.

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
describing live x23 endpoints, bootstrapped dev wallets, and funding. The
standalone gateway and benchmark harness consume the same file:

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

For local protocol skew, pass `groundhog node --hogd-protocol hogp/<hash>`. This
only overrides the x23 intro token; XDR shape mismatches still fail in generated
clients.

CLI dev keys (`dev-0`, `dev-1`, ...) use the runtime manifest for wallet-address
discovery. `groundhog node` writes the active local endpoint and runtime
manifest path into global `local_runtime_state`; profiles with
`network = "local"` consume that state. Scripts can set
`GROUNDHOG_RUNTIME_MANIFEST` directly.

See [Runtime Manifest Quickstart](docs/gateway/runtime-manifest.md) for the
full CLI/gateway/bench loop,
[Benchmark Scenario Library](docs/gateway/bench-scenarios.md) for repeatable
local load shapes, and
[`docs/gateway/examples/runtime-manifest.hogd-dev.example.json`](docs/gateway/examples/runtime-manifest.hogd-dev.example.json)
for the v2 JSON shape.

### Releases

One normal human entrypoint: run the **Release package** GitHub Actions workflow.

- `.github/workflows/publish.yml` publishes the Rust crates to the private
  `groundhog` Cargo registry, creates and pushes the matching `vX.Y.Z` tag, then
  dispatches binary packaging.
- `.github/workflows/release.yml` is dispatch-only and internal to
  `publish.yml`. Do not run it directly for normal releases.

Before release, bump the workspace version in `Cargo.toml` and keep internal
workspace dependency pins aligned. Do not manually push `v*` tags.

Release assets:

```text
hog-gateway-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
groundhog-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
rv32-lower-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
hogsim-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
groundhog-faucet-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

Each archive contains one app binary plus `README.md` and `LICENSE`. `hog-gateway`,
`hogsim`, and `groundhog-faucet` are binary release artifacts only;
`groundhog-rv32-lower` is also a published crate/library.

### API Surface

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

### Public Surface Design Notes

Current and historical design notes for the JSON-RPC surface:

- [View RPC and receipt return-data](docs/archive/cli/2026-05-07-view-rpc-and-receipt-return-data.md)
  — sandboxed contract views plus structured `return_data` /
  `abort_data` on receipts.
- [View RPC protocol-rs follow-up](docs/archive/cli/2026-05-13-view-rpc-protocol-rs-follow-up.md)
  — protocol/protocol-rs service contract used to wire `groundhog_view`
  to hogd.
- [Transaction detail options](docs/cli/specs/2026-05-07-transaction-detail-options.md)
  — receipt response shape.

### Wire-format versioning

The OpenRPC artifact uses its own SemVer in `info.version`, sourced from
`groundhog_json_rpc_api::WIRE_FORMAT_VERSION`, rather than the Rust workspace
version. SDKs should pin on `wireFormat` from `groundhog_version`;
workspace releases identify builds, not the JSON-RPC contract.

### Mini-Glossary

Terms you'll encounter in the CLI output, the OpenRPC artifact, or
this repo's docs:

- `hog-devnode` — single-process Groundhog node implementation used for
  local development.
- `indexer` — receipt / event / transaction-status query plane.
- `scheduler` — submit-tx + chain-tip plane.
- `scheduler`, `indexer`, `dbGateway`, `viewGateway` — runtime-manifest
  endpoint keys for internal x23 services. They are not public JSON-RPC
  methods or JSON-RPC server planes.
- `dev-N` — deterministic dev accounts (`dev-0` ... `dev-N`) the local
  dev backend funds and signs as.
- `x23` — wire protocol between the supervisor and the `hog-devnode`
  subprocess. Internal; SDKs don't touch it.

---

## Contributing

### Crate Layout

Workspace crates are grouped by boundary:

- [`crates/json-rpc/`](crates/json-rpc/) — public JSON-RPC API surface,
  gateway, typed client, shared server implementation, and OpenRPC spec
  generation.
- [`crates/x23/`](crates/x23/) — internal x23 transport helpers used to reach
  hogd services.
- [`crates/tools/`](crates/tools/) — developer-facing tools and supporting
  libraries.
- [`crates/rv32/`](crates/rv32/) — RV32 artifact validation and ELF lowering.
- [`crates/dev/`](crates/dev/) — local development, simulation, benchmark, and
  process-level test harnesses.
- [`crates/faucet/`](crates/faucet/) — hosted network descriptor, airdrop, and
  wallet-creation service.

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
| RV32 | [`artifact`](crates/rv32/artifact/) | Canonical RV32 artifact format and validation |
| RV32 | [`lower`](crates/rv32/lower/) | ELF-to-RV32 artifact lowerer and `rv32-lower` binary |
| Service | [`faucet`](crates/faucet/) | Hosted network descriptor, airdrop, and wallet-creation service |
| Dev | [`node-supervisor`](crates/dev/node-supervisor/) | `groundhog node` facade: supervises `hog-devnode` and serves JSON-RPC via `json-rpc-server` |
| Dev | [`e2e-tests`](crates/dev/e2e-tests/) | Process-level JSON-RPC smoke tests |
| Dev | [`bench`](crates/dev/bench/) | Benchmark harnesses |
| Dev | [`sim`](crates/dev/sim/) | `hogsim` stateful simulation and soak-test harness |

### Local Dev Setup

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The standard contributor smoke is `scripts/smoke-local-dx.sh` — runs
the user-facing CLI loop (`new` → `build` → `node` → `deploy` →
`call` → view → storage) against a real `hog-devnode` binary. Set
`GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode` and invoke.

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

Generated projects require Cargo registry access while the SDK lives in the
private registry. See [`crates/tools/cli/README.md`](crates/tools/cli/README.md)
for the full CLI reference.

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

### Design Rationale

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
