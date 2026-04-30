# Hogd-Owned Hybrid Dev Node

**Date:** 2026-04-25
**Status:** Design proposal, not a decision.

**Companion to:**
- [`plans/2026-04-29-local-node-hogd-dev-supervisor.md`](plans/2026-04-29-local-node-hogd-dev-supervisor.md)
  — focused Rust local-node supervisor implementation plan
- [`specs/2026-04-29-hogd-dev-process-contract.md`](specs/2026-04-29-hogd-dev-process-contract.md)
  — draft hogd-dev subprocess contract proposal
- [`specs/2026-04-29-node-supervisor-x23-client-boundary.md`](specs/2026-04-29-node-supervisor-x23-client-boundary.md)
  — x23 client wiring decision (deliberately deferred adopting
  `chuckd_scheduler::SchedulerClient` as a unifying aperture)

This doc supersedes earlier proposals that argued for an embedded
`libhogdev` FFI backend (the durable target named here is a hogd-owned
subprocess) and the original `native_exec` / `test_ffi` Level B prototype
(now legacy / kept only until hogd-dev covers the development loop).

## Short version

The durable local dev-node shape should probably be neither:

- Rust dev-node directly owning `native_exec` / `test_ffi`, nor
- Rust CLI supervising a whole multi-process hogd stack forever, nor
- Rust embedding hogd through a stable C ABI on day one.

The more attractive target is a **hogd-owned single-process local dev
backend**, extracted from the same wiring idea as
`hogd/hogd/mempool/test/infra.h::MempoolTestInfra`, and driven by the Rust
tooling over x23.

```text
groundhog node
  ├─ starts one hogd-dev subprocess
  │    └─ mempool + scheduler + executor + auth + hogdb/RAMDB + event/indexer state
  └─ starts Rust JSON-RPC server
       ├─ GroundhogApi, same as production
       └─ groundhog_dev_* methods
```

The Rust side keeps the public JSON-RPC surface. The hogd side owns local
execution, mempool, auth/replay, HogDB lifecycle, event/indexer read state,
and dev-only state controls.

The idea here is a process boundary, not an FFI boundary. It preserves crash
isolation and avoids a C ABI while still giving users a single `groundhog node`
command without havingt to be a full multi-process supervisor. In theory, this
might be the sweet spot?

The "single process" claim is about the hogd-dev packaging boundary that Rust
supervises. Internally, hogd-dev still contains multiple hogd components with
their own lifecycle and x23 wiring. The complexity is not eliminated; it is
owned and hidden behind one hogd-maintained process with one readiness signal
and one log stream.

## Why revisit the shape

The current Level B prototype is useful because it proves the Anvil-like DX
loop:

```text
groundhog node
groundhog deploy
groundhog call
```

But its backend is test-harness-shaped:

```text
Rust dev-node
  -> SDK native_exec TestEnvironment
  -> hogd test_ffi
  -> real hogexec runtime/syscall machinery
```

That reaches real execution semantics, but it skips too much of the local-node
lifecycle:

- production mempool admission
- scheduler nomination/commit path
- auth/replay behavior as seen by mempool
- event/read/index retention as a node concern
- clean long-lived reset/snapshot/revert semantics

`MempoolTestInfra` points at a better middle ground. It already stitches real
hogd components into one process for tests:

- `MempoolCoordinatorRpc`
- `Scheduler`
- `Executor`
- `AuthServer`
- HogDB over RAMDB
- DB RPC servers for executor/scheduler/auth
- event aggregation
- fake coordinator callback server

That is much closer to "local node" than `NativeContractTestFramework` or
`native_exec`.

## Non-decisions

This document also does **not** rule out a future `libhogdev` FFI backend. It
argues that the first durable dev-node target can be a hogd-owned subprocess.
FFI should remain contingent on a real need such as high-frequency per-test
embedding or a hard single-binary distribution requirement. Or perhaps a
proof point that is a better trade off of where the complexity boundary
lives.

