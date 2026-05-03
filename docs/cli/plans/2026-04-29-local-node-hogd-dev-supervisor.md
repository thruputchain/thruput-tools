# Local Node Hogd-Dev Supervisor Implementation Plan

**Date:** 2026-04-29
**Status:** Implementation in progress. PRs 1–4c and the backend plane-status
hoist shipped on the Rust supervisor side. End-to-end coverage of the read
plane is gated on the hogd-dev v1 binary exposing the indexer endpoint with
full event-feed wiring. PR 5 ahead.

**Companion to:**

- [`../2026-04-25-hogd-dev-node-hybrid.md`](../2026-04-25-hogd-dev-node-hybrid.md)
  - durable hogd-owned subprocess architecture.
- [`../specs/2026-04-29-hogd-dev-process-contract.md`](../specs/2026-04-29-hogd-dev-process-contract.md)
  - draft hogd-dev subprocess contract proposal.
- [`2026-04-30-hogd-dev-binary-bootstrap.md`](2026-04-30-hogd-dev-binary-bootstrap.md)
  - hogd-side bootstrap plan for the smallest credible `hogd-dev` v1
    (gating PR 4a).

## Goal

Create a fresh Rust crate, `crates/node-supervisor`
(`groundhog-node-supervisor`), that becomes the CLI-facing supervisor/facade
for `groundhog node`.

Use "local node" for the user-facing product shape, not for the crate name.
The crate is the supervisor; naming it that avoids confusion with the legacy
`crates/dev-node` prototype and with "the local node" as the running system.

The durable backend target is a hogd-owned `hogd-dev` subprocess. The current
`crates/dev-node` native-exec path should be treated as legacy prototype code
and reference material, not as a supported long-term backend family.

The supervisor crate should own Rust-side lifecycle concerns:

- generate dev accounts/genesis input;
- start `hogd-dev`;
- wait for readiness;
- wire x23 clients into `GroundhogRpcServer<S, C, R>`;
- expose `groundhog_dev_*` methods when hogd-dev has matching control APIs;
- forward logs and shut the child down cleanly.

## Non-Goals

- Do not build a broad long-term backend plugin framework.
- Do not polish `native_exec` into a first-class selectable backend unless it
  remains useful as a short-lived test harness.
- Do not copy a general service manager such as `vigil-rs` or hogx's full
  agent model. One supervised child process is enough for v1.
- Do not implement snapshot/revert/time travel as Rust-side shims. Those are
  hogd-dev capabilities once the process owns all observable local-node state.

## Coordination Discipline

We can change code anywhere — `groundhog-rpc`, `hogd-dev`, broader hogd,
`protocol-rs`, `xdrs`, `protocol` — but coordination cost varies sharply with
reach. The hogd-dev submodule is isolated. Shared hogd code touches other
teams' work. Protocol XDR changes affect every consumer (native C++,
protocol-go, protocol-rs). Reason about reach explicitly:

- When a smaller change reveals a desire for a wider one, document the case
  in the relevant plan or spec doc even if we defer the wider change.
- Prefer landing the smaller change behind a clean seam — a TODO with a
  tracking issue, a typed `NotImplemented` return, a stubbed plane — rather
  than half-building toward a foundation we may want to revisit.
- Widen scope deliberately, once the design is well enough understood and
  the right people are looped in.

Goal: avoid encoding a foundation we'll regret without blocking on every
adjacent improvement.

## References To Reuse Carefully

### hogd `MempoolTestInfra`

Source reference in sibling repo `hogd`: `hogd/mempool/test/infra.{h,cc}`.

This is the closest existing hogd-side proof point. It wires:

- mempool coordinator RPC;
- scheduler;
- executor;
- auth;
- HogDB over RAMDB;
- database RPC servers for executor/scheduler/auth;
- event aggregation;
- fake coordinator callback server;
- genesis loading.

