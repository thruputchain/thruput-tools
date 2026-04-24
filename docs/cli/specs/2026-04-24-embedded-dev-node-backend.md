# Embedded Dev Node Backend

**Date:** 2026-04-24

## Decision

The durable Groundhog dev-node backend should be a hogd-side embedded
execution library consumed by the Rust CLI through a stable FFI boundary.

The current `native_exec` / `test_ffi` backend remains useful for DX
prototyping, but it is not the long-term backend contract. The problem is
not execution fidelity. The current path reaches the real hogd executor.
The problem is that `test_ffi` was designed for short-lived contract test
lifecycles, while `groundhog node` is using it as a long-lived Anvil-style
node.

Short version:

> We are not abusing the executor. We are abusing the test harness
> lifecycle contract.

## Goals

- Preserve production execution semantics in local development.
- Keep the fast single-binary Rust CLI experience for the first durable
  version.
- Make local-node lifecycle features explicit: reset, mine, time travel,
  snapshots, reverts, receipts, events, traces, and dev-only state
  mutation.
- Avoid a pure Rust semantic clone of hogexec unless Groundhog later makes
  a product-level decision to port execution semantics to Rust.
- Leave room for a future hosted `hogdevd` daemon and heavier multi-process
  localnet mode without requiring either on day one.

## Non-Goals

- Do not implement a second executor in Rust for dev-node only.
- Do not expose arbitrary historical state reads until hogd can actually
  serve them.
- Do not make consensus, gossip, catchup, production mempool policy, or
  validator registration part of the fast local dev-node.
- Do not add dev-only shortcuts to production node paths in a way that can
  leak into real deployments.

## Current Backend

`groundhog node` currently runs a Rust JSON-RPC server backed by
`groundhog_native_exec`.

The current shape is:

```text
groundhog-rpc dev-node
  -> SDK native_exec TestEnvironment
  -> hogd test_ffi shared library
  -> real hogexec runtime/syscall machinery
```

The upside is high fidelity for normal transaction execution. The dev-node
submits signed transactions through the same native executor path used by
SDK tests. In this repo, the relevant ownership point is
`crates/dev-node/src/backend.rs`, where `DevBackend` owns a
`TestEnvironmentHandle` and drives `TransactionHandler`.

The downside is that the FFI aperture is test-shaped:

- SDK `TestEnvironmentHandle` is `Rc<TestEnvironment>`, with raw FFI
  handles underneath. The current dev-node pins it to one worker thread and
  bridges RPC calls over channels.
- hogd `test_ffi/test_bindings.h` documents that sibling-block view
  isolation is not implemented; setup helpers write directly to whatever
  DB state is current.
- hogd `hogdb/rpc_from_exec.cc` currently ignores the `BlockID` passed to
  `get_snapshot_item` because hogdb can only read latest snapshot state.
- teardown is test-style: dropping the environment frees the underlying DB.

Those are hard ceilings for durable Anvil-style features. `snapshot`,
`revert`, time travel, long-lived state, and any public "read state at block
N" API need hogd-side lifecycle support anyway.

## Architecture Principle

Reuse production semantics, not production topology.

Production topology includes distributed services: coordinator, scheduler,
mempool, executor processes, hogdb, event/indexer services, gossip/catchup,
and consensus/finality. Local dev does not need that topology for the inner
loop.

The embedded dev backend should keep:

- transaction decoding and auth verification
- WASM runtime and syscall behavior
- storage deltas and commit behavior
- replay cache / uniquifier semantics
- event emission
- gas/resource accounting
- conflict behavior when explicitly running batch/parallel mode

The embedded dev backend should replace:

- consensus and gossip
- network catchup
- production mempool admission policy
- validator registration
- production finality and retention policies

## Target Shape

Start as an embedded library, not a daemon-first design:

```text
groundhog CLI / dev RPC
  -> Rust local-node wrapper
  -> libhogdev FFI boundary
  -> hogd embedded execution core
  -> real SystemCallHandler + RuntimeContext + DatabaseProxy + hogdb-ish state
```

The hosted daemon can come later:

```text
groundhog CLI / dashboards / CI workers
  -> JSON-RPC or x23
  -> hogdevd
  -> same libhogdev core
```

The heavier localnet mode is separate:

```text
groundhog localnet
  -> real hogdb + hogexec + mempool + coordinator-like driver + event/indexer
```

## hogd Extraction Seed

The closest existing hogd precedent is
`hogexec/runtime/native_contract_test_framework.h`.

That framework already composes many pieces the dev backend needs:

- `SystemCallHandler`
- `RuntimeContext`
- `StorageSnapshotCache`
- `DatabaseProxy`
- mock in-memory DB or HogDB-backed DB wrapper
- event aggregation wrappers
- block open/advance/discard helpers

Important nuance: this is not coordinator-free. It uses thin scheduler and
coordinator-facing wrappers to drive DB snapshots and events. That is fine.
The extraction target should be a supported local block driver around these
concepts, not a copy of the production distributed coordinator topology.

## Components

### Rust CLI / RPC Layer

Responsibilities:

- own CLI commands and human/machine output
- expose the dev JSON-RPC surface
- manage the worker thread or async boundary needed by the FFI wrapper
- translate backend errors into RPC errors
- preserve a single-binary install path for `groundhog node`

This layer should not own execution semantics.

### SDK Local Node Wrapper

Responsibilities:

- expose an ergonomic Rust API over the embedded C ABI
- hide thread-pinning details if the C++ side is not `Send + Sync`
- own safe handle types
- map serialized XDR inputs/outputs
- provide a backend trait that the current `test_ffi` bridge and future
  `libhogdev` backend can both implement during migration

### libhogdev C ABI

Responsibilities:

- stable lifecycle: create, destroy, reset
- explicit ownership and threading contract
- typed result/error model
- transaction execution and simulation calls
- mining/block controls
- snapshot/revert controls
- state query and dev mutation calls
- event/receipt/trace retrieval

This should be separate from `test_ffi`, even if the first implementation
reuses much of the same internal code.

### hogd Embedded Execution Core

Responsibilities:

- compose real hogexec runtime pieces
- provide local block driver semantics
- support in-memory and HogDB-backed state modes
- expose execution outcomes as canonical data, not ad hoc Rust
  reconstruction from traces
- retain enough receipts/events/traces for local queries

## Lifecycle Contract

The embedded library should define the following groups before
implementation starts.

### Chain Lifecycle

- create ephemeral chain
- create chain from config/genesis
- reset chain to genesis/dev state
- destroy chain
- optional save/load state, if the DB backend supports it

### Transaction Execution

- submit signed transaction and commit according to current mining mode
- simulate signed transaction without committing
- call/read-only invocation if supported by the runtime
- return tx hash, status, return data, events, gas/resources, traces, and
  storage diagnostics

### Mining and Time

- instant mine on submit
- manual mine block
- interval mining later, if useful
- set or advance block timestamp
- read current block height, block id/hash, and timestamp

### State Query

- read latest storage item
- read latest balance/auth/deployment data where supported
- read receipt by tx hash
- read transaction detail by tx hash
- filter events by contract/topic/range

Do not expose arbitrary block-height state reads until hogdb supports them.
Internal snapshot ids are acceptable for dev `snapshot` / `revert`.

### Snapshots and Reverts

- create snapshot id
- revert to snapshot id
- discard snapshot id
- define whether snapshots include receipts/events/traces, not only DB state
- define snapshot invalidation rules after revert

### Dev-Only Mutation

- set/free storage tokens or balance
- write storage item, if needed for test setup
- deploy/register contract artifact for dev setup
- reset auth/replay state only if explicitly marked as a cheat

Every mutation method should be namespaced as dev-only and unavailable on
production gateway/PFN surfaces.

### Diagnostics

- retained execution trace
- emitted events
- storage delta summary
- touched storage keys
- conflict/retry explanation in future parallel mode

## State Model

The initial embedded backend should be latest-state first. Snapshots and
reverts are local dev constructs. Public "read at block height" semantics
should wait until Groundhog decides whether historical state lives in hogdb,
an archiver, or a PFN/indexer-adjacent service.

This aligns with the current gateway direction: expose latest state now,
avoid promising arbitrary historical state until the backing state layer can
serve it.

## Execution Modes

### Default: Deterministic Single-Threaded Dev

This is the Anvil-like inner loop:

- instant commit
- deterministic accounts
- manual reset/snapshot/revert
- predictable traces and receipts
- no production mempool or consensus

### Later: Parallel / Batch Mode

Groundhog's conflict model is a differentiator. Add a later mode that runs
transactions through the real production batch/conflict path and surfaces
diagnostics:

- touched keys
- conflicting keys
- transaction retry/reschedule reason
- nondeterministic proposal data, where useful

This should be a mode on top of a working canonical local backend, not the
foundation.

### Later: PFN / Fork Mode

Fork mode should read missing state from a live gateway/PFN and apply local
overlay writes, similar in spirit to Anvil fork mode. This should wait until
the local state backend and query surface are stable.

## External References

- Foundry Anvil: fast local Ethereum node with snapshots, mining controls,
  fork mode, and dev RPC methods.
  https://www.getfoundry.sh/anvil
- Hardhat EDR: reusable Rust Ethereum development runtime, separated from
  the Hardhat CLI.
  https://github.com/NomicFoundation/edr
- Soroban: production host is a reusable Rust library used for local SDK
  testing and simulation.
- Solana local tooling: ships real local runtime/validator-style tooling,
  which is useful as a localnet reference but heavier than the initial
  embedded Groundhog path.

## Open Questions

1. Should the first `libhogdev` state backend use mock in-memory DB, HogDB
   over RAMDB, or both behind a config flag?
2. What is the minimum stable execution outcome type hogd should expose so
   Rust does not reconstruct receipts from implementation-specific traces?
3. Should dev snapshots include only state, or state plus RPC-retained
   receipts/events/traces?
4. Which dev mutation methods are essential for the first release?
5. Where should conformance tests live so SDK tests, dev-node tests, and
   hogd embedded tests share expected outcomes?
6. When does a hosted `hogdevd` daemon become necessary? Proposed answer:
   when a second client appears, such as a dashboard, shared CI node, or
   parallel test runner.

