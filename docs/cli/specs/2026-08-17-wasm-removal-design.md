# WASM Removal Design

Status: active

Owner: CLI / dev tooling

Goal: remove WASM from the Groundhog off-chain tooling workspace, leaving RV32
as the only contract artifact this repo builds, deploys, tests, simulates, or
documents.

## Background

RV32 became the default execution artifact in the
[RV32 default execution plan](../../archive/cli/2026-06-12-rv32-default-execution-plan.md).
That plan deliberately kept WASM as an explicit fallback for differential
debugging: `--target wasm` on build/test/deploy, a `--differential` gate, a
`test_wasm.rs` scaffold target, and WASM-shaped branches through the artifact,
spec, and deploy paths.

Groundhog is removing WASM from the protocol and the node. The hogd-side WASM
removal is already in flight as hogd PR #3465. This repo no longer needs a
second, fully-wired artifact path, so this is a removal rather than a
migration: nothing here has to gain RV32 capability first except
`build --bindings`.

## Non-Goals

- Do not remove `Contract::kWasmi` from `groundhog-xdr`. That is protocol-owned.
  This repo only stops constructing it.
- Do not rename `SyscallErrorCode::kSyscallErrorCodeDeterministicWasm` in
  `crates/json-rpc/json-rpc-types/src/dev.rs`. That string mirrors a hogd error
  code name and changes when hogd renames it.
- Do not edit anything under `docs/archive/`. Archived plans are history and
  should keep describing what shipped at the time.
- Do not add deprecation shims. `clap` already rejects an unknown `--target`
  value with a clear message, and the workspace has no external consumers locked
  into the current CLI surface.

## Design Decisions

### `ArtifactKind` survives as a single-variant enum

`ArtifactKind` loses its `Wasm` variant but stays as a type. It keeps appearing
in build, deploy, and test JSON payloads, in `hogsim` manifests
(`artifact: {kind, path}`) and results (`artifactKind`), and in testkit's
`deploy(bytes, kind)` signature. It names the protocol `Contract` union variant
the bytes will be wrapped in, so it stays factual rather than
reserved-for-future speculation.

### `--target` and `BuildTarget` are deleted

With one artifact kind, target *selection* is dead weight even though the
serialized *kind* is not. `BuildTarget`, `BuildTargetArg`, and the `--target`
flag on `build`, `test`, and `deploy` all go. This is the one place the
abstraction is split rather than kept or collapsed wholesale.

### `--bindings ts` ports to the RV32 path; `--optimize` is deleted

`build --bindings` and `build --optimize` are both WASM-only today
(`crates/tools/cli/src/build.rs:96-105` rejects each for RV32). `--optimize`
exists only to run `wasm-opt` and has no meaning without it — the RV32 lowerer
is its analog and always runs. `--bindings` is a real capability, and the RV32
build already writes a `.spec.xdr` sidecar that `codegen.rs` can generate TS
from, so it ports with a small amount of wiring.

### `--differential` is deleted outright

`groundhog test --integration --differential` is currently the documented
release-checklist gate. Its whole function is comparing WASM and RV32 observable
behavior, which has no meaning with one artifact kind. `groundhog test
--integration` replaces it as the release-checklist exit criterion.

### Spec file inputs become `.xdr`-only, with a targeted `.rv32a` error

`.rv32a` carries no embedded spec section — the RV32 build extracts spec
fragments from the ELF and writes a `.spec.xdr` sidecar. So `call --spec`,
`events --spec`, and `codegen` accept `.xdr` only once `.wasm` is gone. Passing
`.rv32a` is the mistake users will actually make, so it gets its own error
pointing at the sidecar next to it rather than a generic "unsupported
extension".

## Audit

The complete WASM surface in this workspace, which is what the slices below
consume.

### CLI build path