It is not a production lifecycle contract. Its destructor documents fragile
shutdown ordering and detached-coroutine hazards. The hogd-dev work should
promote the useful wiring into supported non-test code with explicit startup,
readiness, and shutdown semantics.

### hogx process supervision

Source reference in sibling repo `hogx`: `cmd-agent/src/supervisor.rs`.

Useful ideas:

- start child processes with piped stdout/stderr;
- copy logs into a combined stream;
- on Unix, make the child a process-group leader;
- stop the process group with `SIGTERM`, then `SIGKILL` after a grace period;
- keep queryable state about pid, start time, exit status, and TCP listeners.

Keep only the one-child subset. The supervisor crate should not grow an agent,
unit graph, persistent host state, or remote control plane.

### hogx CLI lifecycle UX

Source references:

- `cmd-hogx/src/start.rs`
- `cmd-hogx/src/wait.rs`
- `cmd-hogx/src/stop.rs`

Useful ideas:

- explicit planning/config phase before start;
- reject conflicting already-running sessions;
- wait loop that treats unexpected component death as failure;
- stop everything in the context if one required unit dies.

For `groundhog node`, most of this collapses to one process plus one JSON-RPC
server. The user-facing command should stay simple.

### chuckd-rs

Use chuckd-rs for local orchestration and x23-client lessons, especially any
existing scheduler client code that can become the Rust-side `SubmitPlane` and
`ChainPlane` bridge. Do not make node-supervisor depend on chuckd's coordinator
architecture unless the x23 client crate boundary is intentionally shared.

## Target Crate Shape

```text
crates/node-supervisor
  Cargo.toml
  src/lib.rs
  src/config.rs          # NodeSupervisorConfig, HogdDevConfig, defaults
  src/accounts.rs        # deterministic dev accounts/genesis inputs
  src/process.rs         # one-child supervisor
  src/hogd_dev.rs        # hogd-dev command line, readiness, endpoints
  src/rpc_backend.rs     # Submit/Chain/Read adapters over hogd-dev clients
  src/dev_control.rs     # groundhog_dev_* adapter once hogd-dev supports it
  src/server.rs          # JSON-RPC facade startup and handle
```

`crates/dev-node` remains in the tree as reference material for the retired
native-exec prototype. It is not a workspace member and is not wired into the
CLI path.

## Hogd-Dev Process Contract

The Rust supervisor needs a small, explicit contract. The contract can start as
a doc plus stub command-line shape, then become executable once hogd-dev exists.
PR 2 is gated on written agreement with hogd owners for the binary name,
readiness mechanism, endpoint listing, and shutdown semantics. Use the draft
contract proposal and its pre-PR2 checklist linked above as the starting point,
then mirror or link the accepted contract from hogd.

Required v1 inputs:

- listen host, default `127.0.0.1`;
- port assignment strategy, preferably `0`/auto for internal x23 ports;
- v1 genesis bootstrap over the scheduler x23 endpoint after readiness;
- account count and storage-token amount;
- v1 state is ephemeral; add a state directory flag only when persistence is
  real;
- logs on stderr as newline-delimited JSON records suitable for forwarding into
  `groundhog node` output.

Required v1 outputs:

- machine-readable readiness signal;
- endpoint list for scheduler/submit, with read and control added when those
  services exist;
- child exit status;
- clear startup failure messages.

Preferred readiness shape:

```json
{
  "status": "ready",
  "network_id": "groundhog-local",
  "endpoints": {
    "scheduler": "127.0.0.1:...",
    "read": "127.0.0.1:...",
    "control": "127.0.0.1:..."
  }
}
```

Preferred v1 readiness is one stdout JSON line matching this shape, with stdout
otherwise reserved for machine protocol. Ordinary logs go to stderr. If hogd
owners choose a readiness file or control port instead, change PR 2 before
writing tests against the fake child.

