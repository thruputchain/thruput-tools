# Embedded Dev Node Backend Implementation Plan

**Goal:** Replace the current `native_exec` / `test_ffi` dev-node backend
with a supported hogd-side embedded execution library while preserving the
fast Rust CLI experience.

**Spec:** `docs/cli/specs/2026-04-24-embedded-dev-node-backend.md`

**Working principle:** Rust owns CLI, RPC presentation, lifecycle
ergonomics, and DX. hogd owns execution semantics. Codegen belongs at the
boundary, not in the semantic core.

---

## Repo Map

| Repo | Responsibility |
|------|----------------|
| `groundhog-rpc` | CLI, dev JSON-RPC surface, backend trait, current prototype bridge, future `libhogdev` binding consumer |
| `hogd` | embedded execution library, C ABI, local block driver, state backend, execution outcome model |
| SDK / native exec | safe Rust wrapper over current `test_ffi` and future `libhogdev` APIs |

---

## Phase 0: Align The Current Surface

**Purpose:** Keep the prototype useful while preventing it from becoming
the implicit long-term architecture.

**Primary repo:** `groundhog-rpc`

**Natural PR boundaries:**

1. Docs and roadmap alignment.
2. Backend trait extraction inside `crates/dev-node`.
3. Dev RPC surface stubs for features the current backend cannot support.

### Tasks

- [x] Add the embedded dev-node backend design spec.
- [x] Add this implementation plan.
- [x] Mark current `native_exec` / `test_ffi` dev-node as prototype
  infrastructure in CLI docs and help text.
- [ ] Define a `LocalNodeBackend` trait in `crates/dev-node` covering the
  current methods:
  - submit signed transaction
  - transaction receipt/detail/status
  - latest storage read
  - chain tip/block info
  - events
- [ ] Move current `TestEnvironment` usage behind a
  `NativeExecPrototypeBackend` implementation.
- [ ] Add explicit `UnsupportedByBackend` errors for planned lifecycle
  methods:
  - `snapshot`
  - `revert`
  - `mineBlock`
  - `setTime`
  - dev state mutation
- [ ] Add regression tests proving the trait-backed prototype preserves
  current behavior.

**Exit criteria:**

- Current users lose no functionality.
- The dev-node crate has a backend seam ready for the future binding.
- Unsupported dev lifecycle features fail intentionally instead of being
  absent or ambiguous.

---

## Phase 1: Specify `libhogdev`

**Purpose:** Agree on the durable lifecycle contract before writing the
hogd implementation.

**Primary repo:** `hogd`

**Natural PR boundaries:**

1. Design document in hogd.
2. Public C ABI header sketch.
3. Shared XDR/result type sketch.

### Tasks

- [ ] Write hogd-side design doc for `libhogdev` / embedded execution.
- [ ] Choose initial state backend:
  - mock in-memory only
  - HogDB/RAMDB only
  - both behind config
- [ ] Define lifecycle API:
  - create/destroy
  - reset
  - submit transaction
  - simulate transaction
  - mine/advance block
  - set/advance time
  - snapshot/revert
  - latest state reads
  - receipts/events/traces
  - dev-only state mutation
- [ ] Define threading and ownership rules:
  - one handle per chain
  - whether calls must be serialized
  - whether concurrent reads are allowed
  - whether the library is safe to call from multiple OS threads
- [ ] Define error model:
  - FFI-safe status code
  - serialized diagnostic payload
  - distinction between user tx failure, deterministic executor failure,
    backend bug, and unsupported operation
- [ ] Define canonical execution outcome payload so Rust does not infer
  receipts from traces.
- [ ] Define conformance fixture format:
  - input chain config
  - transaction XDR
  - expected status
  - expected return data/events/storage deltas

**Exit criteria:**

- Ryan/hogd owners can review the lifecycle contract as an execution API,
  not a CLI feature request.
- The Rust side can implement a binding against the contract without
  depending on test-only naming or behavior.

---

## Phase 2: Extract hogd Embedded Core

**Purpose:** Promote the useful composition in
`native_contract_test_framework` into supported library code.

**Primary repo:** `hogd`

**Natural PR boundaries:**

1. Internal library target with create/destroy/reset and current-block setup.
2. Submit/simulate transaction path.
3. Mining/block lifecycle.
4. Receipt/event/trace retention.

### Tasks

- [ ] Create a `hogdev` or `hogexec_embedded` library target.
- [ ] Extract local chain state from test framework concepts:
  - local DB wrapper
  - `DatabaseProxy`
  - `StorageSnapshotCache`
  - event aggregator
  - runtime context
  - local block driver
