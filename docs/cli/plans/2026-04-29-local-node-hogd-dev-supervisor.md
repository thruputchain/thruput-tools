# Local Node Hogd-Dev Supervisor Implementation Plan

**Date:** 2026-04-29
**Status:** Implementation in progress. PRs 1–4a shipped; PR 4b in progress;
PRs 4c/5 ahead.

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
and a temporary comparison harness, not as a supported long-term backend family.

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

`crates/dev-node` remains in the tree as the native-exec prototype until
hogd-dev covers the basic deploy/call/detail loop. After that, remove it from
the CLI path or delete it.

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

`groundhog-node-supervisor` should consume `groundhog-rpc-server` directly:

```text
GroundhogRpcServer<SubmitClient, ChainClient, ReadClient>
```

Likely v1 mapping:

- `SubmitPlane::submit_raw_transaction` -> scheduler/submit endpoint.
- `ChainPlane::chain_tip` -> scheduler/readiness endpoint.
- `ChainPlane::pending_transaction_count` -> real hogd-dev mempool count. If
  hogd-dev cannot expose this, do not pretend parity; return `NotImplemented`
  until the scheduler API exists.
- `ChainPlane::get_block_by_number` -> read endpoint once hogd-dev can answer.
- `ReadPlane::transaction_receipt` -> read endpoint.
- `ReadPlane::transaction_status` -> composite read endpoint, or a local
  adapter that queries scheduler pending state then committed read state.
- `ReadPlane::contract_events` -> read endpoint.

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
- Keep native-exec path available only if needed for comparison, but make the
  CLI text clear that native-exec is legacy prototype infrastructure.
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

**Status:** In progress. The current Rust-side path can use
`schedulercoord_submit_unvetted(ParallelSignedTransactionBatch)` on the
advertised scheduler endpoint. Typed rejection mapping is limited by the
current x23/protocol-rs shape; see protocol notes below.

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

- Hoist backend plane-status reporting into a typed `PlaneStatus` /
  `BackendPlanes` shape owned by `groundhog-node-supervisor` before changing
  the CLI read-plane strings again.
- Implement receipts, events, block lookup, and transaction status.
- If hogd-dev has separate pending and committed sources, make the supervisor
  read adapter own the composite status flow: scheduler pending state first,
  committed read state second.

Acceptance:

- unknown tx status/receipt behavior matches conformance expectations;
- current deploy/call loop either works or has a documented blocker.

### PR 5 - Dev Control and Prototype Retirement

Once hogd-dev can cover the current native-exec loop:

- extract `DevControl` into `groundhog-rpc-server`, now that there is a second
  consumer;
- wire `groundhog_dev_*` to hogd-dev control/read APIs;
- add reset and transaction detail support;
- snapshot/revert/time travel are not required for native-exec retirement unless
  product requirements change;
- remove native-exec from `groundhog node`, or leave it behind an explicit
  internal/test-only feature.

Native-exec retirement requires hogd-dev to pass the normal local development
loop used by the CLI docs: start node, list deterministic dev accounts, deploy,
call/query, submit transactions, poll status/receipt, read emitted events, reset
state, and inspect transaction detail. `getStorageAt` should either work
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
2. Is `getStorageAt` required for native-exec retirement, or should it be
   explicitly deferred from the first hogd-dev local-node milestone?

## Recommended Next Action

Validate the `SubmitPlane` wiring against a real hogd-dev binary, not only the
fake scheduler fixture. If real semantic rejection still reaches Rust as a
generic x23 `SystemError`, agree a typed protocol-rs submit result before
depending on fine-grained `TxRejected`/retryable error behavior. Once accepted
submit is proven end to end, move on to PR 4c read/status coverage.
