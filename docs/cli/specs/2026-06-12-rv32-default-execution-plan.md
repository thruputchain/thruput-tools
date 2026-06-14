# RV32 Default Execution Plan

Status: active plan

Owner: CLI / dev tooling

Goal: make RV32 the default execution artifact for build, deploy,
integration tests, and simulation while keeping WASM available as an explicit
fallback for differential debugging.

## Current State

- `groundhog build --target rv32` already compiles
  `riscv32im-unknown-none-elf`, lowers through the in-process RV32 lowerer,
  writes `target/groundhog/<contract>.rv32a`, and finalizes contract-spec
  sidecars from the ELF/artifact pair.
- Fresh scaffolds include the RV32 bin wrapper required by the RV32 build path.
- `groundhog build` still defaults to WASM.
- `groundhog deploy` accepts explicit `.wasm` and `.rv32a` artifacts through
  `--artifact`, auto-resolves by `--target`, and wraps deploy bytes as
  `Contract::kWasmi` or `Contract::kRv32` by artifact kind.
- `groundhog test --integration` defaults to WASM and accepts `--target rv32`
  to build/deploy an RV32 artifact against `hog-devnode`.
- `groundhog test --json` schema v3 reports artifact metadata under
  `artifact.kind` and `artifact.path` instead of a top-level `wasm` field.
- `hogsim` manifests and result contracts now use artifact-neutral
  kind/path/hash fields, and sim deploy branches to `Contract::kWasmi` or
  `Contract::kRv32`.
- The pinned XDR dependency exposes `Contract::kRv32`, and hogd #2640 unblocked
  local `hog-devnode` RV32 execution for gas table v3 / artifact format v4.

## Non-Goals

- Do not add compatibility shims for old CLI JSON, old `hogsim` manifests, or
  old `hog-devnode` binaries unless a real multi-version need appears.
- Do not remove WASM support. Keep `--target wasm` and explicit WASM deploys for
  differential debugging.
- Do not change the normal host WASM test harness until there is an RV32 host
  harness. The default execution artifact pivot is about deploy, integration,
  and sim execution first.

## Design Decisions

- Use artifact-neutral language in CLI and machine output.
- Replace deploy `--wasm` with `--artifact <path>`.
- Use `--target wasm|rv32` when the CLI needs to auto-resolve an artifact path.
- Infer explicit artifact kind from file extension:
  - `.wasm` -> `wasm`
  - `.rv32a` -> `rv32`
- Bump machine-readable schemas when removing WASM-specific fields.
- Pin/document the minimum `hog-devnode` protocol/runtime instead of probing
  capabilities for older runtimes.

## PR 0: Commit The Planning Docs

Scope:

- Commit the docs-only baseline before implementation churn.

Files:

- `docs/README.md`
- `docs/cli/roadmap.md`
- `docs/cli/specs/2026-06-12-rv32-default-execution-plan.md`

Acceptance:

- Main branch has a clean, durable RV32 execution plan before code changes.

## PR 1: Shared Artifact Model And RV32-Aware Deploy

Status: implemented in the current CLI branch.

Goal: make deploy understand artifact kind without changing defaults yet.

Tasks:

1. Add `ArtifactKind` and `ResolvedArtifact` in
   `crates/tools/cli/src/artifacts.rs`.
2. Add helpers to infer artifact kind from explicit paths and auto-resolve
   `target/groundhog/<package>.wasm` or `target/groundhog/<package>.rv32a`.
3. Replace deploy CLI `--wasm` with `--artifact`.
4. Add deploy `--target wasm|rv32` for auto-resolution.
5. Rename deploy internals from `wasm_*` to artifact-neutral names where they
   now apply to both kinds.
6. Branch deploy transaction construction by artifact kind:
   - `wasm` -> `Contract::kWasmi { wasmi_contract: ContractBlob { ... } }`
   - `rv32` -> `Contract::kRv32 { rv32_contract: ContractBlob { ... } }`