## The gateway relationship

The RPC gateway does not go away. It becomes the reusable Rust JSON-RPC
presentation layer.

Today we already have shared RPC crates:

- `crates/api-types` — JSON-facing types
- `crates/rpc-api` — jsonrpsee trait definitions
- `crates/client` — generated typed JSON-RPC client

The missing piece is a reusable server library. Today `crates/gateway` is a
binary with internal modules (`api`, `context`, `handlers`, `submitter`,
`tip_coalescer`). The dev-node separately implements the same
`GroundhogApiServer` in `crates/dev-node/src/rpc.rs`.

The reusable server library should be the JSON-RPC handler layer: request
validation, public error mapping, JSON-RPC plumbing, and shared method
semantics. It should not absorb every backend adapter or operational concern.
For example, gateway-specific batching and coalescing such as `submitter` and
`tip_coalescer` can remain gateway-owned and simply fulfill the backend traits
consumed by the shared server.

The proposed shape is:

```text
crates/rpc-server
  · implements GroundhogApiServer once
  · owns JSON-RPC error mapping, request validation, range checks, middleware hooks
  · depends on backend traits, not concrete production topology

crates/gateway
  · production binary
  · wires rpc-server to production scheduler/indexer/coordinator clients

crates/dev-node
  · local node binary/library used by `groundhog node`
  · starts hogd-dev child process
  · wires rpc-server to local hogd-dev clients
  · adds groundhog_dev_* RPC namespace
```

So `groundhog node` still starts a JSON-RPC server. The CLI's deploy/call path
should continue to talk to `http://localhost:9545` using the same JSON-RPC
client it uses for any other network.

The dev-node should not keep a duplicate implementation of
`GroundhogApiServer`. It should compose the shared server with local backend
adapters.

This also removes the earlier "gateway `--dev` mode" question from
prototype-era discussions. The production gateway stays the production
gateway. The local dev-node is its own composition target that reuses
the same `rpc-server` library.

## Process topology

The user-facing process is still `groundhog node`.

Internally it supervises:

```text
groundhog node process
  ├─ Rust JSON-RPC server on 127.0.0.1:9545
  ├─ x23 clients to hogd-dev
  ├─ deterministic dev accounts / startup display
  ├─ backend health supervision
  └─ child process: hogd-dev

hogd-dev process
  ├─ public x23 scheduler endpoint
  ├─ public x23 dev-read endpoint
  ├─ public x23 dev-control endpoint
  └─ internal hogd components with ephemeral/private ports
```

`hogd-dev` may be a new binary, or a `hogd dev-node` mode. That is a naming
and packaging decision. The important property is that the Rust tooling starts
one hogd-owned process, not five separate service binaries.

The supervision contract is part of the product, not incidental plumbing:

- readiness handshake before JSON-RPC starts accepting traffic
- child process killed when the parent exits
- child stdout/stderr forwarded into the dev-node log stream
- clear startup error if ports, protocol, genesis, or binary lookup fail
- crash detection during a session
- explicit restart policy, likely "fail cleanly" for v1 rather than automatic
  stateful restart

## Backend contracts

`chuckd_scheduler::SchedulerClient` remains the right aperture for the
coordinator/mempool write path:

- `submit_unvetted`
- `submit_vetted`
- `lookup_transactions`
- `get_tip`
- `nominate`
- `validate`
- `commit`

This implies promoting `chuckd_scheduler` from a chuckd-rs internal crate to
a shareable Rust crate consumed by groundhog-rpc (dev-node and gateway), the
chuckd-rs coordinator, and any future embedded backend. The trait, the
`TcpSchedulerClient` impl, and the testkit mock should all be reachable from
one dependency. The crate's long-term home is open — it can stay in chuckd-rs
with groundhog-rpc taking a dep, move into groundhog-rpc if this becomes the
canonical Rust home for hogd-facing protocol code, or live in a shared
workspace crate. Short-term default: leave it in chuckd-rs and depend on it
from here. See the open question below.

