# Local Node Hogd-Dev Supervisor Implementation Plan

**Date:** 2026-04-29
**Status:** PRs 1–4f and the backend plane-status hoist shipped across the
Rust supervisor and hogd-dev path. Published-SDK scaffold compatibility was
verified on 2026-05-07 against `sdk = 0.14.0`. PR 5 follows.

**Companion to:**

- [`../2026-04-25-hogd-dev-node-hybrid.md`](../2026-04-25-hogd-dev-node-hybrid.md)
  - durable hogd-owned subprocess architecture.
- [`../specs/2026-04-29-hogd-dev-process-contract.md`](../specs/2026-04-29-hogd-dev-process-contract.md)
  - draft hogd-dev subprocess contract proposal.
- [`2026-04-30-hogd-dev-binary-bootstrap.md`](2026-04-30-hogd-dev-binary-bootstrap.md)
  - hogd-side bootstrap plan for the smallest credible `hogd-dev` v1
    (gating PR 4a).
- [`../specs/2026-05-03-dev-account-bootstrap.md`](../specs/2026-05-03-dev-account-bootstrap.md)
  - dev-account bootstrap implementation contract for PR 4e: hogd-owned
    bootstrap artifact/profile, deterministic signer metadata, wallet-address
    readiness, and the Rust supervisor/CLI consumer work.
- [`../specs/2026-05-07-view-rpc-and-receipt-return-data.md`](../specs/2026-05-07-view-rpc-and-receipt-return-data.md)
  - read DX follow-ups (receipt return-data + view RPC) running as a
    parallel cross-team track to PR 5. Rust scaffolding waits on
    hogd/protocol design sign-off and protocol-rs publication.

## Goal

Create a fresh Rust crate, `crates/node-supervisor`
(`groundhog-node-supervisor`), that becomes the CLI-facing supervisor/facade
for `groundhog node`.

Use "local node" for the user-facing product shape, not for the crate name.
The crate is the supervisor; naming it that avoids confusion with "the local
node" as the running system. The legacy `crates/dev-node` native-exec
prototype was removed from the tree in PR 5.

The durable backend target is a hogd-owned `hogd-dev` subprocess. The
native-exec path was treated as legacy prototype code through PR 4 and
deleted once hogd-dev proved out the published-SDK scaffold loop.

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

The legacy `crates/dev-node` native-exec prototype was deleted in PR 5.
The git history retains the prototype source for any future reference.

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

**Status:** Shipped in PR #47. The first real-binary smoke after the read
endpoint landed showed that `groundhog_sendRawTransaction` can reach hogd-dev's
scheduler, but the local topology had no chuckd coordinator to drive
`schedulercoord_nominate` / `schedulercoord_commit_block`. Result before this
PR: deploy returned a tx hash, receipt polling timed out, status stayed
unknown, and `chainTip` remained at genesis.

Design choice: `groundhog node`, not `deploy` or `call`, owns this behavior.
Client commands should remain ordinary JSON-RPC clients. The local-node
supervisor supplies the missing single-node coordination policy so all clients
see the same semantics.

Implementation shape (as shipped):

- supervisor-owned coalescing block-driver task in `node-supervisor`;
- driver wakes only after `schedulercoord_submit_unvetted` succeeds;
- one wake drives `schedcoord_get_tip` -> `schedulercoord_nominate` ->
  `schedulercoord_commit_block`;
- `sendRawTransaction` semantics stay "accepted by the local scheduler
  submit path"; submit does not wait for commit;
- driver stops before hogd-dev stops.

Block-driver tests use fake hogd-dev automine mode to prove a successful submit
wakes the driver and advances `chainTip`. A follow-up real-binary smoke against
a rebuilt hogd-dev confirmed the same block-production signal: a deploy submit
still timed out waiting for a receipt, but `chainTip` advanced from height 0 to
height 1. The submitted tx remained `unknown`, so the remaining blocker is
dev-wallet/genesis/auth bootstrap, tracked as PR 4e below.