7. Rename deploy JSON fields:
   - `wasm_bytes` -> `artifact_bytes`
   - add `artifact_kind`
   - add `artifact_path` when the path is known

Files:

- `crates/tools/cli/src/artifacts.rs`
- `crates/tools/cli/src/main.rs`
- `crates/tools/cli/src/deploy.rs`
- `crates/tools/cli/src/error.rs`
- `crates/tools/cli/tests/cli_integration_test.rs`

Acceptance:

- `groundhog --json deploy --artifact test.wasm --dry-run` signs a WASM deploy.
- `groundhog --json deploy --artifact test.rv32a --dry-run` signs an RV32
  deploy.
- Tests prove the signed deploy transaction uses `kWasmi` for `.wasm` and
  `kRv32` for `.rv32a`.
- Auto-resolution still works for WASM with `--target wasm`.
- Auto-resolution works for RV32 with `--target rv32` after the artifact exists.
- Deploy JSON uses artifact-neutral fields.
- `cargo test -p groundhog-cli deploy_` passes.

## PR 2: Test JSON Artifact Metadata

Status: implemented in the stacked CLI branch.

Goal: make `groundhog test --json` artifact-neutral before switching
integration execution to RV32.

Tasks:

1. Add a reusable serialized artifact metadata shape to test JSON.
2. Remove or replace the top-level `wasm: PathBuf` field from `TestReport`.
3. Bump `TEST_JSON_SCHEMA_VERSION` from `2` to `3`.
4. Change `build_test_report` to accept artifact metadata instead of a WASM
   path.
5. Keep normal non-integration host tests building WASM for now, but report it
   as `artifact.kind = "wasm"`.
6. Update schema compatibility tests to assert the v3 shape.

Files:

- `crates/tools/cli/src/test_json.rs`
- `crates/tools/cli/src/test_cmd.rs`
- CLI JSON schema snapshots/tests, if generated in this repo
- `crates/tools/cli/tests/cli_integration_test.rs`

Acceptance:

- `groundhog test --json` emits `artifact` metadata.
- `groundhog test --integration --json` emits `artifact` and `integration`.
- No top-level `wasm` field remains in v3 output.
- Existing test JSON tests pass after schema update.

## PR 3: Live RV32 Runtime Proof

Status: blocked by current local `hog-devnode` RV32 deploy behavior.

Goal: prove the current pinned/minimum `hog-devnode` executes `kRv32`.

Tasks:

1. Build a fixture with `groundhog build --target rv32`.
2. Deploy the `.rv32a` artifact to local `hog-devnode`.
3. Call a known entrypoint.
4. Record the minimum `hog-devnode` binary/protocol/runtime requirement.

Acceptance:

- RV32 deploy succeeds against local `hog-devnode`.
- A call against the deployed RV32 contract returns the expected observable
  result.
- Any required protocol token/version is documented in the roadmap or
  process-contract docs.

Result on 2026-06-13:

- Built a fresh counter scaffold with `groundhog build --target rv32`.
- Started local `hog-devnode` with `--hogd-protocol hogp/7874f05decb9`.
- `groundhog --json deploy --target rv32` emitted `artifact_kind: "rv32"` and
  committed at height 2, but the receipt status was
  `deterministicError { kind: "unknown" }`; `address` and `address_source` were
  `null`.
- Rebuilt the same scaffold as WASM and verified the same node/protocol can
  deploy, call `initialize`, call `increment`, and `view get` returning `12`.
- Resolved by hogd #2640: `hog-devnode` now accepts gas table v3 / artifact v4
  RV32 artifacts and executes `kRv32` deploys successfully.

## PR 4: RV32 Integration Execution

Status: shipped in this branch.

Goal: make `groundhog test --integration` build and deploy RV32 through the
artifact model.

Tasks:

1. Change integration setup from `build_project` to the RV32 build path or a
   shared target-aware build helper.
2. Deploy the RV32 artifact via the artifact-aware deploy code.
3. Keep explicit WASM integration execution available for debugging.
4. Update help text away from "Builds the contract to WASM" where the text now
   describes integration execution.