But `SchedulerClient` is not the entire dev-node backend. The public JSON-RPC
surface also needs reads and dev controls.

The Rust server should depend on a small set of backend traits:

```rust
trait SubmitPlane {
    async fn submit_raw_transaction(&self, tx_bytes: Vec<u8>) -> Result<TxHash, Error>;
}

trait ChainPlane {
    async fn chain_tip(&self) -> Result<ChainTip, Error>;
    async fn get_block_by_number(&self, height: BlockHeight) -> Result<Option<BlockInfo>, Error>;
    async fn pending_transaction_count(&self) -> Result<u64, Error>;
}

trait ReadPlane {
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, Error>;

    async fn transaction_status(&self, tx_hash: TxHash) -> Result<TransactionStatus, Error>;
    async fn contract_events(
        &self,
        address: ContractAddress,
        from: BlockHeight,
        to: BlockHeight,
    ) -> Result<Vec<IdentifiedEvent>, Error>;

    async fn latest_storage(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<Option<StorageItemInfo>, Error>;
}

trait DevControl {
    async fn reset(&self) -> Result<(), Error>;
    async fn snapshot(&self) -> Result<SnapshotId, Error>;
    async fn revert(&self, id: SnapshotId) -> Result<bool, Error>;
    async fn advance_block(&self) -> Result<ChainTip, Error>;
    async fn set_time(&self, timestamp: u64) -> Result<(), Error>;
    async fn set_storage_tokens(&self, address: ContractAddress, amount: u64)
        -> Result<(), Error>;
}
```

Production gateway adapters can back public methods with scheduler + indexer
clients. Local dev adapters can back them with hogd-dev x23 programs. The
`latest_storage` method is dev-only in the current RPC surface; it should be
HogDB-backed through a dev-read API, not indexer-backed.

## What hogd-dev owns

The hogd-owned local backend should own the behavior that Rust should not
reconstruct:

- transaction auth/replay/mempool admission
- scheduler nomination and commit
- executor/runtime/syscall behavior
- HogDB lifecycle, storage deltas, and DB commit semantics
- HogDB-backed latest-state reads
- event/indexer capture and committed-read retention
- reset/snapshot/revert lifecycle
- dev-only mutation hooks

Rust should own:

- CLI UX and process supervision
- JSON-RPC protocol and error mapping
- deterministic account presentation and config
- request validation that belongs to the public API
- translating x23 backend results into `api-types`

## Relationship to current options

### Compared to `native_exec`

The current dev-node directly verifies auth, registers deploy artifacts, runs
`TransactionHandler`, reconstructs receipts/events/traces, and advances a
synthetic block counter in Rust.

The hogd-dev backend moves those correctness-sensitive responsibilities below
the RPC layer. Rust stops pretending to be a node and becomes the API facade
for a real local node runtime.

### Compared to the multi-process `--no-chuckd` stack

The existing `chuckd-rs/scripts/launcher.py --no-chuckd` starts hogexec,
event, hogdb, auth, and mempool as separate processes. That is a useful
existence proof, but it is not the ideal user-facing backend for
`groundhog node`.

`hogd-dev` should provide the same essential local services behind one
process boundary, with internal ports hidden and lifecycle handled in one
place.

This is still a multi-component local runtime. The improvement is not that the
internal component graph disappears. The improvement is that developers and the
Rust CLI no longer manage that graph directly.

### Compared to `libhogdev`

`libhogdev` would make Rust embed hogd behavior through a C ABI. That is still
possible later, but it creates long-term costs around ABI versioning,
ownership, threading, and crash behavior.

The process boundary avoids those costs. Rust talks to hogd-dev over x23,
which is already the production-adjacent control style.