- [ ] Keep test framework users on the old API initially, or make them a
  thin wrapper over the new embedded core if low risk.
- [ ] Implement create/destroy/reset.
- [ ] Implement submit signed transaction with commit.
- [ ] Implement simulate signed transaction without commit.
- [ ] Implement manual mine/advance block.
- [ ] Return canonical execution outcome including:
  - tx hash
  - status
  - return data
  - emitted events
  - trace handle or serialized trace
  - storage/resource diagnostics
- [ ] Add hogd tests comparing embedded execution with existing native
  contract test framework behavior.

**Exit criteria:**

- hogd has a supported local execution core independent of `test_ffi`.
- The core can run current dev-node deploy/call flows without Rust-side
  semantic reconstruction.

---

## Phase 3: Add Durable Dev Lifecycle Features

**Purpose:** Implement the features that current `test_ffi` cannot provide
cleanly.

**Primary repo:** `hogd`

**Natural PR boundaries:**

1. Snapshots/reverts.
2. Time controls.
3. Dev-only state mutation.
4. Latest-state query expansion.

### Tasks

- [ ] Implement `snapshot` returning a dev snapshot id.
- [ ] Implement `revert(snapshot_id)`.
- [ ] Define and implement snapshot retention/invalidation rules.
- [ ] Decide whether snapshots include receipts/events/traces or only state.
- [ ] Implement block timestamp controls.
- [ ] Implement deterministic account/dev genesis setup hooks.
- [ ] Implement storage token or balance mutation hooks.
- [ ] Implement optional storage item mutation hook if needed for tests.
- [ ] Implement latest state reads for:
  - contract storage item
  - balance/storage token state
  - deployment/contract metadata
- [ ] Do not implement arbitrary historical state reads unless hogdb gains
  real backing support.

**Exit criteria:**

- Anvil-style `snapshot`, `revert`, `mine`, and `setTime` semantics are
  backed by hogd, not by ad hoc Rust-side state.
- Dev-only mutation is clearly isolated from production APIs.

---

## Phase 4: Build The Rust Binding

**Purpose:** Replace `groundhog_native_exec` test-environment usage with a
safe Rust wrapper over the new embedded API.

**Primary repos:** SDK / `groundhog-rpc`

**Natural PR boundaries:**

1. Low-level generated/bindgen or hand-written FFI bindings.
2. Safe Rust wrapper.
3. Conformance tests shared with hogd fixtures.

### Tasks

- [ ] Add `groundhog-sys` bindings for `libhogdev`.
- [ ] Add safe handle types:
  - `LocalNode`
  - `LocalNodeConfig`
  - `ExecutionOutcome`
  - `SnapshotId`
  - `LocalNodeError`
- [ ] Encode thread model in Rust types:
  - single-thread worker if required
  - `Send`/`Sync` only if the C++ contract supports it
- [ ] Map XDR payloads without reinterpreting semantic fields.
- [ ] Add wrapper APIs for all Phase 1 lifecycle methods.
- [ ] Add fixture-based conformance tests.
- [ ] Keep the old `native_exec` wrapper available until
  `groundhog-rpc` migrates.

**Exit criteria:**

- Rust consumers can use embedded execution without touching `test_ffi`
  or raw pointers.
- The binding can be swapped into `crates/dev-node` behind the backend
  trait.

---

## Phase 5: Swap The dev-node Backend

**Purpose:** Make `groundhog node` use `libhogdev` while preserving the
existing JSON-RPC surface.

**Primary repo:** `groundhog-rpc`

**Natural PR boundaries:**

1. Add `LibHogdevBackend` behind feature flag.
2. Switch default dev-node backend.
3. Remove direct `TestEnvironment` dependency from dev-node.

### Tasks

- [ ] Implement `LocalNodeBackend` for the new Rust wrapper.
- [ ] Preserve existing RPC methods and response shapes.
- [ ] Port deterministic account setup to the new lifecycle hooks.
- [ ] Port receipt/event/transaction-detail storage to canonical outcome
  payloads where possible.
- [ ] Add compatibility tests:
  - deploy
  - call
  - storage read
  - event query
  - receipt/detail/status
- [ ] Make prototype backend selectable only for transition/debugging, then
  remove it when confidence is high.

**Exit criteria:**

- `groundhog node` keeps the same user-facing behavior with the supported
  embedded backend.
- dev-node no longer depends on the SDK test environment as its primary
  execution backend.