Default contract: v1 hogd-dev advertises one `read` key multiplexing the
indexer-side x23 services it exposes, if hogd owners agree that one listener
can cleanly host the needed indexer service. If hogd-dev v1 needs to listen on
multiple x23 services for the read plane (e.g., a separate block-store
endpoint), enumerate them as additional named keys rather than overloading
`read`. The single-key default matches the existing readiness contract and
what gateway already consumes
(`groundhog_indexer::IndexerBlockExplorer_IndexerBlockExplorer1_Client`).

Required shutdown contract:

- `SIGTERM` requests graceful shutdown;
- hogd-dev stops accepting new requests;
- in-flight internal tasks drain or fail deterministically;
- process exits within the configured shutdown grace timeout, default 5s;
- Rust sends `SIGKILL` only after that timeout;
- Rust starts hogd-dev in its own process group and stops the group, not just
  the direct child.
- v1 is ephemeral, so shutdown grace exists for log drain and clean SIGTERM
  handling rather than state persistence.
- On Linux, use `prctl(PR_SET_PDEATHSIG, SIGTERM)` if it is cheap to add. On
  macOS there is no equivalent; document orphan-child risk for v1 rather than
  building a persistent agent.

## RPC Adapter Shape

`groundhog-node-supervisor` consumes `groundhog-rpc-server` directly:

```text
GroundhogRpcServer<SubmitClient, ChainClient, ReadClient>
```

The `SubmitPlane` / `ChainPlane` / `ReadPlane` traits in
`crates/rpc-server/src/backend.rs` are the codified contract. The mapping
below documents where each method terminates in hogd-dev's x23 services:

- `SubmitPlane::submit_raw_transaction` -> scheduler/submit endpoint.
- `ChainPlane::chain_tip` -> scheduler/readiness endpoint.
- `ChainPlane::pending_transaction_count` -> real hogd-dev mempool count once
  hogd-dev exposes a count/size RPC. Until then, do not pretend parity; return
  `NotImplemented`.
- `ChainPlane::get_block_by_number` -> read endpoint once hogd-dev can answer.
- `ReadPlane::transaction_receipt` -> read endpoint.
- `ReadPlane::transaction_status` -> composite read endpoint, or a local
  adapter that queries scheduler pending state then committed read state. A
  scheduler-only pending probe is not enough for the public method because a
  scheduler miss can mean either unknown or already committed.
- `ReadPlane::contract_events` -> read/indexer endpoint. PR #39 settled the
  JSON-RPC shape as paginated `getEvents(EventQuery) -> EventPage`, with
  bounds validation in `groundhog-rpc-server`, so the supervisor adapter should
  preserve that shape rather than inventing a local event-query API.

Keep `sendRawTransaction` production-style by default: accepted into the local
mempool path means the method returns the canonical hash; callers poll status
or receipt. Any auto-advance or strict commit behavior should be explicit.

## CLI Shape

Initial CLI shape:

```bash
groundhog node
groundhog node --hogd-dev-binary /path/to/hogd-dev
```

Avoid exposing `--backend native-exec|hogd-dev` as a long-term UX. If a backend
flag is temporarily useful during bring-up, mark it hidden or experimental and
do not promise parity. Do not silently fall back to native-exec when hogd-dev
startup fails; fail clearly.

Binary lookup order:

1. explicit `--hogd-dev-binary`;
2. `GROUNDHOG_HOGD_DEV_BINARY`;
3. sibling path next to the `groundhog` binary
   (`std::env::current_exe().parent()`), for bundled releases;
4. `PATH`;
5. typed startup error with install/build guidance.

Do not expose `--state-dir` until hogd-dev persistence is real.

JSON startup output should continue to include:

- RPC URL;
- chain/mode;
- backend kind and stability;
- deterministic dev accounts.

Human startup output should identify the backend clearly:

```text
Groundhog Local Node
Backend: hogd-dev
Listening on http://127.0.0.1:9545
```

## Implementation PRs

### PR 1 - Contract and Crate Scaffold