### PR 4e - Dev-Wallet/Genesis/Auth Bootstrap

**Status:** Shipped. Implementation contract:
[`../specs/2026-05-03-dev-account-bootstrap.md`](../specs/2026-05-03-dev-account-bootstrap.md).
The process path is live through hogd-dev startup, dev-account discovery,
submit, block production, read/indexer receipt polling, and deploy. The
remaining default-loop gap is the published SDK/template compatibility tracked
in PR 4f below.

The current supervisor genesis is empty. The legacy native-exec dev node used
an in-process shortcut: install dev-wallet WASM at each `dev-N` pubkey-derived
address, initialize auth data, then let `deploy` / `call` invoke wallet
entrypoints at that address. hogd-dev cannot reproduce that by merely loading
the current empty `GenesisConfiguration`.

Important correction from the PR 4e spec review: protocol-rs 0.10
source-address auth is necessary but not sufficient. It authorizes the invoked
wallet address; it does not remove the need for wallet code, a deploy
entrypoint, or call return-value forwarding. PR 4e should therefore bootstrap
real wallet accounts rather than planting only auth data.

#### Design Constraint

The immediate product remains `groundhog node`: one CLI command supervises
hogd-dev and exposes the JSON-RPC surface. But the bootstrap contract should
also leave room for a future simulation or bench rig that runs the real gateway
against a real hogd process suite. Avoid a shape that only works because the
Rust supervisor privately knows how to reconstruct dev wallets.

Weight candidate shapes by:

- reproducibility from explicit input data;
- observability of resulting accounts and auth state;
- compatibility with hogd/chuckd-style genesis loading;
- whether the same data can feed hogd-dev and a future real-process rig;
- minimal special cases in gateway and public JSON-RPC paths.

Shipped direction for PR 4e:

- Define a hogd-owned bootstrap profile/artifact: `GenesisConfiguration`, vetted
  initialization transaction batches, dev-wallet contract bytes/hash, and public
  account metadata.
- Let hogd-dev apply that artifact internally for the v1 local-node path, or
  expose enough of it for the supervisor to load and pump if that matches the
  C++ APIs better.
- Treat account address and signer identity as separate fields. The signer key
  stays deterministic; the account address is the wallet contract address that
  hogd actually deployed and initialized.
- Teach the Rust supervisor/CLI to consume advertised account metadata through
  `groundhog_dev_getAccounts`, instead of locally assuming
  `PubkeyAccount(public_key, DEFAULT_PUBKEY_ACCOUNT_NONCE)` is the wallet
  address.
- Wire `groundhog_dev_getAccounts` in PR 4e, not PR 5. The rpc-api trait and
  `DevAccount` type already exist, but node-supervisor must implement and merge
  the dev RPC module so `deploy` / `call` can resolve `dev-N` before signing.
- Treat missing `groundhog_dev_getAccounts` as a clear local-dev-node error in
  CLI deploy/call paths. Do not silently fall back to pubkey-derived addresses.
- Use hogd-dev readiness as the source of truth for wallet address, signer
  public key, and funded storage-token balance. The supervisor derives private
  keys locally only for the well-known dev accounts.

Rejected or deferred shapes:

- Avoid an ad hoc Rust-only shim except as a deliberately short-lived
  diagnostic. It would unblock the localhost loop quickly, but it would not
  teach us the durable protocol or process shape.
- If preserving pubkey-derived wallet addresses is a hard product requirement,
  first add an explicit process/XDR surface for arbitrary account deployment,
  auth data, and funding. Do not revive the native-exec shortcut in Rust.
- Do not add a `--coord-mode external` flag just to keep a theoretical rig path
  open. The future rig should reuse the bootstrap artifact with real
  loadgen/chuckd-style init pumping.