| Location | Surface |
| --- | --- |
| `crates/tools/cli/src/build.rs` | `WASM_TARGET_TRIPLE`, `BuildTarget::Wasm`, `BuildResult.wasm`, `build_project()`, `wasm_build_command()`, the `wasm-opt` block, WASM build JSON payload |
| `crates/tools/cli/src/wasm_instrument.rs` | Entire module: `groundhog-wasm-meter` stack limiting plus verify. No RV32 analog; RV32 sets stack size at link time via `--stack-size` |
| `crates/tools/cli/src/artifacts.rs` | `ArtifactKind::Wasm`, `cargo_wasm_output_path`, `groundhog_wasm_output_path`, `.wasm` extension inference, JSON schema `enum: ["wasm", "rv32"]` |
| `crates/tools/cli/src/contract_spec.rs` | `rewrite_wasm_with_spec` extraction, `finalize_spec()`, the `.wasm` arm of `load_contract_spec_from_file`, `load_registered_interfaces()`, and the WASM-vs-RV32 modification-time arbitration in `load_built_contract_spec()` |
| `crates/tools/cli/src/main.rs` | `BuildTargetArg::Wasm`, `--target` on build/test/deploy, `--differential`, `--optimize`, command help text |
| `crates/tools/cli/src/error.rs` | `WasmOptNotFound`, `WasmNotFound`, `WasmOptFailed`, `WasmInstrumentFailed`, `WasmVerifyFailed`, plus their `code()` and `hint()` arms |
| `crates/tools/cli/src/checks.rs` | `wasm_target`, `wasm_opt`, the `WasmTargetStatus` alias, and the WASM leg of `detect_rust_environment()` |
| `crates/tools/cli/src/init.rs` | Four doctor/init messages about the WASM target and `wasm-opt` |
| `crates/tools/cli/src/deploy.rs` | `ArtifactKind::Wasm` to `Contract::kWasmi` arm |
| `crates/tools/cli/src/event_spec.rs`, `src/codegen_cmd.rs`, `src/call.rs` | `.wasm` accepted as a trusted contract-spec source |

### Scaffolding and templates

`templates/counter/test_wasm.rs.tmpl`, `templates/stub/test_wasm.rs.tmpl`,
`scaffold.rs` (two `include_str!` constants, the five-tuple template match, the
`tests/test_wasm.rs` write), `templates/config.toml.tmpl`
(`[target.wasm32-unknown-unknown]`), `templates/rust-toolchain.toml.tmpl`
(`wasm32-unknown-unknown` in `targets`), `templates/Cargo.toml.tmpl`
(`cfg(not(any(target_arch = "wasm32", ...)))` and a feature comment),
`templates/contract.toml.tmpl` (comment).

`test_wasm.rs.tmpl` calls the SDK's `load_wasm_contract`. Deleting the template
is in-repo; the SDK function's fate belongs to the SDK repo.

### Test harness and machine-readable schemas

- `test_cmd.rs`: `WASM_TEST_TARGET`, the `--differential` machinery
  (`IntegrationDifferentialReport`, the differential runner, its tests), and two
  `BuildTarget::Wasm` build arms.
- `test_json.rs`: `TestMode::Wasm` is the current serde `#[default]`;
  `TestArtifactInfo.kind`; `TEST_JSON_SCHEMA_VERSION` is `4`.
- Doctor environment JSON: `wasmTarget` and `wasmOpt`. This payload carries no
  `schemaVersion` today, so there is no version to bump.

### Simulator

`crates/dev/sim/src/manifest.rs` (`ContractArtifactKind::Wasm`),
`contract.rs` and `provision.rs` (`kWasmi` arms; `provision.rs:646`'s
`_ => ContractArtifactKind::Wasm` extension default), and `.wasm` fixtures
across `validation.rs`, `executor.rs`, `event.rs`, `compare.rs`, `run.rs`,
`catalog.rs`, and `main.rs`. Plus `docs/sim/README.md`,
`docs/sim/sim-design.md`, and example manifests.

### Testkit

`crates/dev/testkit/src/artifact.rs` (`ArtifactKind::Wasm`, `kWasmi` arm,
`.wasm` inference), `src/error.rs` ("expected .wasm or .rv32a"),
`src/scenario.rs` doc comment, `tests/trivial_scenario.rs`,
`docs/testkit/DESIGN.md`.

