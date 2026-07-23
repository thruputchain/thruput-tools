# Positive Event Indexing Scenarios

## Goal

Add a small state-changing contract workload so the simulator can emit real
contract events during scheduled load and then observe them through
`groundhog_getEvents`.

This builds on the Fibonacci `events-query` smoke, which intentionally accepted
empty event pages. The new scenario uses the already-deployed Fibonacci
contract's event-emitting `compute` method to prove positive event indexing
without introducing multi-contract or per-user protocol state yet.

## Scope

- Add manifest field `contractCalls`.
  - `name`, unique within `contractCalls`;
  - `contract`, referencing `contractArtifacts[].name`;
  - `entrypoint`, exactly 8 bytes of hex;
  - `calldata`, raw CXS calldata hex.
  - exactly one entry for this slice; multi-call selection policy is deferred.
- Add workload kind `contract-call`.
- Prepare contract-call targets after successful startup deployment by resolving
  manifest-local contract names to deployed addresses.
- Execute `contract-call` through the dev-wallet `doAction` path with the
  scheduled actor's signer and nonce.
- Wait synchronously for the transaction receipt before completing the workload.
- Record JSONL workload fields for contract-call name, contract address, tx
  hash, nonce, receipt status, receipt height, and submit-to-receipt latency.
- Add local catalog scenario `local-fibonacci-contract-call-events-smoke`.

## Out Of Scope

- ABI/spec-driven argument encoding in simulator manifests.
- Topic filters and event payload assertions.
- Contract-call selection policies beyond the first configured call.
- Startup contract calls outside scheduled load.
- Multi-contract/cross-contract event provenance scenarios.
- Per-user wallet behavior beyond existing actor nonce ownership.

## Semantics

1. Startup contract deployment is unchanged.
2. Contract setup prepares configured `contractCalls` only after all deployments
   complete successfully.
3. For each scheduled `contract-call` tick, the executor uses the first prepared
   call:
   - outer invoked address: actor wallet;
   - outer entrypoint: runtime dev-wallet `doAction`;
   - inner target: deployed contract address;
   - inner entrypoint/calldata: manifest raw hex.
4. A successful submit records the actor's submitted transaction and consumes the
   actor nonce.
5. The workload polls `groundhog_getTransactionReceipt` with the existing bounded
   receipt-drain timeout and retry interval.
6. Receipt `ok` completes the workload and records the observed receipt.
7. Non-`ok` receipt status, submit RPC failure, or receipt polling failure records
   a failed workload.

## Ordering Guarantees

The positive event observation depends on actor-local serial execution, not only
on the schedule shape. The scheduler emits a `contract-call` tick before a later
`events-query` tick, and the executor's per-actor in-flight limit keeps a single
actor from starting the later tick while the earlier contract call is still
polling its receipt. Because `contract-call` completes only after an `ok`
receipt is observed, a later same-actor `events-query` can observe the indexed
effects from that call.

The local smoke asserts this artifact ordering directly: `result.jsonl` must
contain a completed `contract-call` before an `events-query` for the Fibonacci
query reports `eventCount > 0`.

## Artifact Contract

`result.jsonl` `workload-completed` / `workload-failed` records for
`contract-call` include additive optional fields:

- `contractCallName`;
- `contractCallContract`;
- `txHash`;
- `txNonce`;
- `receiptPresent`;
- `receiptStatus`;
- `receiptStatusDetail`, when present;
- `submitToReceiptLatencyMs`, when a receipt is observed;
- `chainHeight`, when a receipt is observed.

No `result.json` schema bump is required. Per-workload metrics are keyed by
`contract-call` like other workloads.

## Scenarios

The local scenario builds `fibonacci.wasm` from `groundhog-examples`, deploys
it, runs a mix of `contract-call` and `events-query`, and keeps
`eventQueries[].expectMinEvents` at `0` so early pre-emission queries do not
fail the run. The gated smoke asserts at least one later `events-query` record
observes `eventCount > 0`.

The scenario uses the same raw Fibonacci entrypoint and calldata as the
`view-call` smoke:

- `compute`: `0x1450686bab1d6a5f`, calldata `0x0a`.

## Test Plan

- Unit tests:
  - contract-call manifest validation accepts valid references and raw hex;
  - validation rejects unknown contract references, short entrypoints, and
    missing `contractCalls`;
  - contract setup prepares deployed contract-call targets;
  - contract-call transaction construction does not consume actor nonce;
  - executor submits, polls receipt, updates actor state, and records artifacts;
  - non-`ok` receipt status records a failed contract-call workload;
  - JSONL contract-call shape serializes expected additive fields.
- Gated smoke:
  - extend scenario smoke to run the positive Fibonacci event scenario;
  - assert completed contract-call artifacts;
  - assert at least one events-query record observes indexed events.

## Review Gates

- Slice K1: manifest/contract setup review for validation and raw-call
  semantics.
- Slice K2: executor review for nonce ownership, receipt handling, and artifact
  shape.
- Slice K3: scenario/smoke review for deterministic positive event observation
  and catalog UX.