Protocol alignment note (2026-05-02): latest `hogd`/`protocol` have moved to
the simplified protocol-rs 0.10 auth shape: `Transaction.auth_claim` is a single
claim, `DecoratedSignature` no longer carries an auth-claim index, and wallet
actions should use `kSourceAddressAuthData { allowed_keys: 0x01,
required_weight: 1 }` rather than the old invoked-address-signature claim.
`groundhog-rpc` updates the hogd-dev supervisor, JSON-RPC validator, CLI
signing, and gateway-facing protocol crates to 0.10. The legacy native-exec
dev-node was pinned to SDK/native-exec 0.13 and protocol-rs 0.9.x; it was
removed from the workspace at PR 4f and the source tree was deleted in PR 5.
The git history retains the prototype source if it is ever needed for
reference.

#### Acceptance

The PR4 hogd-dev local-node path is solid once the real-binary smoke reaches
the normal user loop against a published SDK crate:
`groundhog new` -> `build` -> `deploy` -> receipt -> `call`.

Post-merge smoke on 2026-05-05 showed:

- a fresh hogd-dev rebuild starts with matching protocol
  `hogp/830b7810ee93`;
- `groundhog node --hogd-dev-binary <path> --json` advertises submit and chain
  as `scheduler` and read as `indexer`;
- `groundhog_dev_getAccounts` returns deterministic wallet metadata;
- `groundhog deploy` signed as `dev-0` commits and is visible through the read
  plane;
- the current `groundhog new` template still pins `sdk = 0.11.4`, which
  deploys but produces deterministic call failures against current hogd-dev;
- latest published `sdk = 0.13.0` is not a viable replacement because its
  syscall ABI does not compile under normal dependency resolution when Cargo
  selects published `groundhog-xdr 0.9.4`.

A local `sdk_rs` checkout is useful to diagnose the contract path only after it
has been bumped to the same protocol crates as hogd-dev. As of the 2026-05-06
recheck, the local checkout resolves `groundhog-xdr 0.10.1`
(`hogp/500463c1d97f`), so it is still behind the current hogd-dev protocol
`hogp/830b7810ee93` unless locally patched.

### PR 4f - Published SDK / Scaffold Compatibility

**Status:** Shipped. Verified on 2026-05-07.

Goal: make the default local loop work from a fresh scaffold without path or git
dependency overrides.

Shipped work:

- published a protocol-current SDK crate family whose syscall ABI matches the
  current hogd-dev protocol and built-contract path;
- updated the `groundhog new` template from `sdk = 0.11.4` to that published
  crate;
- used exact pins in the SDK crate set so Cargo does not float
  protocol/XDR crates across incompatible syscall ABI boundaries;
- reran the real-binary smoke from a fresh scaffold:
  `groundhog new` -> `build` -> `node` -> `deploy` -> receipt -> `call`
  (`initialize`, `increment`, `decrement`, `get`);
- kept call return-data surfacing as a separate follow-up because it does not
  block committed calls.

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

### PR 5 - Dev Control Endpoint and Transaction Detail

After PR 4f proved the published-SDK scaffold loop against hogd-dev,
PR 5 starts the durable hogd-dev dev-control surface without treating
runtime clean-slate reset as a required primitive. The product shape was
refined after comparing Anvil/Hardhat, Aptos, NEAR sandbox, and Solana
local-validator workflows: EVM-style full-chain forking makes runtime
reset more useful, but Groundhog is closer to Move/WASM/account-based
systems where the high-leverage controls are transaction detail and
targeted state surgery.

Clean slate is a process-lifecycle concern: restart hogd-dev or add a
`groundhog node --reset` startup flag that clears local dev state before
launch. Do not implement `groundhog_dev_reset` by reaching into
hogd-dev internals across RAMDB, mempool, auth, event, and indexer
state.

**Rust-only slices:**

- Delete `crates/dev-node` entirely. The dev-wallet source there is a
  stale duplicate of the canonical wallet that now lives in hogd at
  `built_contracts/integration_test_contracts/dev_wallet`; the Rust
  workspace consumes the deployed wallet by address and the entrypoint
  constants in `crates/signing/src/build.rs` and `crates/cli/src/call.rs`,
  not the contract source. Native-exec retirement is finished once the
  tree is gone.