### hogd-owned dev-wallet artifact

`crates/tools/signing/src/dev_wallet.rs:34` infers the kind of an artifact
*hogd* produced, defaulting to `Contract::kWasmi` for any non-`.rv32a`
extension. Thirty `dev-wallet.wasm` fixture references across 18 files mirror
what hogd currently emits in its runtime manifest: `crates/dev/bench`,
`crates/dev/node-supervisor` (including `fake_hogd_dev.rs`), `crates/dev/sim`
(five files), `crates/json-rpc/json-rpc-gateway/src/config.rs`,
`crates/json-rpc/json-rpc-types` (`bootstrap_manifest.rs`,
`runtime_manifest.rs`), and `crates/tools/cli` (`wallet.rs`,
`account_resolution.rs`, `network_descriptor.rs`, `node.rs`, plus two test
files).

### CI and docs

`ci.yml` has three `rustup target add wasm32-unknown-unknown` steps plus a
`binaryen` install; `publish.yml`, `publish_docs.yml`, and
`publish_dry_run.yml` have one target-add step each. The clippy and test jobs'
wasm32 installs appear vestigial — the workspace itself is host-only — and
the `binaryen` install exists only so the scaffold's `groundhog test` can run
`wasm-opt`. Docs: `README.md`, `crates/tools/cli/README.md`,
`crates/tools/cli/assets/groundhog/SKILL.md`, `docs/cli/roadmap.md`,
`docs/testkit/DESIGN.md`,
`CLAUDE.md`, `typos.toml`, `crates/rv32/lower/README.md` and
`crates/rv32/lower/Cargo.toml` (both describe the lowerer as "the wasm-opt
analog", which stays useful as an explanation only while readers remember
wasm-opt).

Two living CLI specs also carry WASM contracts, not just mentions.
`docs/cli/specs/2026-04-14-bundled-wasm-opt-design.md` is a deferred-decision
doc about bundling `wasm-opt` that becomes entirely moot when `--optimize` goes,
and it has an open tracking issue.
`docs/cli/specs/2026-07-27-event-and-token-transfer-inspection-design.md`
specifies `--spec` loading from `.wasm` and `.xdr` in roughly fourteen places,
including the documented help text, so it is the design source for the
`event_spec.rs` change rather than a docs-sweep item.
`docs/cli/specs/2026-05-07-transaction-detail-options.md` uses WASM only as
protocol and competitive context.

### Cross-repo boundaries

- `Contract::kWasmi` is defined in `groundhog-xdr`. The four construction sites
  in this repo (`deploy.rs`, `sim/contract.rs`, `sim/provision.rs`,
  `testkit/artifact.rs`) are **not** blocked — they fall out mechanically when
  `ArtifactKind` loses its `Wasm` variant. Removing them here before the
  protocol drops `kWasmi` also avoids a window where this workspace stops
  compiling.
- `signing/dev_wallet.rs` **is** blocked, because it classifies a foreign
  artifact rather than one we built. It follows hogd PR #3465.
- The SDK owns `load_wasm_contract`.
- `groundhog-wasm-meter` becomes fully removable from the workspace.
  `groundhog-spec-tool` stays: the RV32 path uses `parse_elf`,
  `canonicalize_spec`, `check_raw_spec_format`, `parse_raw_fragments`, and
  `render_spec_json`.

## Slices

The slices are independently verifiable, with one implementation constraint:
slices 1 and 3 land together because the CLI test harness consumes the
`BuildTarget` and WASM build helpers removed by slice 1. They remain separate
tracking slices because they describe distinct public contracts. Slice 6 is the
only slice that depends on another repo.

### Slice 1: CLI build path

Scope:

- Delete `crates/tools/cli/src/wasm_instrument.rs` and its `pub mod` line in
  `lib.rs`. Drop `groundhog-wasm-meter` from the CLI and workspace manifests.
