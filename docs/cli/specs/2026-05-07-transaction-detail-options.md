# Transaction Detail Options for Local Dev DX

**Date:** 2026-05-07
**Status:** Options paper for cross-team weighing. No commitment to
implementation; written to capture a brainstorm so the team can decide
whether `groundhog_dev_getTransactionDetail` should ship, in what
shape, and on what timeline.

**Companion to:**

- [`../plans/2026-04-29-local-node-hogd-dev-supervisor.md`](../plans/2026-04-29-local-node-hogd-dev-supervisor.md)
  – PR 5 plan; this spec narrows PR 5 to the bare control-endpoint
  scaffold and defers the detail RPC pending the decisions captured
  here.
- [`2026-05-07-view-rpc-and-receipt-return-data.md`](2026-05-07-view-rpc-and-receipt-return-data.md)
  – the receipt return-data + view RPC spec. Milestone A there
  delivers `return_data` and `abort_data` on the public receipt, which
  reframes the everyday "what happened to my tx" loop.

## Goal

Decide what `groundhog_dev_getTransactionDetail` (or any equivalent
post-hoc tx inspection RPC) should look like, given:

- the M-A receipt enrichment changes the everyday loop;
- Groundhog's resource model is materially different from the chains
  whose dev tooling we are tempted to copy;
- the implementations on offer all touch hogd-side code with
  unscoped cost.

This document does not pick a winner. It records the analysis so the
team can pick when there is a real demand signal.

## What changed our reading of the problem

### The Groundhog resource model breaks the EVM/Aptos/Sui motivation

EVM, Aptos, and Sui all treat gas as expenditure: gas → fee → user
money. `forge test --gas-report`, `aptos move run --profile-gas`,
Hardhat's gas reporter, Sui's gas dashboards — all exist primarily as
optimization tools for cost-per-call that flows to end users. Gas
profiling is a financial DX concern dressed up as a debugging tool.

Groundhog burns no tokens. Gas is a halting/protection mechanism, not
an economic one. The remaining product questions about gas reduce to:

- "Did my tx hit the cap?" — already categorical from `ExecResult`
  (`Fault` vs `OK`).
- "Will this tx finish under the configured cap?" — answered by
  running it during tests.
- "Why is this slow?" — perf concern, not a gas concern.

That dissolves most of the live retained-data motivation that other
chains have for keeping `gas_consumed` on every tx.

Storage tokens are not a substitute story. They are **reserved while
storage exists, not burned**. When a contract grows storage, hogdb
locks enough storage tokens to cover the committed storage-cost
increase. When storage shrinks or is deleted, committed storage-cost
decreases can release reserved tokens, subject to the storage-cost
model and item-specific semantics. The product question is therefore
"what balance and storage do I currently have?", which is a current
state question. Historical per-tx deltas are a separate debugging or
accounting view, not the default cost loop.

So neither gas nor storage tokens give us a "what did this transaction
cost?" question to answer post-hoc. Per-tx execution detail is in the
debugging-curiosity bucket — useful when something is mysterious, not
in the everyday loop — for every field, not just trace.

### Cross-chain inspection patterns, with the model difference accounted for

| Chain | Live retained shallow data | Rich submit response | Replay-based deep trace |
|---|---|---|---|
| Anvil/Foundry | receipt + logs | `eth_sendTransaction` returns hash; rich data via separate calls | `cast run <txhash>` re-executes; `--steps-tracing` enables in-place; `debug_traceTransaction` |
| Hardhat | receipt; auto Solidity stack traces on revert | n/a | `debug_traceTransaction` returns structLogs |
| Aptos *(Move L1)* | `gas_used`, status, vm_status | n/a | **`aptos move replay --txn-id N --profile-gas`** instrumented VM |
| Sui *(Move L1)* | effects, events, objectChanges, balanceChanges | n/a | **`sui replay --digest <D> --trace`** re-executes locally |
| NEAR *(WASM L1)* | n/a | **`broadcast_tx_commit` returns `transferOutcome`** with receipts/logs/gas inline | n/a documented |
| Solana | logMessages, computeUnitsConsumed, innerInstructions, pre/post balances | n/a | `simulateTransaction` re-executes |

**Move L1s — Aptos and Sui — both use replay for deep trace data.**
Neither retains step-level trace in production. Aptos says it
explicitly: "instrumented VM" runs only on replay. Sui requires
`--trace` to download data and re-execute. Even EVM tooling gates
deep-trace live capture behind startup flags; it is never the default.

Live retention is reserved for shallow data the production indexer is
already storing. NEAR's pattern is different again: rich submit
response carries the just-submitted tx outcome inline, so the dev
needs no separate inspection RPC for the most common case.

The full picture is three layers, not two:

1. live retained shallow data (every chain),
2. rich submit response (NEAR; what our M-A return_data + abort_data
   starts to do),
3. replay-on-demand for deep traces (Aptos, Sui).

Nobody ships live-capture deep trace into production indexers. That
matches the shape of the executor sink-based design we considered and
set aside.

## What we have today

Confirmed by reading the current code:

- `TxEventLog` (the indexer's per-tx record, `protocol/event.x:47`)
  retains `proposed_tx_hash`, `exec_result`, and `events`. **No
  `gas_consumed`, no `storage_deltas`, no kConsoleLog output.**
- `gas_consumed` is computed inside hogexec at the runtime/invocation
  layer (`hogd/hogexec/runtime/runtime.cc:101`) and accumulated by
  `SystemCallHandler` for the transaction. It does **not** flow into
  wire `ExecResult` or any retained data. The trace sink path is the
  only place it is captured per-tx today.
- hogexec already has full trace machinery
  (`InvocationFrameTrace`, `SyscallTrace`) behind
  `enable_tracing()`. Used by tests; never on in production.
- hogdb's "snapshot" terminology refers to per-tx MVCC concurrency
  primitives (`hogd/hogdb/block_tx.h`, `invoke_tx.h`,
  `item_lock.h`), **not** historical state-at-height reads. There is
  no obvious API for "give me the state at block N."
- Per-tx contract execution is single-threaded; the WASM-visible
  surface is deterministic by protocol design. The non-determinism in
  hogexec (storage-key shuffle in `item_view.cc:348,464`,
  `database_proxy.h:135`; thread-id hash in `dispatch_pool.h:76`) is
  internal-ordering only and should not affect WASM-visible behavior
  or per-tx trace shape. Worth confirming with hogd team before
  committing to replay-based fidelity.
- M-A (`docs/cli/specs/2026-05-07-view-rpc-and-receipt-return-data.md`)
  delivers `return_data` and `abort_data` on the public receipt. With
  M-A, the receipt RPC alone covers status + return value + revert
  reason for the just-submitted loop.

## The four paths

| Path | Production cost | Hogd-side work | Strength | Weakness |
|---|---|---|---|---|
| **A1-deltas** retention | per-tx delta retention added to the indexer | indexer schema + committed-delta capture from the hogdb/block commit path, or an equivalent event/indexer feed | Maps to a Groundhog-real concern (state transitions and current locks) | Bigger schema change of the A1 family; once retained it is a forever cost on production indexers |
| **A2** replay | none | hogdb state-at-height read API + replay harness in hogd-dev | Zero production cost; one path covers gas + deltas + trace; matches Aptos/Sui exactly | Requires hogdb historical reads; unknown scope; depends on per-tx trace determinism (likely fine, not yet confirmed) |
| **A3** rich submit | response shape on `sendRawTransaction` (or sibling `submitAndAwait`) | scheduler/coordinator return-path enrichment | Just-submitted dev loop is the common case; no retention burden; complements M-A naturally | Doesn't help historical inspection; only useful at submit time |
| **A4** defer | none | none beyond the null capability-discovery control endpoint already in flight | M-A covers the everyday loop; nothing committed prematurely | Real product gap if/when post-hoc deep inspection becomes a workflow blocker |

A `gas-only A1` retention path is explicitly excluded: gas has no
fee story in Groundhog and the live retained data is low-value
relative to its protocol-change cost.

A live-capture sink in production executor code (the path we
considered before this brainstorm) is also excluded: no comparable
chain ships live deep-trace capture, and the boundary surface in
production code paths is hard to defend against the data we now have.

## Recommendation

**A4 for PR 5; A2 as the preferred future path; A3 as a possible
complementary move if the just-submitted ergonomics need more.**

Reasoning:

- M-A receipt enrichment covers the everyday loop and lands first.
- All three implementation paths require hogd-side work of unscoped
  cost. We cannot credibly commit without hogd-team input.
- Of those three, A2 has the cleanest production cost story (zero) and
  matches the precedent set by Move L1s, which are the closest
  architectural cousins.
- A3 is independent of A2 and remains attractive for "just-submitted"
  ergonomics. They compose; they are not in conflict.
- A1-deltas is the path of last resort: real product motivation, but
  the only one that adds permanent cost to production data retention,
  and it does so in service of debugging-curiosity rather than
  cost-accounting.

If the team picks A4, PR 5 narrows to the control endpoint scaffold
plus `hogddevcontrol_null` already in flight on the hogd side. The
detail RPC waits for a real demand signal and a follow-up spec.

## What this means for PR 5

If A4 is accepted:

- `groundhog_dev_getTransactionDetail` is removed from PR 5 scope.
- `crates/rpc-api`'s `GroundhogDevApi` keeps `get_accounts` only;
  `get_transaction_detail` comes out of the trait alongside `reset`
  (already removed).
- The supervisor stub `crates/node-supervisor/src/dev_control.rs`
  drops the `get_transaction_detail` METHOD_NOT_FOUND stub.
- `TransactionDetail` and friends in `crates/api-types/src/dev.rs`
  are kept (they have unit-test coverage and represent useful future
  shape) but become unused; defer their removal until the followup
  spec picks a final shape.
- The hogd-side control-endpoint scaffold (`hogd_dev_control.x`,
  `hogd_dev_control.h`, readiness `control` field, `hogddevcontrol_null`)
  lands as the only PR 5 deliverable on the hogd side.

If a different path is later picked, this document's table is the
input; the followup spec is a fresh design pass.

## Open questions deferred to the followup

- A2 viability detail: what is the actual scope of adding state-at-height
  reads to hogdb? Confirm replay determinism for per-tx trace shape.
- A3 surface: is a separate `submitAndAwait` RPC preferred to enriching
  the existing `sendRawTransaction` return shape? What fields go inline
  vs. behind explicit request flags?
- A1-deltas surface: if the team picks retention anyway, what is the
  retention window, and does it live in the production indexer or only
  in hogd-dev?
- Whether `kConsoleLog` output deserves its own retention/exposure path
  independent of any of A1/A2/A3. Console-log capture is a separate
  product question with its own precedent (Hardhat's `console.log`,
  Solana's program logs) and is not tied to the deep-detail decision.