**Status:** ✅ Shipped in PR #30.

- Add this plan and any hogd-dev subprocess contract doc needed for review.
- Add `crates/node-supervisor` to the workspace.
- Define `NodeSupervisorConfig`, `NodeSupervisorHandle`, and error types.
- Add `process.rs` with a minimal one-child supervisor abstraction, tested with
  a trivial local command.
- Do not wire the CLI yet unless the scaffold is otherwise too hard to test.

Acceptance:

```bash
cargo test -p groundhog-node-supervisor
cargo clippy -p groundhog-node-supervisor --all-targets -- -D warnings
cargo fmt --check
```

### PR 2 - Hogd-Dev Process Adapter Skeleton

**Status:** ✅ Shipped in PR #31.

- Add `hogd_dev.rs` that builds the child command line and parses readiness.
- Implement the agreed binary lookup order.
- Add tests for command construction, readiness parsing, startup timeout, and
  child-exit-before-ready behavior.
- Use a fake helper process in tests; do not require real hogd-dev yet.
- Do not start this PR until the hogd-dev process contract one-pager is agreed.

Acceptance:

- a fake child can report readiness and be stopped cleanly;
- a fake child that exits before readiness returns a typed startup error;
- logs are captured or forwarded predictably.

### PR 3 - CLI Integration Behind Experimental Path

**Status:** ✅ Shipped in PR #35. Includes interim PR #34 (supervisor JSON-RPC
startup with stub backend) which the CLI integration sits on top of, plus
the supporting `groundhog-x23-transport` extraction in PR #33 and the
client-boundary decision in PR #32.

- Add a `groundhog node` path that starts `groundhog-node-supervisor` when the
  required hogd-dev binary is configured.
- Keep native-exec out of the CLI path; if it is needed again for comparison,
  revive it behind a separate internal feature.
- Preserve the existing JSON/human startup payload shape as much as possible.

This PR can land before real hogd-dev works if it uses a hidden flag or
requires an explicit binary path and fails gracefully. The default path should
not silently fall back to native-exec once the supervisor path is selected.

### PR 4a - First Real Hogd-Dev Chain RPC

**Status:** ✅ Shipped. `groundhog_chainTip` is backed by hogd-dev's scheduler
endpoint, with supervisor-owned dev genesis bootstrap.

- Decide the Rust x23 client crate boundary.
- Add the minimal client needed for `ChainPlane::chain_tip`.
- Add the supervisor-side genesis bootstrap: copy
  `chuckd_scheduler::empty_genesis_config()` and the
  `get_tip → NoGenesisBlock → load_genesis → re-tip` dance from chuckd-rs
  (see `2026-04-30-hogd-dev-binary-bootstrap.md` for provenance). No
  `chuckd-rs` Cargo dep — copy with provenance comments.
- Start hogd-dev, run the genesis bootstrap, and prove `groundhog_chainTip`
  works end to end through the JSON-RPC facade.

Acceptance:

- `groundhog node` starts hogd-dev, loads empty genesis, and JSON-RPC;
- `groundhog_chainTip` works;

### PR 4b - Submit RPC

**Status:** ✅ Shipped in PR #40. `groundhog_sendRawTransaction` validates and
hashes through `groundhog-rpc-server`, submits the signed transaction bytes to
hogd-dev through `schedulercoord_submit_unvetted(ParallelSignedTransactionBatch)`,
and returns the canonical transaction hash when the scheduler RPC succeeds.
Typed rejection mapping is limited by the current x23/protocol-rs shape; see
the typed-submit follow-up below.

- Confirm or add the minimal hogd-dev submit contract first: the x23 method
  that accepts signed transaction bytes, its accepted/rejected outcomes, and
  whether it returns any data beyond acceptance.
- Extend hogd-dev just enough to route submissions through the scheduler/mempool
  path if the scheduler endpoint does not already expose the needed behavior.
