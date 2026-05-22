# Groundhog Developer Tools

Off-chain developer tooling for the Groundhog smart-contract platform:
the `groundhog` CLI, a JSON-RPC gateway, a reusable RPC-server layer,
a transaction signing library, and a typed JSON-RPC client. The
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

The `groundhog` CLI runs an isolated Groundhog chain in a `hogd-dev`
subprocess and gives you a normal dev loop on top:

```bash
cargo install --path crates/cli --features dev-node
groundhog init                   # one-time environment check
groundhog login                  # one-time; early-access registry auth
groundhog new my-counter && cd my-counter
groundhog build
groundhog test                   # executor-backed WASM tests
# in another terminal
groundhog node --hogd-dev-binary /path/to/hogd-dev
groundhog deploy
groundhog call my-counter get
# one-command local-node integration path
GROUNDHOG_HOGD_DEV_BINARY=/path/to/hogd-dev groundhog test --integration
```

`groundhog login` is currently required for the Groundhog contract SDK
to resolve from the private crate registry that `groundhog new` writes
into the project's `Cargo.toml`. This is an early-access constraint
and will relax when the SDK ships publicly.

`hogd-dev` is the single-process Groundhog node implementation used
for development — scheduler, indexer, and storage in one binary. The
`groundhog node` supervisor manages its lifecycle and serves the
public JSON-RPC on top, so every CLI command (`deploy`, `call`,
receipts, events, storage reads) talks to the same JSON-RPC surface
an SDK would.

`groundhog node` keeps stdout script-safe. In human mode stdout carries
status lines; in `--json` mode stdout carries the startup JSON payload.
Child-process logs are requested from `hogd-dev` as newline-delimited JSON on
stderr, parsed by `node-supervisor`, and re-emitted through Rust `tracing` with
`hogd_timestamp_ms`, `hogd_target`, `hogd_file`, `hogd_line`, and
`hogd_fields` metadata.
The default `groundhog node` output is the quiet golden path; use
`groundhog node -v` / `--verbose` for the normal debugging view, including
with `--json` because logs stay on stderr, and `RUST_LOG` for exact tracing
filters.

With the CLI's `dev-node` feature enabled, `groundhog test
--integration` uses that same local-node path without a separate
terminal: it builds the WASM artifact, starts an ephemeral hogd-dev
node, deploys with `dev-0`, runs `tests/integration.rs`, then shuts the
node down. The test process receives `GROUNDHOG_RPC_URL`,
`GROUNDHOG_CONTRACT_ADDRESS`, `GROUNDHOG_PROFILE=integration`, and
`GROUNDHOG_DEFAULT_KEY=dev-0`.

For gateway and benchmark runs, `hogd-dev` also writes a runtime manifest
describing the live x23 endpoints, bootstrapped dev wallets, and funding
state. `groundhog node --json` reports the manifest under
`data.runtime_manifest.path`; the standalone gateway and benchmark harness can
consume the same file:

```bash
# in one terminal
groundhog --json node --hogd-dev-binary /path/to/hogd-dev > /tmp/node.json

