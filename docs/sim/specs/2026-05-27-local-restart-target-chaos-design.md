# Local Restart-Target Chaos (Phase 2, Slice B) - Design

**Date:** 2026-05-27
**Status:** Handoff design, ready for implementation.
**Owner of implementation:** TBD.

This is a handoff design doc. It is self-contained: an implementer who has
not seen the planning conversation should be able to build from it.

## Summary

Add executable `restart-target` chaos for the simulator's `local` target
provider. A restart event stops the simulator-owned `groundhog node`, waits for
it to exit, relaunches it on the same public JSON-RPC port, and resumes the
workload schedule only after the restarted node emits startup JSON.

The simulator still talks to the target only through public JSON-RPC. The new
chaos controller owns only process lifecycle around the target.

## Scope

This is **Slice B** of Phase 2. Slice A added the managed local provider. Slice
B makes the existing chaos model executable for that provider.

### In scope

- `TargetCapabilities::for_provider(Local)` advertises `restart-target`.
- Manifest validation accepts `restart-target` for `local` and continues to
  reject it for `existing` and `remote-ssh`.
- `LocalTargetProvider` exposes a real `ChaosController`.
- Restart is whole-target restart: stop and relaunch `groundhog node`, not only
  the `hogd-dev` child.
- Restart reuses the resolved port from the active `rpc_url`, including when the
  initial launch requested `--port 0`.
- Restart verifies the new startup JSON resolves to the same `rpc_url`.
- The executor drains completed workload tasks before a chaos action, executes
  the action synchronously, and does not dispatch later ticks until recovery.
- Chaos events at the same simulation time as actor ticks still execute first,
  preserving the scheduler's deterministic ordering.
- Pending actor receipt queues are drained at restart time and recorded as lost
  work with `errorClass: "target-restarted"`.
- `result.jsonl` records chaos transition events.
- `result.json.chaosEvents` contains the transition history.
- Failed chaos marks the run outcome `failed`, even if workload assertions pass.
- Add one local restart smoke example and process-level ignored smoke coverage.

### Out of scope

- Provider log capture in result artifacts.
- Sweeper ownership fields.
- `remote-ssh` restart chaos.
- Asynchronous outage modeling where workloads continue dispatching into
  downtime.
- Mutating target identity or runtime identity after restart.

## Architecture

### 1. Local provider restart state

`LocalTargetProvider` keeps provider-owned state because `ResolvedTarget` is a
pure data value. The acquired local state must retain:

- live `groundhog node` child handle;
- resolved `groundhog` binary path;
- optional `hogd-dev` binary path;
- required dev account count;
- startup timeout;
- active resolved RPC URL;
- active resolved RPC port;
- latest runtime manifest path for provider-internal cleanup/diagnostics only.
  `ResolvedTarget` and `result.json` keep the original resolved runtime
  identity for the run.

Initial acquire and restart use the same launch helper:

```
launch_local_node(config, port):
  spawn groundhog node --dev-account-count N --json
       [--hogd-dev-binary X] --port P
  read startup JSON with timeout
  parse data.rpc_url and data.runtime_manifest.path
  return child + startup
```

The initial launch may omit `--port` or use `--port 0`; after startup the
provider parses the port from the emitted `rpc_url`. Restart always passes that
resolved port.

### 2. Chaos controller

`LocalTargetProvider::chaos_controller()` returns `Some(self)`, and
`LocalTargetProvider` implements `ChaosController`.

`execute(event)` for `restart-target`:

1. Reject if no acquired child exists.
2. Take the current child from provider state.
3. SIGTERM the child, wait up to the normal local cleanup grace, SIGKILL if
   needed.
4. Relaunch using the retained launch config and resolved port.
5. Parse startup JSON.
6. Verify restarted `rpc_url` equals the original resolved `rpc_url`.
7. Store the new child and updated internal runtime manifest path.
8. Return `Recovered` on success.

If relaunch fails after a replacement child was spawned, the provider
terminates that replacement before returning the failure. The provider leaves no
stale child in state. Cleanup remains idempotent.

### 3. Executor integration

The executor accepts an optional chaos controller. For each scheduled chaos
event:

1. Drain already-completed workload tasks without waiting for still-running
   tasks.
2. Sleep to the chaos simulation time while continuing to drain completed
   tasks, matching actor tick behavior.
3. Emit `scheduled` and `started` chaos transitions.
4. Abort any still-running workload tasks and stop counting their permits as
   in-flight.
5. Execute the provider chaos action synchronously.
6. After successful recovery, record aborted tasks and drain all pending actor
   receipt queues as failed diagnostics with `errorClass: "target-restarted"`.
   Deferring these records until recovery prevents a provider-side restart
   failure from falsely clearing valid actor receipt state.