- In `build.rs`: delete `WASM_TARGET_TRIPLE`, `BuildTarget`, `build_project()`,
  `wasm_build_command()`, the `wasm-opt` invocation, and the WASM `BuildResult`.
  Rename `Rv32BuildResult` to `BuildResult` and type its `target` field as
  `ArtifactKind`.
- Wire `--bindings` into the RV32 build: after `finalize_rv32_spec()`, generate
  TS from the returned spec, writing alongside the `.rv32a` artifact the same
  way the WASM path wrote alongside the `.wasm`.
- In `artifacts.rs`: drop `ArtifactKind::Wasm`, `cargo_wasm_output_path()`,
  `groundhog_wasm_output_path()`, and the `.wasm` inference arm. The JSON schema
  enum becomes `["rv32"]`.
- In `contract_spec.rs`: delete `load_embedded_contract_spec()`,
  `load_embedded_contract_spec_from_bytes()`, `finalize_spec()`,
  `load_contract_spec()`, and `load_registered_interfaces()` (unused outside the
  module once the WASM path is gone). Collapse `load_built_contract_spec()` to a
  single check: the `.spec.xdr` sidecar next to `<package>.rv32a` is current
  when it is at least as new as the artifact.
- In `load_contract_spec_from_file()`: keep `.xdr`, drop `.wasm`, and add a
  `.rv32a` arm whose error names the `.spec.xdr` sidecar.
- In `error.rs`: remove the five WASM variants with their `code()` and `hint()`
  arms; update remaining `.wasm` text in `UnsupportedArtifactExtension` and the
  event-spec hint.
- In `checks.rs`: remove `wasm_target`, `wasm_opt`, and the `WasmTargetStatus`
  alias; `detect_rust_environment()` returns a two-tuple.
- In `init.rs`: remove the four WASM/`wasm-opt` messages.
- In `event_spec.rs`: drop the `.wasm` dispatch arm and reword the unsupported
  extension error.
- In `main.rs`: remove `BuildTargetArg`, `--target` on build/test/deploy, and
  `--optimize`; rewrite the `build` `long_about`; update the two `--spec`
  argument help strings that advertise `.wasm`.
- In `call.rs`: replace the `wasm_with_embedded_spec` /
  `wasm_with_custom_section` test fixtures with `.spec.xdr` sidecar fixtures, so
  spec-discovery tests exercise the path that still exists.
- Delete the WASM build-path tests in `tests/build_test.rs`: the
  `[target.wasm32-unknown-unknown]` config fixture, the processed-WASM-path
  assertions, and the `--lib` scoping tests that exist only because a WASM
  `cdylib` and an RV32 bin wrapper collide on output filename. Also delete the
  build and deploy WASM assertions in `tests/cli_integration_test.rs`.
- Update [the event and token transfer inspection
  design](2026-07-27-event-and-token-transfer-inspection-design.md), which is a
  living doc specifying `--spec` loading from `.wasm` and `.xdr` in roughly
  fourteen places, including the documented `--spec` help text. It describes
  shipped behavior, so it changes with the behavior rather than in the docs
  sweep.

Acceptance:

- `groundhog build` in a fresh scaffold produces `.rv32a` plus `.spec.json` and
  `.spec.xdr` sidecars.
- `groundhog build --bindings ts` writes TS bindings next to the `.rv32a`.
- `groundhog build --target wasm` and `--optimize` fail as unknown arguments.
- `groundhog codegen ts <artifact>.rv32a` errors pointing at the sidecar.
- No `wasm` identifier remains in `crates/tools/cli/src/build.rs`,
  `artifacts.rs`, `contract_spec.rs`, `error.rs`, `checks.rs`, `init.rs`,
  `event_spec.rs`, or `call.rs`.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean, which also
  proves nothing else referenced the deleted helpers.

### Slice 2: Scaffolding and templates

Scope:

- Delete `templates/counter/test_wasm.rs.tmpl` and
  `templates/stub/test_wasm.rs.tmpl`, their `include_str!` constants, and the
  `tests/test_wasm.rs` write in `scaffold.rs`. The template tuple goes from five
  entries to four.