Files:

- `crates/tools/cli/src/test_cmd.rs`
- `crates/tools/cli/src/build.rs`
- `crates/tools/cli/src/main.rs`
- `crates/tools/cli/tests/cli_integration_test.rs`
- CLI README/docs that describe `groundhog test --integration`

Acceptance:

- `groundhog --json test --integration --target rv32 --hogd-protocol ...`
  deploys RV32.
- JSON reports `artifact.kind = "rv32"`.
- Host WASM test behavior remains stable.
- Explicit WASM integration execution remains available.

## PR 5: Hogsim Artifact Schema And RV32 Deploy

Goal: intentionally break `hogsim` manifest/results from WASM-only fields to an
artifact-neutral shape.

Tasks:

1. Replace manifest `wasm` with an artifact object, for example:

   ```json
   {
     "name": "counter",
     "artifact": {
       "kind": "rv32",
       "path": "./counter.rv32a"
     }
   }
   ```

2. Replace legacy WASM-specific result path/hash fields with `artifactPath`,
   `artifactSha256`, and `artifactKind`.
3. Branch sim deploy by artifact kind:
   - `wasm` -> `Contract::kWasmi`
   - `rv32` -> `Contract::kRv32`
4. Update validation tests, event tests, compare tests, examples, fixtures, and
   sim docs in the same change.

Files:

- `crates/dev/sim/src/manifest.rs`
- `crates/dev/sim/src/contract.rs`
- `crates/dev/sim/src/result.rs`
- `crates/dev/sim/src/validation.rs`
- `crates/dev/sim/src/event.rs`
- `crates/dev/sim/src/compare.rs`
- `docs/sim/**`
- Sim example manifests and fixtures

Acceptance:

- `cargo test -p groundhog-sim` passes.
- Sim result records include artifact kind/path/hash.
- No intentional legacy WASM-specific result path/hash fields remain.
- An RV32 sim smoke deploys successfully.

## PR 6: WASM/RV32 Differential Gate

Goal: make the default flip defensible and repeatable.

Tasks:

1. Add a repeatable smoke or test fixture that builds both WASM and RV32.
2. Deploy both artifacts to local `hog-devnode`.
3. Call the same method on both deployed contracts.
4. Assert the same observable status and return data.

Acceptance:

- One documented command or automated check proves WASM and RV32 observable
  behavior match for the fixture contract.
- The check is referenced as the exit criterion for flipping the default.

## PR 7: Flip Defaults To RV32

Goal: make RV32 the default execution artifact.

Tasks:

1. Change `groundhog build` default target from WASM to RV32.
2. Make deploy auto-resolution prefer `.rv32a`.
3. Make integration execution default to RV32.
4. Update CLI help, README, roadmap, examples, and scaffold docs away from
   WASM-specific language.
5. Keep explicit `--target wasm` and explicit `.wasm` deploys.

Acceptance:

- Fresh scaffold `groundhog build` produces `.rv32a` by default.
- Fresh scaffold deploy/integration uses RV32 by default.
- Explicit `--target wasm` still works.
- The WASM/RV32 differential gate passes before the default changes land.

## Verification Matrix

Run the relevant subset for each PR, and the full matrix before PR 7 lands:

- `cargo fmt --check`
- `cargo test -p groundhog-cli`
- `cargo test -p groundhog-sim`
- `groundhog build --target wasm`
- `groundhog build --target rv32`
- `groundhog --json deploy --artifact target/groundhog/<name>.rv32a --dry-run`
- Live `hog-devnode` RV32 deploy/call smoke
- `groundhog --json test --integration --target rv32 --hogd-protocol <token>`
- `hogsim` RV32 scenario smoke

## Open Follow-Ups

- Record the exact released `hog-devnode` binary/protocol/runtime version that
  includes hogd #2640 once it is cut.
- Decide whether the differential gate lives as an automated test, a smoke
  script, or a documented release checklist command.
- Update bootstrap/runtime docs that still reference `dev-wallet.wasm` once the
  runtime artifact naming is settled.