- Implement `SubmitPlane` over the hogd-dev scheduler/submit endpoint.
- Preserve shared transaction validation and hash computation through
  `groundhog-rpc-server`.

Acceptance:

- invalid transaction input still fails through shared validation;
- accepted transactions return the canonical hash.

Protocol shape notes to discuss with hogd/protocol-rs owners:

- `schedulercoord_submit_unvetted` returns `void`; acceptance is inferred from
  absence of an x23/RPC error. A typed submit result would let the JSON-RPC
  facade distinguish accepted, rejected, duplicate, and retryable outcomes.
- The generated x23 dispatcher maps handler `ProgramError` to generic
  `RpcError::SystemError`, so semantic scheduler rejection is currently
  indistinguishable from backend failure at the Rust client boundary.
- `ParallelSignedTransactionBatch` is the right scheduler-oriented batch
  transport, but callers must manually construct `segment_tokens`,
  `segment_bytes`, and concatenated `xdr` even for a single transaction.
  protocol-rs helper constructors would reduce adapter drift.

### PR 4c - Read RPC

**Status:** Shipped on the Rust supervisor side in PR #44.
`HogdDevReadBackend` (in `crates/node-supervisor/src/read_backend.rs`)
implements `ReadPlane` against hogd-dev's `IndexerBlockExplorer1` endpoint:
receipts, events, and the composite scheduler-pending-then-indexer-committed
transaction status. `start_rpc_server` connects to the indexer when
readiness JSON advertises `read: <addr>` (probing
`indexer_block_explorer_intro` for service/version) and falls back to
`StubBackend` otherwise. `BackendPlanes::hogd_dev_full()` advertises
`{Scheduler, Scheduler, Indexer}` in startup metadata. The fake hogd-dev
fixture grew an `IndexerBlockExplorer1` server with canned committed-set
and event log, plus failure modes (`indexer-connect-fail`,
`indexer-rpc-fail`, `indexer-runtime-rpc-fail`) used by the integration
tests.

End-to-end coverage is gated on the hogd-dev v1 binary actually advertising
the read endpoint with full event-feed wiring (Option B in the bootstrap
plan: connect the existing scheduler/executor event flow into
`IndexerEventRpc` so blocks committed by hogd-dev populate IndexDB).
Until then, the supervisor adapter runs only against the fake fixture.

Decisions captured here that survived implementation:

- The supervisor read adapter owns the composite status flow per the
  `ReadPlane::transaction_status` trait contract: scheduler pending state
  first, indexer committed state second. Gateway already implements this at
  `crates/gateway/src/handlers/receipt.rs`.
- Gateway-style degraded-mode semantics: scheduler failure may degrade
  pending detection (warn + continue), but indexer failure is backend
  unavailable because committed state is authoritative.
- `ChainPlane::get_block_by_number` returns `NotImplemented` in v1. Block
  materialization in hogd-dev would require a hogdb block source or a
  coordinator-style block store, neither of which is in scope for the
  hogd-dev submodule today. Stays stubbed until hogd grows that surface.

Acceptance criteria for closing this section out (pending the hogd-dev
binary):

- receipts, events, and committed transaction status work end to end against
  a real hogd-dev v1 binary advertising a read endpoint;
- unknown tx status/receipt behavior matches conformance expectations;
- current deploy/call loop either works or has a documented blocker.

### PR 4d - Supervisor Block Driver

**Status:** In progress on the Rust supervisor side. The first real-binary
smoke after the read endpoint landed showed that `groundhog_sendRawTransaction`
can reach hogd-dev's scheduler, but the local topology has no chuckd
coordinator to drive `schedulercoord_nominate` / `schedulercoord_commit_block`.
Result before this PR: deploy returns a tx hash, receipt polling times out,
status stays unknown, and `chainTip` remains at genesis.

Design choice: `groundhog node`, not `deploy` or `call`, owns this behavior.
Client commands should remain ordinary JSON-RPC clients. The local-node
supervisor supplies the missing single-node coordination policy so all clients
see the same semantics.