- Remove `[target.wasm32-unknown-unknown]` from `config.toml.tmpl`,
  `wasm32-unknown-unknown` from `rust-toolchain.toml.tmpl`'s `targets`, the
  `target_arch = "wasm32"` cfg predicate and WASM feature comment from
  `Cargo.toml.tmpl`, and the WASM mention in `contract.toml.tmpl`.
- Update `scaffold_test.rs` expectations.

Acceptance:

- `groundhog new my-counter` produces `tests/test.rs`, `tests/test_rv32.rs`, and
  `tests/integration.rs`, and no `tests/test_wasm.rs`.
- The scaffold's `rust-toolchain.toml` lists only
  `riscv32im-unknown-none-elf`.
- `cargo test` and `groundhog test` both pass in the fresh scaffold on a machine
  with no `wasm32-unknown-unknown` target installed.

### Slice 3: Test harness and machine-readable schemas

This is the consumer-visible break; it lands as one change so downstream
tooling sees a single schema transition.

Scope:

- Remove `--differential`: the flag in `main.rs`,
  `IntegrationDifferentialReport`, the differential runner in `test_cmd.rs`,
  and their tests.
- Remove `WASM_TEST_TARGET` and both `BuildTarget::Wasm` build arms in
  `test_cmd.rs`, along with the WASM-shaped fixtures in its test module and in
  `tests/test_cmd_test.rs` (which constructs `BuildTarget::Wasm` directly).
- In `test_json.rs`: `TestMode` becomes `Rv32 | Integration` with
  `#[default] Rv32`, and `TEST_JSON_SCHEMA_VERSION` goes `4` to `5`.
- Remove `wasmTarget` and `wasmOpt` from the doctor environment payload.

Acceptance:

- `groundhog test --json` emits `schemaVersion: 5`, `mode: "rv32"`, and
  `artifact.kind: "rv32"`.
- `groundhog test --integration --differential` fails as an unknown argument.
- The doctor payload has no `wasmTarget` or `wasmOpt` field.
- The PR body calls out both breaks explicitly, including that `mode`'s serde
  default changes meaning for any consumer that omits it, and that the doctor
  payload has no version field to signal the change.

### Slice 4: Simulator

Scope:

- Drop `ContractArtifactKind::Wasm` from `manifest.rs` and the `kWasmi` arms in
  `contract.rs` and `provision.rs`.
- `provision.rs`'s extension-inference default becomes RV32-or-error. Sequence
  this with slice 6 — the artifact it classifies is hogd's dev wallet.
- Sweep `.wasm` fixtures in `validation.rs`, `executor.rs`, `event.rs`,
  `compare.rs`, `run.rs`, `catalog.rs`, and `main.rs`.
- Bump `result.json` schema version from 3 to 4 because
  `contracts[].artifactKind` no longer accepts `wasm`.
- Update example manifests, `docs/sim/README.md`, and
  `docs/sim/sim-design.md` (including the `test_cmd.rs` description at line 159
  and the WASM-driven workload hook note at line 430).

Acceptance:

- `cargo test -p groundhog-sim` passes.
- `hogsim validate` accepts a manifest with `artifact: {kind: "rv32", path}` and
  rejects `kind: "wasm"` as an unknown variant.
- `scripts/smoke-sim-local.sh` passes against a real `hog-devnode`.

### Slice 5: Testkit

Scope:

- Drop `ArtifactKind::Wasm`, the `kWasmi` arm, and `.wasm` inference from
  `artifact.rs`; update the `error.rs` message and the `scenario.rs` doc
  comment.
- Update `tests/trivial_scenario.rs` and `docs/testkit/DESIGN.md`.

Acceptance:

- `cargo test -p groundhog-testkit` passes.
- `scripts/smoke-testkit-local.sh` passes against a real `hog-devnode` with a
  `caller-counter` fixture.

### Slice 6: hogd dev-wallet artifact