---

## Phase 6: Expose Dev Lifecycle RPC and CLI UX

**Purpose:** Turn the backend lifecycle into explicit developer features.

**Primary repo:** `groundhog-rpc`

**Natural PR boundaries:**

1. RPC methods.
2. CLI commands/flags.
3. Docs and examples.

### Tasks

- [ ] Add dev RPC methods:
  - `groundhog_dev_snapshot`
  - `groundhog_dev_revert`
  - `groundhog_dev_mineBlock`
  - `groundhog_dev_setTime`
  - `groundhog_dev_setBalance` or storage-token equivalent
  - `groundhog_dev_reset`
- [ ] Add CLI commands or subcommands:
  - `groundhog node snapshot`
  - `groundhog node revert`
  - `groundhog node mine`
  - `groundhog node reset`
  - naming may change during CLI design review
- [ ] Add `--json` output for all lifecycle commands.
- [ ] Add human output that reports snapshot ids, block ids, and tx hashes
  without exposing raw internals unnecessarily.
- [ ] Update `docs/cli/localdev-rpc-node.md` with the new supported
  backend behavior.

**Exit criteria:**

- Developers can use snapshot/revert/mine/reset from both RPC and CLI.
- Unsupported-operation errors disappear for the supported backend.

---

## Phase 7: Parallel / Conflict Diagnostics Mode

**Purpose:** Build Groundhog-specific DX rather than only cloning Anvil.

**Primary repos:** `hogd`, `groundhog-rpc`

**Natural PR boundaries:**

1. hogd embedded batch execution mode.
2. conflict diagnostics payload.
3. RPC/CLI presentation.

### Tasks

- [ ] Add optional `--parallel` / batch execution mode.
- [ ] Drive transactions through the real batch/conflict path.
- [ ] Expose touched keys and conflicting keys.
- [ ] Expose retry/reschedule reasons where available.
- [ ] Add `groundhog_dev_explainConflict` or equivalent.
- [ ] Add tests for deterministic diagnostics on known conflict fixtures.

**Exit criteria:**

- The local node can explain Groundhog conflict behavior in a way normal
  EVM tooling cannot.
- Parallel mode remains optional and does not destabilize the default
  deterministic inner loop.

---

## Phase 8: PFN / Fork Mode

**Purpose:** Support local work against live network state.

**Primary repos:** `hogd`, `groundhog-rpc`, gateway/PFN stack

**Natural PR boundaries:**

1. Read-through state backend contract.
2. Local overlay writes.
3. CLI/RPC configuration.

### Tasks

- [ ] Define fork source interface:
  - gateway/PFN URL
  - base tip
  - latest-only vs pinned-block behavior
- [ ] Implement read-through for missing state.
- [ ] Implement local overlay writes and reset-to-fork-base.
- [ ] Cache remote reads safely.
- [ ] Clearly report unsupported historical-state requests.
- [ ] Add CLI flags:
  - `groundhog node --fork-url <url>`
  - optional `--fork-block <height>` only when backing state supports it

**Exit criteria:**

- Developers can run a local overlay on top of live network state.
- The feature does not imply arbitrary historical reads unless backed by
  real state support.

---

## Phase 9: Optional Hosted Daemon / Localnet

**Purpose:** Add heavier deployment modes once the embedded library is
stable and there is a concrete second-client need.

**Primary repos:** `hogd`, `groundhog-rpc`, ops/localnet tooling

**Natural PR boundaries:**

1. `hogdevd` daemon hosting `libhogdev`.
2. CLI process management.
3. multi-process localnet profile.

### Tasks

- [ ] Build `hogdevd` as a thin JSON-RPC/x23 host around the same embedded
  core.
- [ ] Add CLI support for spawning/attaching to `hogdevd`.
- [ ] Add log/port/lifecycle management.
- [ ] Add shared CI-node mode if needed.
- [ ] Separately define `groundhog localnet` as real-process topology:
  - hogdb
  - hogexec
  - mempool
  - coordinator-like driver
  - event/indexer services as needed

**Exit criteria:**

- Daemon mode is available without changing execution semantics.
- Localnet is clearly distinct from the fast embedded dev-node.

---

## Decision Rules

- Prefer stable execution semantics over Rust purity.
- Prefer embedded library first; daemon only after a second client exists.
- Prefer latest-state honesty over unsupported historical-state promises.
- Prefer small PRs that add one lifecycle capability at a time.
- Keep dev-only powers namespaced and isolated.
- Keep conformance tests close to every boundary.