The proposal makes a stronger bet than "defer and decide later": the dev-node
should use the hybrid subprocess model. A future `libhogdev` may still be a
valuable hogd capability for other consumers, especially high-frequency SDK
tests or embedded tools, but the dev-node use case alone does not justify the
C ABI surface. If `libhogdev` exists later, it should slot behind the same
backend traits.

This also clarifies the relationship to the embedded-backend spec. That spec
describes a real capability hogd may eventually want. Under this proposal,
`groundhog node` is no longer the forcing function for that capability.

### Compared to full localnet

This is not chuckd consensus, gossip, repair, catchup, or multi-validator
simulation. Those belong to a heavier `groundhog localnet` or CI validation
mode.

The local dev-node is a single-node, instant-finality development runtime.

## Transaction flow

Default auto-advance flow, equivalent to Anvil's auto-mine behavior:

```text
groundhog_sendRawTransaction
  -> rpc-server validates hex/XDR and computes tx hash
  -> SubmitPlane submits via SchedulerClient::submit_unvetted
  -> hogd-dev auth/mempool path vets and stores admissible txs
  -> dev-node block-production policy calls nominate + commit
  -> ReadPlane can serve receipt/events/status
  -> JSON-RPC returns tx hash
```

One subtlety: `submit_unvetted` currently returns `()` for batch ingest. hogd
may filter invalid transactions during auth verification before adding them to
the scheduler. That means `sendRawTransaction` cannot automatically be treated
as "this exact transaction committed" unless the dev-node performs a follow-up
read after block production.

Open design choice:

- **Default:** preserve production-style semantics. Return tx hash once
  accepted by the ingest path, then clients poll status/receipt.
- **Optional dev strict mode:** in auto-advance mode, return an error if the
  submitted tx is absent after the block-production attempt.

The default should keep local and production JSON-RPC behavior close. A stricter
mode may still be useful, but it should be visibly dev-only, for example
`groundhog node --strict-advance`.

## Read-plane options

The read side is where `SchedulerClient` is insufficient. It is also not a
single backend concern: latest state is HogDB-shaped, while committed
receipt/status/event history is indexer-shaped.

Options:

1. **Embed a composite dev-read service inside hogd-dev.**
   - Reuse HogDB for latest-state reads.
   - Reuse hogd event/indexing concepts for receipts, event filters, and
     committed tx status.
   - Add dev-only retention for tx details/traces if production indexer data
     is insufficient.
   - Expose one dev-read x23 program to Rust.
   - Best long-term shape for one process.

2. **Run the real event/indexer path as internal components.**
   - Closest to production read behavior.
   - More moving parts inside the process.
   - Still needs a HogDB-backed dev-read method for latest storage.
   - May be the right implementation strategy if the indexer code is already
     reusable.

3. **Keep Rust-side in-memory receipts/events initially.**
   - Fastest bridge from today's dev-node.
   - Weakest correctness story.
   - Should be temporary only, because it keeps Rust reconstructing semantic
     outcomes from implementation details.
   - Latest storage should still come from HogDB if hogd-dev exposes it.

Preferred direction: (1), possibly implemented using pieces from (2). That
keeps Rust talking to one dev-read surface while hogd-dev owns the distinction
between canonical state and derived committed-read indexes.

This is one of the two major hogd-side work items in the proposal. Production
hogd's event/indexer code is non-trivial, and an embedded read plane inside
hogd-dev is a real engineering project. The HogDB-backed latest-state side also
needs a supported dev-read surface rather than test-only access. The other
major hogd-side dependency is snapshot/revert support over the relevant HogDB,
indexer/read-store, and node lifecycle state.

## Dev-control surface

`groundhog_dev_*` methods should be backed by hogd-dev, not by ad hoc Rust
state.

Initial methods:

- `groundhog_dev_reset`
- `groundhog_dev_getAccounts`
- `groundhog_dev_getTransactionDetail`
- `groundhog_dev_getStorageAt`
- `groundhog_dev_advanceBlock`
- `groundhog_dev_snapshot`
- `groundhog_dev_revert`
- `groundhog_dev_setTime`
- `groundhog_dev_setStorageTokens`

Ethereum/Anvil-compatible aliases such as `evm_mine` or `anvil_mine` can still
map to Groundhog block advancement. The Groundhog-native terminology should be
"advance" or "block production," not "mine."

`getAccounts` can remain Rust-owned if accounts are deterministic and also
passed into hogd-dev at genesis. Rust should generate the deterministic keys
and pass dev genesis/account configuration into hogd-dev; that keeps secret
material in the consumer's control and matches hogd's existing config-driven
shape. Everything that mutates or reads chain state should go through hogd-dev.
Specifically, `getStorageAt` should be HogDB-backed, while transaction
detail/receipt/event reads should come from the indexer-shaped read store.

Snapshot/revert needs a precise scope:

- HogDB state
- mempool contents
- auth/replay state
- current block/timestamp
- event/read-store retention
- receipts/traces/transaction detail

For local developer expectations, snapshots should probably include the whole
observable node state, not just storage.

## Implementation stages

### Stage 0 — Align on architecture

- Review this doc with CLI, gateway, chuckd-rs, and hogd owners.
- Decide whether the durable target is a hogd-owned subprocess rather than
  `libhogdev` as the primary plan.
- Keep the monorepo question separate.

### Stage 1 — Extract reusable Rust RPC server

- Create `crates/rpc-server`, or turn `crates/gateway` into a library plus
  binary.
- Move `GroundhogApiServer` implementation and shared handler logic into the
  library.
- Define backend traits for submit/chain/read/dev-control.
- Keep current production gateway behavior unchanged.
- Keep current `native_exec` dev-node behavior behind adapters so users lose
  no functionality while the hogd-dev backend is built.

Stage 1 stands on its own merits. Even if hogd-dev never ships, extracting
the shared RPC server and backend traits removes the duplicated
`GroundhogApiServer` implementation across `crates/gateway` and
`crates/dev-node`, and gives the production gateway a cleaner internal seam
between handler logic and backend adapters. The work de-risks the overall
proposal: at worst, the codebase ends up with a better-factored gateway.

### Stage 2 — Build hogd-dev prototype

- Extract `MempoolTestInfra` wiring into supported non-test hogd code.
- Hide internal ports; expose stable scheduler/read/control endpoints.
- Make the read endpoint explicit about HogDB-backed latest-state reads versus
  indexer-shaped committed receipt/event reads.
- Add clean startup and shutdown contracts.
- Accept a dev genesis config with deterministic accounts, wallet contracts,
  storage tokens, and cost model.
- Support empty/resettable ephemeral state first.

This is the gating stage. `MempoolTestInfra` is an existence proof, not a
production lifecycle contract. Its shutdown path already documents fragile
ordering and exception-prone teardown. Promoting it will likely require real
hogd work around scheduler/executor shutdown, detached coroutine draining,
component ownership, and startup/readiness reporting. Hogd owner buy-in is a
hard dependency.

### Stage 3 — Wire experimental Rust backend

- Add `groundhog node --backend hogd-dev`.
- Rust starts hogd-dev, waits for readiness, connects x23 clients, then starts
  JSON-RPC.
- Use `chuckd_scheduler::TcpSchedulerClient` for scheduler operations.
- Add read/control x23 clients as they become available.

### Stage 4 — Fill Anvil-style dev lifecycle

- Add auto/manual/interval block advancement policies.
- Add snapshot/revert.
- Add set-time.
- Add storage-token mutation.
- Add transaction detail and trace retention.
- Add state persistence if/when hogd-dev has a clear HogDB/read-store
  persistence story.