Initial implementation shape:

- add a supervisor-owned coalescing block-driver task in `node-supervisor`;
- wake it only after `schedulercoord_submit_unvetted` succeeds;
- one wake drives `schedcoord_get_tip` -> `schedulercoord_nominate` ->
  `schedulercoord_commit_block`;
- keep `sendRawTransaction` semantics as "accepted by the local scheduler
  submit path"; do not wait for commit inside submit;
- stop the driver before stopping hogd-dev.

Block-driver tests use fake hogd-dev automine mode to prove a successful submit
wakes the driver and advances `chainTip`. A follow-up real-binary smoke against
a rebuilt hogd-dev confirmed the same block-production signal: a deploy submit
still times out waiting for a receipt, but `chainTip` advances from height 0 to
height 1. The submitted tx remains `unknown`, so the remaining blocker is
dev-wallet/genesis/auth bootstrap or scheduler inclusion policy for that dev
transaction, not missing local block production.

Remaining blocker for the default CLI local loop: the current supervisor genesis
is still empty. The native-exec dev node bootstraps a dev-wallet WASM at each
`dev-N` address and initializes auth data before `deploy` / `call` can use
those keys. hogd-dev's current scheduler genesis RPC accepts
`GenesisConfiguration`, whose `InitialConfigurationEntry` describes preset
deployment claims by `(contract_hash, nonce, entrypoint, calldata, amount)` but
does not carry the deployed contract bytes needed to construct or unfurl those
claims. That is an XDR/process-contract shape issue, not just missing Rust
plumbing.

Follow-up shape to settle with the team:

- either extend the hogd-dev process contract so Rust can pass dev-wallet
  contract bytes plus deterministic dev-account initialization data, or
- make hogd-dev own a built-in dev-wallet bootstrap mode and report the
  resulting accounts in readiness/control metadata.

Protocol alignment note (2026-05-02): latest `hogd`/`protocol` have moved to
the simplified protocol-rs 0.10 auth shape: `Transaction.auth_claim` is a single
claim, `DecoratedSignature` no longer carries an auth-claim index, and wallet
actions should use `kSourceAddressAuthData { allowed_keys: 0x01,
required_weight: 1 }` rather than the old invoked-address-signature claim.
`groundhog-rpc` updates the hogd-dev supervisor, JSON-RPC validator, CLI
signing, and gateway-facing protocol crates to 0.10. The legacy native-exec
dev-node remains pinned to SDK/native-exec 0.13 and protocol-rs 0.9.x in the
published registry, so it has been removed from the workspace and is no longer
wired into `groundhog node`. Keep `crates/dev-node/` only as reference material
unless we explicitly revive it behind a separate internal feature after the SDK
0.14 crate family is published.

Do not treat the hogd-dev local-node path as complete until the real-binary
smoke reaches:
`groundhog new` -> `build` -> `deploy` -> receipt -> `call`.

### Typed Submit Follow-Up

**Status:** Cross-repo design track. Independent of PR 4c. The temporary
Rust-side behavior maps scheduler submit RPC failure to backend unavailable
because the current generated client exposes `submit_unvetted` as a `void`
RPC and collapses semantic scheduler errors into generic x23 failure.

Desired protocol shape:

- Return a typed batch result from scheduler submit instead of `void`.
- Preserve per-transaction outcomes, aligned with the submitted batch order,
  because the gateway batches multiple client submissions into one scheduler
  call and hogd already verifies auth per transaction.
- Distinguish at least accepted, duplicate/idempotent success, permanent
  rejection, retryable/backpressure, malformed batch, and internal failure.
- Carry enough rejection detail to map JSON-RPC errors without treating
  permanent invalid transactions as retryable backend outages.

Gateway impact:

- `SubmitSink::submit` cannot stay `Result<(), GatewayError>` once the
  scheduler returns per-transaction outcomes. It needs a per-item result shape
  so `BatchSubmitter` can fan out mixed accepted/rejected results accurately.