7. Emit `completed` and `recovered` on success, or `failed` on error.

The schedule is paused for the duration of step 6. Later actor ticks run only
after the restarted node is online.

### 4. Pending receipt loss semantics

Actor state gains a helper to drain pending receipt queues:

```
drain_pending_receipts() -> Vec<DrainedPendingReceipt>
```

Each drained receipt target becomes a visible failed workload event:

- `kind: "workload-failed"`
- `workload: "receipt-poll"`
- `actorId`: the actor that owned the pending receipt
- `scheduledTimeMs`: the chaos event simulation time
- `startedAtUnixMs` and `completedAtUnixMs`: the wall-clock time the loss was
  recorded
- `latencyMs: 0`
- `rpcMethod: "groundhog_getTransactionReceipt"`
- `txHash: <pending hash>`
- `receiptPresent: false`
- `errorClass: "target-restarted"`
- `error: "pending receipt lost because target restarted"`

These failures increment `receipt-poll` failed counts and error class counts, so
assertions and error-rate consumers see the lost work.

## Result Artifacts

### JSONL

Add a `chaos-transition` result event:

```json
{
  "schemaVersion": 1,
  "kind": "chaos-transition",
  "chaosIndex": 0,
  "chaosKind": "restart-target",
  "state": "recovered",
  "simulationTimeMs": 1000,
  "startedAtUnixMs": "1710000000000",
  "completedAtUnixMs": "1710000000050",
  "latencyMs": 50,
  "detail": "optional provider detail"
}
```

Required fields for every transition are `chaosIndex`, `chaosKind`, `state`,
`simulationTimeMs`, and `startedAtUnixMs`. `completedAtUnixMs` and `latencyMs`
are present for `completed`, `failed`, and `recovered`; they are omitted for
instantaneous `scheduled` and `started` markers. `detail` is optional for all
states and should be present for provider failure details when available.

State emission:

- `scheduled`: emitted when the executor reaches the chaos event in schedule
  order.
- `started`: emitted immediately before local target stop/relaunch begins.
- `completed`: emitted after the provider restart action returns successfully.
- `recovered`: emitted after `completed` for restart actions whose successful
  final state is `Recovered`.
- `failed`: emitted if the controller is missing or the provider returns an
  error.

This is additive and does not require a schema-version bump.

### result.json

Populate `SimResult.chaosEvents` with the same transition history at aggregate
level. In aggregate records, `kind` is the chaos kind (`"restart-target"`), not
the JSONL event tag (`"chaos-transition"`). Additive optional fields include
`chaosIndex`, `detail`, and `latencyMs`; existing readers that only know
`kind`, `state`, and `simulationTimeMs` remain compatible.

## Failure Semantics

- Missing controller for a scheduled chaos event is a simulation error.
- Provider chaos failure is recorded in artifacts and causes final outcome
  `failed`.
- Workload assertion success cannot override a chaos failure.
- Cleanup still runs after failure through the existing provider lifecycle.

## Testing

Unit tests should cover:

- local capabilities advertise `restart-target`;
- existing and remote providers reject `restart-target`;
- local manifests with restart chaos validate;
- scheduler keeps chaos before actor ticks at the same simulation time;
- restart argv uses resolved port, not original `--port 0`;
- local restart rejects missing acquired child;
- pending receipt queues drain and classify as `target-restarted`;
- lost receipt events appear in `result.jsonl`, increment
  `workloadMetrics.receipt-poll.counts.failed`, and populate
  `errorCounts["target-restarted"]`;
- chaos events serialize to the JSONL shape;
- `result.json.chaosEvents` records expected states;
- failed chaos marks the run failed.
- a deterministic failed-controller or failed-restart case leaves failed chaos
  visible in `result.json`.

Process-level smoke:

- Add `examples/sim/local-restart-chain-tip-smoke.json`.
- Add ignored local smoke coverage gated by `GROUNDHOG_SIM_HOGD_DEV_BINARY`.
- The smoke runs one local restart and verifies:
  - run passes;
  - `result.json` contains completed/recovered restart chaos;
  - `result.jsonl` contains chaos transition events;
  - final cleanup closes the RPC port.

## Review Gates

Each implementation slice should be reviewed before moving on:

- behavior/lifecycle reviewer: process ownership, stable RPC URL, startup
  failure cleanup, SIGTERM/SIGKILL ordering, deterministic scheduling;
- test/artifact reviewer: JSONL/result shape, backward compatibility,
  assertion/outcome behavior, and coverage gaps.