Snapshot/revert should be treated as a hard backend capability, not a Rust
shim. It needs hogd-side semantics for HogDB state plus all observable
local-node state listed above.

### Stage 5 — Flip default and retire prototype coupling

- Make hogd-dev the default backend for `groundhog node`.
- Keep `native_exec` only where it is still valuable for SDK/unit-test style
  loops, or remove it from the CLI dev-node path.
- Update docs and help text to present the current backend as supported, not
  experimental prototype infrastructure.

## Packaging and distribution

User-facing install should still feel like one tool:

```bash
groundhog node
```

Possible packaging options:

- Ship `groundhog` and `hogd-dev` together in one release artifact.
- Look up `hogd-dev` on `PATH` for source-tree developers.
- Add `--hogd-dev-binary` for explicit local builds and CI.

The strongest DX is a bundled two-binary release. It keeps the process
boundary without requiring users to manually install hogd.

## Open questions

1. **Hogd-dev lifecycle commitment. (Gating.)**
   Which hogd owners sign up for the supported startup/shutdown/readiness
   contract, and what cleanup is required before it can be exposed to CLI
   users? This is the single largest external dependency in the proposal —
   without an answer here, Stages 2 and 4 are aspirational.

2. **Name and ownership.**
   Is the child process `hogd-dev`, `hogd dev-node`, or a mode of an existing
   hogd binary?

3. **Read-plane implementation.**
   Should hogd-dev expose one composite dev-read endpoint backed by HogDB for
   latest state and an indexer-like store for committed receipts/events, or
   should those stay as separate internal APIs?

4. **`sendRawTransaction` dev semantics.**
   Default should preserve production-style submit-and-poll semantics. Is an
   explicit `--strict-advance` mode worth adding for fast local feedback?

5. **Genesis and accounts.**
   Proposed default: Rust generates deterministic account keys and passes
   account/genesis config to hogd-dev. Confirm hogd owners are comfortable
   with that split.

6. **Snapshot scope.**
   Are snapshots whole-node observable state, or only HogDB state?

7. **State persistence.**
   Is v1 ephemeral only, or should the hogd-dev process support a ledger/state
   directory from the beginning?

8. **Backend trait granularity.**
   Should `SubmitPlane`, `ChainPlane`, and `ReadPlane` be separate traits, or
   one `NodeBackend` trait with grouped methods?

9. **Production gateway reuse.**
   How much of `crates/gateway` should become `crates/rpc-server` before the
   dev backend exists?

10. **Conformance tests.**
    Where do tests live that assert the same JSON-RPC behavior against:
    production gateway mocks, current native-exec backend, and hogd-dev?

11. **Long-term home for `chuckd_scheduler`.**
    - Option A: stays in chuckd-rs; groundhog-rpc takes a registry/path dep.
      Simplest; tightest coupling to chuckd-rs's release cadence.
    - Option B: moves to groundhog-rpc; chuckd-rs takes a dep. Sensible if
      groundhog-rpc becomes the canonical Rust-side home for hogd-facing
      protocol code.
    - Option C: moves to a shared workspace crate outside both repos.
      Cleanest ownership story; most upfront packaging work.

    Short-term default: A. Revisit when the trait shape needs to change in
    lockstep with both consumers, or when a third consumer (gateway,
    embedded backend) makes shared ownership the obvious shape.

## Decision points before implementation

- Approve a hogd-owned subprocess as the preferred durable dev-node backend.
- Approve keeping the Rust JSON-RPC server as the public API facade for both
  production gateway and local dev.
- Approve extracting reusable RPC-server code before wiring hogd-dev.
- Decide the first read-plane implementation.
- Confirm production-style `sendRawTransaction` semantics as the default, with
  any stricter dev behavior behind an explicit flag.
- Decide packaging expectations for `hogd-dev` in local source builds and
  released CLI artifacts.
- Confirm hogd-side ownership for the single-process lifecycle and read-plane
  work.