- Duplicate should likely be treated as idempotent success for
  `groundhog_sendRawTransaction`, returning the canonical hash.
- Permanent auth rejection should map to `TxRejected` / `TxInvalid`, while
  retryable scheduler conditions should remain retryable backend errors.
- A scheduler result count mismatch is a backend/protocol bug and should map
  to backend unavailable.

Rollout order:

- XDR/protocol PR to change or version the scheduler submit method.
- hogd PR with C++ smoke coverage for accepted, mixed accepted/rejected,
  malformed, and duplicate cases.
- protocol-rs regeneration.
- groundhog-rpc PR updating supervisor submit mapping and gateway batch fanout.

### PR 5 - Dev Control and Local Loop Completion

Once hogd-dev can cover the current local development loop:

- extract `DevControl` into `groundhog-rpc-server`, now that there is a second
  consumer;
- wire `groundhog_dev_*` to hogd-dev control/read APIs;
- add reset and transaction detail support;
- snapshot/revert/time travel are not required for local-loop completion unless
  product requirements change;
- keep the native-exec prototype out of the `groundhog node` build.

Hogd-dev local-loop completion requires the normal local development loop used
by the CLI docs: start node, list deterministic dev accounts, deploy,
call/query, submit transactions, poll status/receipt, read emitted events,
reset state, and inspect transaction detail. `getStorageAt` should either work
end-to-end or be explicitly removed/deferred from the public local-node promise.

## Testing Strategy

- Unit-test process supervision with short-lived fake commands.
- Unit-test readiness parsing without launching hogd-dev.
- Add integration tests that launch a fake hogd-dev and the local JSON-RPC
  facade.
- Extend `crates/e2e-tests` to run against local-node once real hogd-dev is
  available.
- Keep `rpc-server` conformance tests backend-agnostic. Do not try to import
  `crates/rpc-server/tests` from the supervisor crate. For adapter conformance,
  add `crates/node-supervisor/tests/conformance_node_supervisor.rs` that reuses
  the same test pattern against fake hogd-dev clients. For whole-pipe coverage,
  use `crates/e2e-tests` with a launched hogd-dev fixture.

## Open Questions

1. ~~Which Rust x23 client crate should node-supervisor depend on for scheduler/read
   calls: chuckd-rs, a new shared crate, or local groundhog-rpc code?~~
   **Resolved 2026-04-29:** node-supervisor depends directly on the codegen
   x23 client crates over `rpc_driver`, matching gateway. See
   [`../specs/2026-04-29-node-supervisor-x23-client-boundary.md`](../specs/2026-04-29-node-supervisor-x23-client-boundary.md).
   The shared transport-wiring helper now lives in `crates/x23-transport`.
2. Is `getStorageAt` required for hogd-dev local-loop completion, or should it be
   explicitly deferred from the first hogd-dev local-node milestone?

## Recommended Next Action

Close PR 4d's Rust-side block driver, then settle dev-wallet genesis/bootstrap
shape before promising the default CLI local loop against hogd-dev.

Concrete next steps:

1. Land the supervisor-owned block driver with fake hogd-dev coverage.
2. Use the real-binary smoke result as the handoff point for bootstrap work:
   submits wake nominate/commit and `chainTip` advances, while `dev-0` deploys
   still need dev-wallet auth/bootstrap support before receipts appear.
3. Decide the dev-wallet genesis shape with hogd/protocol owners. Prefer a
   real process/XDR shape over Rust-side ad hoc reconstruction from generated
   artifacts.
4. After dev-wallet bootstrap lands, rerun the normal local loop:
   `groundhog new` -> `build` -> `deploy` -> receipt -> `call`.
5. Typed submit stays deferred until this real local-node path is proven end to
   end; otherwise we risk designing the typed-submit shape against incomplete
   topology assumptions.