- Defer hoisting `dev_control` plumbing into `groundhog-rpc-server`
  until a second consumer actually exists. Today only node-supervisor
  mounts `SupervisorDevRpc`; the gateway should not expose
  `groundhog_dev_*`. Hoisting prematurely creates churn without a
  payoff. Reassess after the hogd-dev control endpoint lands.

**Hogd-dev-only slices (same-owner, no broader hogd team gate):**

- Add a control endpoint to hogd-dev's readiness so the supervisor can
  consume transaction-detail RPCs. `HogdDevEndpoints.control`
  on the Rust side already exists as `Option<SocketAddr>` with
  `serde(default)`; today it deserializes to `None` until hogd-dev
  advertises the endpoint.
- Implement `groundhog_dev_getTransactionDetail`. Cover dev-only debug
  data (per-frame info, traces, gas breakdown) and explicitly do not
  duplicate fields that the read-DX spec's Milestone A places on the
  public `TransactionReceipt` (`return_data`, `abort_data`). Once
  Milestone A lands, this method's surface shrinks, not grows.

**Out of scope for PR 5:**

- Runtime `groundhog_dev_reset`. Clean-slate UX belongs to process
  startup (`groundhog node --reset`) or node restart, not an in-process
  dev-control RPC.
- Snapshot/revert/time travel.
- State-surgery RPCs such as balance/storage/code/account patching.
  These are higher-value dev controls than wipe-everything reset and
  should be designed as a follow-up surface.
- Any reset-driven change to `hogexec`, `hogdb`, `indexer`, auth,
  event, or mempool internals. Broader read/trace retention work goes
  through the read-DX spec.

Hogd-dev local-loop completion requires the normal local development loop used
by the CLI docs: start node, list deterministic dev accounts, deploy,
call/query, submit transactions, poll status/receipt, read emitted events,
start from clean state when requested at launch, and inspect transaction
detail. `getStorageAt` should either work end-to-end or be explicitly
removed/deferred from the public local-node promise.

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
3. ~~Confirm whether local dev account addresses may be hogd-deployed wallet
   addresses rather than pubkey-derived addresses. If not, PR 4e must start
   with the explicit arbitrary-address bootstrap surface.~~
   **Resolved 2026-05-05:** local dev accounts use hogd-deployed wallet
   addresses advertised through readiness and `groundhog_dev_getAccounts`.
4. ~~Choose the exact home for the current-protocol dev-wallet contract bytes
   used by the bootstrap artifact.~~
   **Resolved 2026-05-05:** the dev-wallet contract lives under hogd's
   `built_contracts/integration_test_contracts/dev_wallet` pipeline and is
   embedded as `built_contracts/dev_wallet.h`.

## Recommended Next Action

PR 4f is green. Two parallel tracks are now open.

**This plan (PR 5):**

1. Start PR 5 dev-control work with the narrowed scope: hogd-dev
   advertises a control endpoint, then implements transaction-detail
   data behind it. Keep runtime reset out of the stack; track
   `groundhog node --reset` as a startup-lifecycle follow-up.

**Cross-team read-DX track (separate spec):**

2. Send
   [`../specs/2026-05-07-view-rpc-and-receipt-return-data.md`](../specs/2026-05-07-view-rpc-and-receipt-return-data.md)
   to Geoff/Ryan for hogd-side design sign-off on Milestone A
   (`TxEventLog` root output + indexer migration) and Milestone B
   (top-level read-only execution endpoint). Rust-side scaffolding for
   either milestone waits on hogd implementation and protocol-rs
   publication; this avoids the protocol-skew rework cost the workspace
   has paid before.

**Continued deferrals:**

1. Typed submit stays deferred until this real local-node path is proven end to
   end; otherwise we risk designing the typed-submit shape against incomplete
   topology assumptions.