Sequenced behind hogd PR #3465, since it encodes what hogd emits rather than
what this repo builds.

Scope:

- `signing/dev_wallet.rs`: replace the `_ => Contract::kWasmi` fallback with
  explicit RV32 classification, erroring on anything else instead of silently
  wrapping unknown bytes as a WASM contract.
- Update the 30 `dev-wallet.wasm` fixture references across the 18 files listed
  in the audit to whatever hogd's WASM-removal PR emits.

Acceptance:

- The hogd runtime manifest's `artifactHint` extension and this repo's fixtures
  agree.
- An unrecognized dev-wallet artifact extension produces a typed error naming
  the offending path, not a `kWasmi` deploy.
- `scripts/smoke-local-dx.sh` passes against a `hog-devnode` built from the
  post-removal hogd.

### Slice 7: CI and docs

Scope:

- Remove three `rustup target add wasm32-unknown-unknown` steps and the
  `binaryen` install from `ci.yml`, and one target-add step each from
  `publish.yml`, `publish_docs.yml`, and `publish_dry_run.yml`.
- Update `README.md`, `crates/tools/cli/README.md`,
  `crates/tools/cli/assets/groundhog/SKILL.md`,
  `docs/cli/roadmap.md`, `CLAUDE.md`, and `typos.toml` (whose `"*.wasm"` exclude
  becomes dead config).
- Delete `docs/cli/specs/2026-04-14-bundled-wasm-opt-design.md` and its
  `CLAUDE.md` index entry. It is a living deferred-decision doc about replacing
  the external `wasm-opt` subprocess with the Rust `wasm-opt` crate, and
  `--optimize` no longer exists. Close its tracking issue
  (`groundhogchain/groundhog-rpc#68`) as obsolete rather than leaving an open
  issue pointing at a deleted design.
- In `docs/cli/specs/2026-05-07-transaction-detail-options.md`, reword
  "WASM-visible behavior" to "contract-visible behavior". The `NEAR (WASM L1)`
  comparison row is a fact about NEAR and stays.
- Replace the release-checklist exit criterion: `groundhog test --integration
  --differential` becomes `groundhog test --integration` wherever it is
  documented as the gate.
- Reword `crates/rv32/lower/README.md` and `crates/rv32/lower/Cargo.toml` so the
  lowerer is described on its own terms rather than as "the wasm-opt analog".

Acceptance:

- CI is green with no WASM target or `binaryen` install. If the clippy or test
  job fails without the wasm32 target, that assumption was wrong and the step
  goes back with a comment explaining what needs it.
- `rg -i wasm` across the repo returns only: this removal design,
  `docs/archive/**`, intentional regression tests that reject the removed
  inputs, protocol-owned `Contract::kWasmi` / hogd syscall identifiers, the
  factual NEAR comparison row, and `Cargo.lock` / `pnpm-lock.yaml` transitive
  entries.
- No living doc describes a WASM build, test, or deploy path.

## Ordering

Slices 1 and 3 form one implementation and review gate. Slice 2 follows because
slice 1's `checks.rs` change feeds its scaffold expectations. Slices 4 and 5 are
independent of each other and of 1 through 3, so they can run in parallel. Slice
7 lands last so the docs describe the finished state. Slice 6 lands after hogd
PR #3465 merges; if it lands before slice 4, run it first so the simulator's
extension-inference default only changes once.

## Verification

Per slice:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Before slice 7 closes, run the local smokes against a real `hog-devnode`:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode scripts/smoke-local-dx.sh
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode scripts/smoke-sim-local.sh
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  GROUNDHOG_EXAMPLES_ROOT=/path/to/groundhog-examples \
  scripts/smoke-testkit-local.sh
```

## Resolved Questions

- Workspace clippy and test jobs do not need the removed contract target; the
  CI and publish setup was deleted in slice 7.
- The fresh scaffold's RV32 host-test harness still builds and runs after the
  template cleanup, so no SDK compatibility shim was needed in slice 2.