# in other terminals
RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/node.json)
groundhog-rpc --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method sendRawTransaction
```

See [Runtime Manifest Quickstart](docs/gateway/runtime-manifest.md) for the
full CLI/gateway/bench loop,
[Benchmark Scenario Library](docs/gateway/bench-scenarios.md) for repeatable
local load shapes, and
[`docs/gateway/examples/runtime-manifest.hogd-dev.example.json`](docs/gateway/examples/runtime-manifest.hogd-dev.example.json)
for the v1 JSON shape.

### The API surface

The JSON-RPC contract lives in the OpenRPC artifact at
[`crates/open-rpc-spec/snapshots/groundhog-spec.json`](crates/open-rpc-spec/snapshots/groundhog-spec.json),
also published as the `groundhog-open-rpc-spec` crate on the private
registry. Two method namespaces:

- `groundhog_*` — public surface intended for SDK consumption.
- `groundhog_dev_*` — local-development helpers (account introspection,
  test-mode utilities). SDKs should not surface these by default.

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
[`crates/api-types/src/error.rs`](crates/api-types/src/error.rs).

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
`groundhog_rpc_api::WIRE_FORMAT_VERSION`, rather than the Rust workspace
version. SDKs should pin on `wireFormat` from `groundhog_version`;
workspace releases such as `0.3.x` identify builds, not the JSON-RPC
contract.

### Mini-glossary

Terms you'll encounter in the CLI output, the OpenRPC artifact, or
this repo's docs:

- `hogd-dev` — single-process Groundhog node implementation used for
  local development.
- `indexer` — receipt / event / transaction-status query plane.
- `scheduler` — submit-tx + chain-tip plane.
- `scheduler_coord`, `indexer_block_explorer`, `db_gateway`,
  `view_gateway` — hogd-dev readiness endpoint keys. These name x23
  services, not public JSON-RPC methods or `rpc-server` planes.
- `dev-N` — deterministic dev accounts (`dev-0` ... `dev-N`) the local
  dev backend funds and signs as.
- `x23` — wire protocol between the supervisor and the `hogd-dev`
  subprocess. Internal; SDKs don't touch it.

---

## Contributing

### Crates

| Crate | Description |
|-------|-------------|
| [`cli`](crates/cli/) | `groundhog` CLI for the contract dev workflow |
| [`gateway`](crates/gateway/) | JSON-RPC 2.0 gateway server |
| [`signing`](crates/signing/) | Off-chain transaction signing and construction |
| [`client`](crates/client/) | Typed JSON-RPC client |
| [`api-types`](crates/api-types/) | Shared RPC types (including the error catalog) |
| [`rpc-api`](crates/rpc-api/) | Shared jsonrpsee trait definitions |
| [`rpc-server`](crates/rpc-server/) | Shared server-side JSON-RPC implementation and validation |
| [`x23-transport`](crates/x23-transport/) | Shared x23 connection-pool helpers (used by gateway and node-supervisor) |
| [`node-supervisor`](crates/node-supervisor/) | `groundhog node` facade: supervises `hogd-dev` and serves JSON-RPC via `rpc-server`. Chain/submit wired to the hogd-dev scheduler, read wired to the hogd-dev indexer when advertised. |
| [`open-rpc`](crates/open-rpc/) | OpenRPC document generator: `Project` + `ModuleBuilder` thread a shared `schemars::SchemaGenerator` so named types land in `components.schemas` with `$ref` use sites |
| [`open-rpc-macros`](crates/open-rpc-macros/) | `#[open_rpc]` proc macro emitting OpenRPC method descriptors from jsonrpsee `#[rpc]` traits |
| [`open-rpc-spec`](crates/open-rpc-spec/) | Library + binary that assembles the canonical `groundhog-open-rpc-spec` artifact for downstream codegen |
| [`e2e-tests`](crates/e2e-tests/) | Process-level JSON-RPC smoke tests |
| [`bench`](crates/bench/) | Benchmark harnesses |

### Local dev setup

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The standard contributor smoke is `scripts/smoke-local-dx.sh` — runs
the user-facing CLI loop (`new` → `build` → `node` → `deploy` →
`call` → view → storage) against a real `hogd-dev` binary. Set
`HOGD_DEV_BINARY=/path/to/hogd-dev` and invoke.

For the runtime-manifest gateway/bench path, run
`scripts/smoke-runtime-manifest-bench.sh`. It starts `groundhog node`,
extracts the generated runtime manifest, runs `groundhog-bench` against the
CLI node, starts `groundhog-rpc --runtime-manifest`, and repeats the bench
checks against the standalone gateway. Set `BENCH_SCENARIO=gateway-baseline`
or `BENCH_SCENARIO=submit-ramp` for the heavier local scenario library.

The `test` subcommand requires `groundhog login` to authenticate
against the private crate registry that ships the Groundhog contract
SDK. See [`crates/cli/README.md`](crates/cli/README.md) for the full
CLI reference.

### Conventions

[`CLAUDE.md`](CLAUDE.md) is the convention reference for both human
contributors and LLM assistants. Highlights:

- Edition 2024, stable Rust, 100-column max, `thiserror` only (no
  `anyhow`), no `.unwrap()` outside tests.
- Schema ownership: XDR in `protocol-rs/groundhog-xdr` (Rust) and
  `protocol-ts` (TypeScript); JSON-RPC projections in `api-types`;
  method surface in `rpc-api`; OpenRPC artifact generated, never
  hand-authored.
- Wire-format and error-catalog conventions are summarized in the
  "Building on Groundhog" section above and detailed in CLAUDE.md
  "Gateway Conventions".
- CLI machine-readable output: typed report types in `crates/cli`,
  `data.schemaVersion` from v1 onward, `CommandResult` envelope on a
  single stdout stream.

### Design rationale

Active design notes:

- [`docs/cli/specs/`](docs/cli/specs/) — CLI and developer-tooling specs.
- [`docs/cli/roadmap.md`](docs/cli/roadmap.md) — CLI roadmap.
- [`docs/gateway/`](docs/gateway/) — gateway architecture and roadmap.

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
- `hogd` — the canonical Groundhog node. `hogd-dev` is the developer-
  loop variant this repo's `groundhog node` supervises.

See [`docs/gateway/system-design.md`](docs/gateway/system-design.md)
for the full chain-level architecture and open questions.

---

## License

Apache-2.0
