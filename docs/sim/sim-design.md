# Simulator design

`hogsim` runs stateful traffic and fault scenarios against a real JSON-RPC target.
It answers how a workload behaves over time: whether the chain progresses,
transactions complete, actors observe their writes, and the generator or target
runs out of capacity. Start with the [quickstart](README.md) to run a scenario.

## Boundaries

The simulator owns actor scheduling, workload state, assertions, result artifacts,
and the lifecycle of targets it starts. Bootstrap and runtime manifests remain
the contracts for chain setup and discovery. Workloads use public JSON-RPC;
they do not bypass it through private scheduler injection.

A `local` provider owns a `thruput node` subprocess. An `existing` provider
borrows an endpoint and must not restart or clean it up. Process ownership is
recorded separately from chain identity because knowing where a service runs
does not grant permission to kill it. Orphan sweeping verifies recorded ownership
against the live process before signalling it.

`thruput-bench` measures controlled method-specific load. Testkit runs arbitrary
application actor logic with concrete assertions. Hogsim occupies the space
between them: reusable stateful workloads expressed as scenario data, with
long-run diagnostics and scheduled faults. Cloud deployment remains outside its
scope.

## Deterministic schedule, real execution

A scenario and seed determine the intended actor schedule. The node, network,
and OS still run in real time; equal seeds do not guarantee identical completion
order or chain outcomes. The schedule is streamed so a long soak does not require
allocating every future event up front.

Actors hold state across ticks, including nonce allocation and pending receipts.
Write, receipt, and read lanes have different concurrency needs. Serializing an
actor's nonce-bearing writes protects its transaction sequence without preventing
independent reads. Global limits bound generator work. Skipped ticks under
backpressure must remain visible so a slow generator cannot appear to have
successfully delivered the requested load.

Read-after-write is a coupled workload: reconcile a baseline, submit, await a
successful receipt, read, and compare. A restart or failed step invalidates what
the actor thought it knew. It reconciles again rather than silently carrying a
stale expected value forward. This focused state machine avoids introducing a
general workflow language into scenario files.

## Target faults and results

Local restart chaos replaces the owned node process and reacquires readiness.
Pending transactions from the old target cannot silently remain ordinary pending
work after an ephemeral restart. Results distinguish target loss from an RPC or
assertion failure. Existing targets do not inherit local-process capabilities.

The event stream records observations; the aggregate result summarizes them.
Keep workload failures, skipped work, unresolved receipts, and unavailable
measurements distinct. A missing sample is not zero resource use, and submission
latency is not confirmation latency. Shutdown includes bounded reconciliation
and cleanup rather than unbounded waiting for every receipt.

Resource sampling is diagnostic and belongs to the provider that can identify
the target process. It must not guess process ownership for an external endpoint
or alter traffic dispatch. Compare configurations and available measurements
before treating a result delta as a regression. Local throughput alone does not
establish capacity for a different topology.

## Source map

| Concern | Source |
| --- | --- |
| Scenario types and validation | [manifest.rs](../../crates/sim/src/manifest.rs), [validation.rs](../../crates/sim/src/validation.rs) |
| Intended schedule | [scheduler.rs](../../crates/sim/src/scheduler.rs) |
| Actor state and execution | [actor.rs](../../crates/sim/src/actor.rs), [executor.rs](../../crates/sim/src/executor.rs) |
| Target lifecycle and orphan cleanup | [target.rs](../../crates/sim/src/target.rs), [sweep.rs](../../crates/sim/src/sweep.rs) |
| Results and comparison | [result.rs](../../crates/sim/src/result.rs), [compare.rs](../../crates/sim/src/compare.rs) |
| Resource observations | [resource.rs](../../crates/sim/src/resource.rs) |
| Executable scenarios | [examples/sim](../../examples/sim/) |

Use those types, tests, and scenario files for supported fields and exact
semantics. The design guide should explain why the boundaries exist, without
becoming another implementation or result-schema specification.
