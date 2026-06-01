# Actor State Machine + Read-After-Write Scenarios (Phase 3, Slice L) - Design

**Date:** 2026-05-31
**Status:** L1 signed off. State-machine semantics, fixture, submit-failure
branch, and view-only read decoding finalized per review. v1 read mode is
view-only; storage read mode is deferred. **Complete (L1-L4).** Contract fixture,
manifest, validation, `ReadAfterWriteState`, contract setup, executor round-trip,
scenario, and smoke all landed; the live smoke confirmed advancing round-trips
and surfaced + fixed an hogd ViewGateway staleness bug. End-to-end reviewer-fleet
findings addressed. Shipped across the `sim-read-after-write` (groundhog-rpc),
`caller-counter-fixture` (groundhog-examples), and `viewgateway-refresh-on-tip-height`
(hogd) branches.

## Goal

Give an actor a small, deterministic state machine that tracks what it knows it
wrote, and add a `read-after-write` workload preset that writes a per-actor
value, waits for the write to commit, reads it back, and asserts the round-trip.

This is the first workload whose *read* outcome depends on the actor's own
*prior write* rather than on static scenario configuration or incidental
schedule ordering. It satisfies both remaining Phase 3 bullets ("richer actor
state machines", "read-after-write scenario presets") with one coupled
capability.

Every contract workload to date (`view-call`, `storage-read`, `events-query`,
`contract-call`) targets the first configured entry and reads chain state
independent of the scheduled actor's history. Read-after-write is the deliberate
next step toward the richer per-actor flows future "user agent" scenarios will
build on, without committing to a manifest-level multi-step flow DSL before two
or more real flows exist to validate its shape.

## Design Decision: Coupled Preset, Not a Flow DSL

The reusable substrate future richer interactions need is the **per-actor
state-machine primitive** — an actor that carries a typed, reconcilable
expectation across ticks and a workload that reads it and asserts. It is *not* a
manifest `flows`/`sequences` construct. A flow DSL is a wire-shaped abstraction;
building it from one example locks scenario authors into a shape guessed from a
single use case and forces workload selection to become stateful, discarding the
deterministic-replay property the simulator depends on.

Therefore:

- Workload selection stays **pure** (the existing seeded weighted hash).
- The reconcile → write → await → read → assert sequence is **internal to one
  `read-after-write` tick**. No new scheduler concepts.
- The actor's expectation is the only new persistent state and is **private
  library state**, free to refactor until the public `groundhog-sim` API is
  frozen (tracked separately).

When a second and third genuinely different flow appear, the general construct
grows out of this primitive instead of being speculated up front.

## Fixture Decision (approved at L1)

A meaningful read-after-write needs a **write-dependent read**: the read must
return something different *because* the write committed. The constraints:

- Fibonacci `compute(N)` is **pure** — `view(compute, N)` returns the same bytes
  whether or not any transaction ran, so a view read-back proves nothing.
- Fibonacci's committed storage key is **undocumented in this repo**.
- An events read-back duplicates Slice K and is not a state round-trip.

**Decision: add a minimal per-caller counter contract to `groundhog-examples`**
(checked out locally at `../groundhog-examples`, a workspace with `fibonacci`,
`events`, `cross-call`, etc.). The SDK already ships a `counter` example, but it
is a **single global cell** (`Counter { value: NNInt }`) — under multi-actor
load every actor would race on one slot, breaking both per-actor isolation and
the deterministic `+1` invariant the state machine relies on. The new fixture
keys the count by `get_caller()` (the SDK's `GetMsgSender`):

- `bump()` — no-arg write; `count[get_caller()] += 1` (empty calldata avoids any
  argument encoding, consistent with every prior slice's no-ABI rule).
- `get() -> u64` — view returning `count[get_caller()]` (0 when absent), giving a
  clean reconciliation pre-read for the uncertain case below.

We do **not** spend L2 trying to make fibonacci storage work first; the counter
is the behavior we actually need. Fibonacci-storage is a fallback only if the
cross-repo fixture is somehow blocked, which it is not.

### Caller identity (explicit L2 verification)

The round-trip is only coherent if the write and the read observe the **same**
`get_caller()` slot. Both must resolve to `actor.wallet_address`:

- Write: a dev-wallet `doAction` whose inner call invokes `bump()`. From the
  counter's view, `get_caller()` is the immediate caller — the wallet contract,
  i.e. `actor.wallet_address`.
- Read: `groundhog_view` with `caller = actor.wallet_address` (the existing
  `view-call` workload already passes this).

L2 must verify the inner-call `msg.sender` is in fact the wallet contract
address (not the signing account or some sub-identity) before trusting the
round-trip; if it is not, the read caller must be set to whatever the inner call
actually presents. This is the first concrete L2 task.

## Actor State Machine

The naive "advance only on whole-workload success" rule is wrong: an `ok` write
followed by a failed read-back RPC commits the chain `+1` while the actor's
expectation stays put, so the next tick false-fails forever. The fix is to
**rebase the actor's belief at the moment the write is known committed**, and to
make unknown commit status an explicit, self-healing state.

```rust
// private ActorState field
read_after_write: ReadAfterWriteState,

enum ReadAfterWriteState {
    /// No trusted baseline yet (first tick, or after an uncertain outcome).
    /// The next tick pre-reads to rebase before writing.
    NeedsReconcile,
    /// We know the actor's committed counter value.
    Known(u64),
}
```

Per-caller keying makes the increment **provably** `+1` from the actor's own
last committed value — no other writer touches this slot, and the per-actor
in-flight=1 limit serializes the actor's own writes — so `Known(v)` after a
committed `bump()` is exactly `Known(v + 1)` without needing to read.

A future storage read mode (see Deferred: storage read mode) would carry a
`Known { key, value }` payload instead; the state-machine shape is identical, so
it is additive.

## Execution Semantics

For each scheduled `read-after-write` tick, using the first prepared entry:

1. **Reconcile (only if `NeedsReconcile`):** pre-read `get()`. On any read
   failure (see Read decoding) the tick fails and state stays `NeedsReconcile`;
   nothing is written. On success, baseline := observed; `expected := baseline + 1`.
   When `Known(v)`, skip the pre-read; `expected := v + 1`.
2. **Build the write:** a dev-wallet `doAction` wrapping `bump()` against the
   deployed counter, using the actor's signer and next namespaced nonce (reuse
   `build_contract_call_transaction`).
3. **Submit** `groundhog_sendRawTransaction`:
   - **Submit RPC failure (no tx hash returned):** `workload-failed`,
     `errorClass="rpc-error"`; there is no commit status, state is **unchanged**,
     and the actor nonce is **not** advanced — identical to `wallet-submit-smoke`
     and `contract-call`, so the same namespaced nonce is retried on the next
     tick. Skip await and read.
   - **Submit succeeds:** record the submitted transaction (this is what advances
     the actor nonce), then continue.
4. **Await:** poll `groundhog_getTransactionReceipt` with the existing bounded
   drain timeout/retry interval.
5. **Branch on the write's commit status — this is where state advances:**
   - **`ok` receipt → write is known committed.** Set `Known(expected)`
     immediately, *before* reading. The committed value is provably `expected`.
     Then read-back `get()`:
     - observed == expected → `workload-completed` (`readAfterWriteMatched=true`).
     - observed != expected → `workload-failed`,
       `errorClass="read-after-write-mismatch"`, and set `NeedsReconcile` (a real
       discrepancy; re-read and rebase next tick rather than cascade false-fails).
     - read failure (RPC error, abort, fault, or decode failure; see Read
       decoding) → `workload-failed` with the matching error class, but keep
       `Known(expected)` — the write committed; only the observation failed, so
       the actor model is **not** left behind.
   - **non-`ok` receipt status → write did not take effect.** `workload-failed`,
     `errorClass="contract-call-receipt-status"`; state **unchanged** (no
     advance). This relies on Groundhog commit atomicity: a non-`ok` receipt is
     assumed to roll back all target state from the call, so "no advance" is
     correct.
   - **receipt unknown** (poll RPC error/timeout): `workload-failed`,
     `errorClass="rpc-error"`; set `NeedsReconcile` — commit status is unknown,
     so rebase by pre-read next tick.
6. **Restart during the write:** the existing chaos path aborts the in-flight
   task and advances the uncertain submit nonce; additionally set
   `NeedsReconcile` so the actor rebases on its next tick. No new chaos behavior.

The whole sequence runs in one workload task, so the per-actor in-flight=1 limit
already serializes reconcile/write/read; no actor starts a second write before
the first round-trip resolves.

### Read decoding

Both the reconcile pre-read and the post-commit read-back call `get()`, which
returns a CXS-encoded `u64`. A read is **successful** only when the view output
is `RootInvocationOutput::Return { return_data }` *and* `return_data` CXS-decodes
to a `u64`. `RootInvocationOutput::Abort`, `RootInvocationOutput::Fault`, a view
RPC error, and a CXS decode failure are all **read failures**, with error classes
`view-aborted`, `view-fault`, `view-rpc-error`, and `read-decode-error`
respectively.

State effect is determined by *where* the read failed, not by the class:

- **Post-commit read** (after an `ok` write): keep `Known(expected)` — the write
  is provably committed; only the observation failed.
- **Reconcile pre-read** (`NeedsReconcile` path, before writing): stay
  `NeedsReconcile`; nothing is written this tick.

## Determinism and Ordering Guarantees

- Selection stays pure; an actor's expectation is a deterministic function of how
  many `read-after-write` ticks it has committed, itself fixed by the seeded
  schedule.
- Per-caller keying isolates actors and makes the `+1` rebase provable, so
  concurrent actors stay independent and the run is replayable.
- `NeedsReconcile` makes recovery deterministic: the rebasing pre-read observes
  committed chain state, so a transient failure or restart converges instead of
  desyncing.

## Artifact Contract

`result.jsonl` `workload-completed` / `workload-failed` records for
`read-after-write` add additive optional fields:

- `readAfterWriteName`, `readAfterWriteContract`;
- `txHash`, `txNonce` (the write);
- `receiptPresent`, `receiptStatus`, `receiptStatusDetail`;
- `submitToReceiptLatencyMs`, `chainHeight` (write commit);
- `readAfterWriteExpected` (decimal string);
- `readAfterWriteObserved` (decimal string; absent when the read-back RPC failed
  or was a non-`ok`/unknown write);
- `readAfterWriteMatched` (bool);
- `readAfterWriteRebased` (bool; true when this tick performed a reconcile
  pre-read).

`result.json` keys per-workload metrics by `read-after-write` like other
workloads. **No schema bump:** a mismatch is a failed workload already counted
toward `maxErrorRate`. A dedicated `maxReadAfterWriteMismatchRate` assertion is
**deferred** until a scenario needs to gate mismatches separately from generic
errors.

## Validation

- Reject duplicate `readAfterWrite[].name`.
- Reject unknown `contract` references.
- Reject non-`0x` or non-8-byte write/read entrypoints.
- Reject `read-after-write` workloads with no contract artifact or no
  `readAfterWrite` entry.
- Require capabilities: the view-only read needs the view gateway, and the write
  path needs the submit + indexer planes. `read-after-write` can reuse a single
  `WorkloadKind` capability requirement (view gateway + indexer) because v1 has
  no storage read mode to distinguish.

## Manifest Shape

Add `readAfterWrite: Vec<ReadAfterWriteSpec>` and workload kind
`read-after-write`. v1 supports exactly one configured entry (matching every
prior contract workload's first-entry selection); multi-entry selection is
deferred.

```json
{
  "workloads": [{ "name": "read-after-write", "weight": 1 }],
  "contractArtifacts": [{ "name": "caller-counter", "wasm": "caller-counter.wasm" }],
  "readAfterWrite": [
    {
      "name": "counter-roundtrip",
      "contract": "caller-counter",
      "write": { "entrypoint": "0x<bump-selector>" },
      "read":  { "entrypoint": "0x<get-selector>" }
    }
  ]
}
```

`RawWriteSpec` carries the 8-byte write entrypoint and optional fixed calldata
(empty for `bump()`). `RawReadSpec` carries the 8-byte view entrypoint and
optional fixed calldata. No per-write calldata templating in v1.

### Deferred: storage read mode

v1 reads are **view-only**. A storage read-back (`groundhog_getStorageAt`) is
deliberately omitted rather than reserved as unused surface: it needs an exact,
documented committed-storage key shape that the counter fixture does not require.
A future additive `read.mode` enum (`view` default, `storage` with a defined key
derivation) can be added without breaking v1, when a scenario actually needs it.

## Scenarios

- `examples/sim/local-counter-read-after-write-smoke.json`: deploy the per-caller
  counter, run a pure `read-after-write` load over a few seconds, assert
  `maxErrorRate` near zero. A multi-actor variant is a natural follow-up once the
  single-actor smoke is green.
- Add a `catalog.json` entry tagged `contract` + `read-after-write`.
- Extend `scripts/smoke-sim-scenarios.sh` to build the counter fixture (alongside
  fibonacci) and run the gated smoke; assert at least one completed
  `read-after-write` record with `readAfterWriteMatched: true` and that
  `readAfterWriteExpected` advances across records.

## Test Plan

- Unit tests:
  - manifest validation accepts a valid entry; rejects duplicate names, unknown
    contracts, short entrypoints, and missing entries;
  - capability validation requires the view gateway (read) and submit/indexer
    planes (write);
  - submit RPC failure records `rpc-error`, does not advance the nonce, and
    leaves state unchanged;
  - read decoding: a non-`Return` view output or a non-`u64` payload is a read
    failure;
  - state machine: `Known(v)` + committed write → `Known(v+1)` without a pre-read;
  - `NeedsReconcile` performs a pre-read and rebases before writing;
  - committed write + read RPC failure keeps `Known(expected)` (no desync) and
    records `rpc-error`;
  - read mismatch records `read-after-write-mismatch` and sets `NeedsReconcile`;
  - non-`ok` receipt does not advance; receipt-unknown sets `NeedsReconcile`;
  - replays are stable across identical seeds;
  - contract setup prepares the target from a deployed address;
  - JSONL shape serializes the additive fields.
- Gated smoke: deterministic positive round-trip over the counter fixture.

## Slices and Review Gates

- **Slice L1 (this doc):** architecture + state-machine semantics + fixture
  decision. *Revised per review.*
- **Slice L2 (done):**
  1. Added the per-caller counter contract to `../groundhog-examples`; verified
     write/read caller identity both resolve to `actor.wallet_address` at the code
     level (the live `groundhog_view` caller check is on the L4 smoke).
  2. Added manifest `readAfterWrite` + `read-after-write` workload kind and
     validation.
  3. Added the `ReadAfterWriteState` machine on `ActorState`.
  *Review gate L2 passed:* caller-identity verification, state-machine
  determinism, manifest shape, validation. Contract setup is consumed only by the
  executor, so it moved to L3 to avoid a dead-code field.
- **Slice L3 (done):** contract setup (`PreparedReadAfterWrite`) plus the
  executor reconcile → write → await → read → assert, additive artifacts, and the
  `read-after-write-mismatch` error class. Read-back decodes the CXS `u64`. Six
  mock-client executor tests cover the transition matrix.
  *Review gate L3:* commit-vs-read failure handling, rebase correctness, artifact
  shape.
- **Slice L4:** scenario, catalog entry, smoke coverage (build the fixture), docs.
  *Final review gate:* deterministic round-trip and catalog UX.
