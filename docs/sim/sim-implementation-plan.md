# Groundhog Simulation Implementation Plan

**Date:** 2026-05-25
**Status:** Draft execution checklist. Phase 1 existing-target execution,
reviewer hardening, live hogd-dev smoke, submit/receipt actor state,
submit/receipt assertion wiring, numeric compare, the target-provider boundary,
Phase 2 Slice A/B local-provider execution and restart-target chaos, local
provider log capture, the local sweeper, the scenario exerciser catalog, and
Phase 3 Slice G contract deploy + view-call execution and Slice H first-class
scenario library catalog are complete. Phase 3 Slice I contract storage-read
execution is complete.

This plan pairs with [Simulation design](sim-design.md). The design doc owns
the architectural boundary and long-term shape. This file is the implementation
tracker, with detail focused on Phase 1.

## Scope

The first useful simulator should run against an already-started target:

```bash
hoglab run examples/sim/chain-tip-smoke.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest /tmp/hogd-dev/runtime-manifest.json
```

Phase 1 should prove:

- public JSON-RPC traffic generation;
- runtime-manifest account discovery;
- deterministic dev signer derivation;
- run-unique transaction nonce namespacing for existing targets;
- basic actor scheduling;
- append-only event output;
- aggregated result output;
- pairwise result comparison.

Phase 1 explicitly does not launch `hogd-dev`, manage a local process, execute
chaos, deploy new contracts, or create remote infrastructure.

## Completed Slice: Chain-Tip Execution

- [x] Add a scheduler module that expands load phases into deterministic actor
  ticks.
- [x] Add an event writer abstraction so `result.jsonl` append/flush behavior
  is owned in one place.
- [x] Implement the `chain-tip` workload through the typed RPC client.
- [x] Aggregate `chain-tip` counts, errors, latency samples, and chain progress
  into `result.json`.
- [x] Add unit tests for deterministic scheduling and result aggregation.
- [x] Add an ignored process-level smoke test gated by
  `GROUNDHOG_SIM_RPC_URL` and `GROUNDHOG_SIM_RUNTIME_MANIFEST`.
- [x] Run `examples/sim/chain-tip-smoke.json` against a live `groundhog node`
  / `hogd-dev` target and verify a passing result bundle.
- [x] Verify the same smoke with `groundhog node --hogd-protocol
  hogp/77397967490b`, using registry-linked Rust protocol crates and only
  overriding the intro protocol token for a newer local `hogd-dev` build.
- [x] Make `groundhog node --hogd-protocol` a documented, visible CLI flag
  for local protocol-token skew; real XDR shape mismatch still fails through
  the generated clients.
- [x] Fix executor in-flight accounting so completions drained during the
  schedule loop are not waited on a second time at shutdown.

## Completed Slice: Reviewer Hardening

- [x] Normalize `simulation.json` after CLI overrides and hash the emitted
  artifact.
- [x] Record the resolved nonce namespace in `simulation.json` and
  `result.json`.
- [x] Add runtime identity and target identity to `result.json`.
- [x] Change the scheduler to emit deterministic actor ticks; workload choice
  now happens at dispatch time so actor state can participate later.
- [x] Dispatch workload execution asynchronously with global and per-actor
  in-flight bounds.
- [x] Use bounded latency histograms instead of retaining every latency sample.
- [x] Cap chain-progress samples in `result.json` while preserving total
  observation count and first/last/max heights, without rewriting earlier
  samples after the cap is reached.
- [x] Evaluate `maxErrorRate` and `maxChainStall`; mark submit/receipt
  assertions unsupported until those workloads land.
- [x] Keep workload failures fatal even when all configured assertions pass.
- [x] Evaluate chain-stall assertions in simulation-time order so concurrent
  completion order cannot panic or distort the stall window.
- [x] Open `result.jsonl` in append mode and cover append-only behavior with a
  unit test.
- [x] Make workload selection hashing architecture-independent.
- [x] Parse manifest durations as integer milliseconds in scheduler,
  validation, and assertion paths.
- [x] Reject unsupported `result.json` schema versions during compare/load.
- [x] Rename latency p99.9 output to strict camelCase `p999Ms`.
- [x] Serialize wall-clock event timestamps as decimal strings.
- [x] Surface workload task panics with actor/workload context instead of
  collapsing them into completion-channel shutdown.
- [x] Move compare text rendering next to the structured diff.
- [x] Remove duplicated workload-name formatting helper.
- [x] Fail instead of falling back to non-hex debug strings for JSON string
  fields such as block and genesis hashes.
- [x] Validate existing-target flags before applying CLI overrides.
- [x] Remove async-trait allocation from the Phase 1 mock-client seam.
- [x] Delete dead chain-progress construction helper.
- [x] Drain completed workload tasks while waiting for the next simulation tick
  or phase so `result.jsonl` does not trail long scheduler sleeps.
- [x] Add `runId` to `result.json` and `schemaVersion` to each `result.jsonl`
  event for future UI/API consumers.
- [x] Reuse the shared `groundhog-signing` dev-wallet `doAction` transaction
  helper across CLI calls, bench submit load, and sim submit smoke.
- [x] Replace manual result-event serialization with derived event serialization
  plus an `EventWriter` version wrapper so new event variants inherit
  `schemaVersion`.
- [x] Add target-provider traits and an `existing` provider adapter without
  changing Phase 1 run behavior.

## Current Next Slice

- [x] Implement `wallet-submit-smoke` transaction construction and submit
  execution.
- [x] Add actor state tracking for signer, wallet address, next namespaced
  nonce, and last submitted transaction hash.
- [x] Advance actor nonce only after a submit returns a transaction hash, so
  transient RPC submit failures retry the same namespaced nonce.
- [x] Share runtime-manifest dev-wallet entrypoint parsing through
  `groundhog-json-rpc-types`.
- [x] Share sign-and-0x-encode transaction helper through `groundhog-signing`.
- [x] Add a byte-equal determinism test for wallet-submit-smoke transaction
  construction.
- [x] Add receipt-poll workload tied to actor state and record submit-to-receipt
  latency when receipts appear.
- [x] Wire submit/receipt assertion evaluation for p99 submit latency and
  receipt timeout rate.

## Open Non-Blocking Hardening Follow-Ups

- [x] Move executor, result building, and compare logic out of `main.rs` into
  library modules while the surface area is still small.
- [x] Add a mock-client orchestration test that drives schedule dispatch,
  workload execution, event writing, and result aggregation end to end.
- [x] Add Phase 1 compare numeric deltas with compare-specific tests.
- [x] Add compare unit tests for config mismatch, metric availability
  transitions, numeric deltas, and numeric-skip behavior.
- [x] Add provider trait and `ChaosController` types matching the design names,
  even if only the `existing` provider is executable in Phase 1.
- [ ] Add doc comments and `#[non_exhaustive]` to public library API before
  treating `groundhog-sim` as a stable downstream dependency.
- [ ] Give `groundhog node` cooperative shutdown for programmatic parents
  (parent-death detection or an explicit shutdown channel) so a hoglab panic
  or SIGKILL cannot orphan the node + hogd-dev tree. This is the durable home
  for managed-target robustness; the local provider deliberately does not add
  a sim-side panic guard or signal handler. Normal-path teardown is a graceful
  SIGTERM (catchable, so the node reaps hogd-dev), and terminal Ctrl-C is
  already handled by process-group signal delivery to the node itself. Lower
  priority while a deployed devnet/EC2 target may make the local provider the
  non-strategic, least-hardened provider.
- [x] Decide whether local-provider process handles, log handles, and metrics
  endpoints live on `ResolvedTarget` or in provider-owned state returned by
  target acquisition. Slice A/B keep live process state and restart launch
  config in provider-owned state; `ResolvedTarget` remains pure result data.
- [ ] Replace ad-hoc CLI output and silent internal drops with the eventual
  logging/printing discipline.
- [ ] Add `--json` to `hoglab validate` and `hoglab run` so all hoglab
  subcommands match the CLAUDE.md machine-readable-output convention. Each
  needs a typed payload: validate emits a manifest-validation report;
  run emits a run summary (outcome, counts, out-dir, run id).
- [ ] Add a stream header event or sibling metadata file when the first UI/live
  progress consumer needs stream-level run metadata without scanning JSONL.
- [ ] Add a redacted actor view type when actor state becomes user-visible.
- [ ] Revisit `build_actor_states` if v1's static one-account-per-actor pool
  evolves into hot account rotation or dynamic actor spawning.
- [ ] Decide whether submit events should inline `walletAddress` and
  `accountIndex`, or keep those fields derivable from run metadata until the
  UI/event consumer exists.
- [ ] Canonicalize or document the CLI wallet entrypoint constant versus the
  runtime-manifest-derived dev-wallet `doAction` entrypoint used by bench and
  sim.
- [ ] Add `wallet-submit-smoke` coverage to the ignored existing-target smoke
  test once the local smoke fixture can reliably provide a runtime manifest.
- [ ] Replace `ChainProgressBuilder`'s retained uncapped observations with
  streaming max-stall computation before long Phase 2 soaks make chain-tip
  observation memory material.
- [ ] Decide how `simulator-backpressure` skipped completions interact with run
  outcome and `maxErrorRate`. Today they are emitted as `WorkloadSkipped` with
  `errorClass: "simulator-backpressure"` and excluded from both the error-rate
  denominator and the failed-workload count, so a saturated simulator can shed
  scheduled load and still report `passed`. Options include adding a dedicated
  `maxBackpressureRate` assertion, surfacing `backpressureRate` in
  `result.json` for downstream gating, or reclassifying backpressure skips as
  failures.
- [x] Replace receipt-poll's v1 latest-transaction slot with per-transaction
  receipt tracking before wiring `maxReceiptTimeoutRate`; otherwise a newer
  submit can hide timeout state for an older unreceipted transaction.
- [ ] Cap per-actor pending receipt queues and emit simulator overload events
  when submit rate exceeds receipt tracking capacity.
- [ ] Decide whether pre-submit receipt-poll ticks should remain
  `WorkloadSkipped` with `errorClass: "no-submitted-transaction"` or move to a
  separate warm-up classification before error-rate consumers depend on it.

## Phase 0: Skeleton

- [x] Add a `crates/dev/sim` workspace member named `groundhog-sim`.
- [x] Add a `hoglab` binary with `run`, `compare`, and `validate`
  subcommands.
- [x] Keep the manifest kind as `groundhog-simulation` while using `hoglab`
  as the user-facing CLI name.
- [x] Add typed simulation manifest structs.
  - Include `baseline` as an optional typed path; Phase 2 adds automatic
    runtime baseline diffing.
- [x] Add typed result structs.
- [x] Add typed `MetricAvailability`:
  - `observed`
  - `unsupported`
  - `unavailable`
- [x] Add typed `ChaosEvent` and `ChaosEventState` enums even though Phase 1
  providers execute no chaos.
  - `ChaosEventState` variants: `scheduled`, `started`, `completed`, `failed`,
    `recovered`.
- [x] Add target provider traits:
  - target acquisition
  - capability declaration
  - `ChaosController`
  - ownership metadata
  - cleanup hook
- [x] Add typed target provider specs, capabilities, and ownership metadata.
- [x] Add JSON parsing and validation for simulation manifests.
- [x] Add example manifest under `examples/sim/`.
- [x] Add unit tests for manifest parsing and validation.

## Phase 2: Local Managed Target

### Slice A: Local Provider

- [x] Launch and clean up a simulator-owned `groundhog node`.
- [x] Request the dev account count required by the scenario.
- [x] Parse startup JSON for the effective RPC URL and runtime manifest.
- [x] Record local target ownership artifacts.
- [x] Add local-provider process-level smoke coverage.

### Slice B: Local Restart-Target Chaos

- [x] Add handoff design:
  `docs/sim/specs/2026-05-27-local-restart-target-chaos-design.md`.
- [x] Advertise and validate `restart-target` for the `local` provider.
- [x] Add local provider restart support with stable resolved-port relaunch.
- [x] Wire executor chaos handling and pause later ticks until restart
  recovery.
- [x] Mark pending receipt queues lost at restart with
  `errorClass: "target-restarted"`.
- [x] Record chaos transitions in `result.jsonl` and `result.json`.
- [x] Make failed chaos actions produce a failed run outcome.
- [x] Add `examples/sim/local-restart-chain-tip-smoke.json`.
- [x] Extend local smoke coverage to verify restart artifacts and final port
  cleanup.

## Phase 1: Existing Target Simulator

### Inputs

- [x] `hoglab run <scenario-path>`
- [x] `--rpc-url <url>` override.
- [x] `--runtime-manifest <path>` override.
- [x] `--out-dir <path>` override.
- [x] `--seed <seed>` override.
- [x] `--nonce-namespace <u64>` override for repeatable runs on fresh targets.
- [x] Reject scenarios whose selected provider does not match the supplied
  target flags.
- [x] Validate runtime manifest schema version using `json-rpc-types`.
- [x] Reject runtime manifests that do not advertise bootstrap accounts.
- [x] Reject unsupported bootstrap account derivation schemes.
- [x] Validate bootstrap account public keys against derived signers, matching
  the `groundhog-bench` behavior.
- [x] Validate that requested actor count does not exceed available bootstrap
  accounts unless account reuse is explicitly added to the scenario schema.
- [x] Validate that workloads requiring receipts or status have read-plane
  support in the runtime manifest, or reject the scenario before sending
  traffic.
- [x] Reject any chaos event for the `existing` provider.

### Runtime Model

- [x] Use scenario-seed-derived deterministic scheduling and workload
  selection. Phase 1 uses stable hash inputs rather than a stateful RNG.
- [x] Generate a run-unique nonce namespace when one is not supplied, and record
  it in normalized `simulation.json` plus `result.json`.
- [x] Represent nonce namespaces as the high 32 bits of the transaction nonce;
  validate that `--nonce-namespace` fits in `u32`, and combine it with the
  per-actor low-32-bit sequence as `(namespace << 32) | actor_sequence`.
- [x] Document that fixed nonce namespaces require a fresh target or an empty
  replay cache.
- [x] Assign one bootstrap account per actor.
- [x] Track per-actor state:
  - account index
  - signer
  - wallet address
  - next nonce within the run nonce namespace
  - last submitted transaction hash
  - last observed receipt
- [x] Implement load phases with:
  - duration
  - actor count
  - rate per actor
- [x] Implement weighted workload selection.
- [x] Keep workload execution asynchronous.
- [x] Bound in-flight work globally.
- [x] Bound in-flight work per actor.
- [x] Emit explicit overload/backpressure events when the simulator cannot
  maintain its own schedule.

### Workloads

- [x] `chain-tip`
  - call `groundhog_chainTip`
  - record latency, success, and error classification
- [x] `wallet-submit-smoke`
  - build dev-wallet-compatible signed transactions from runtime-manifest
    bootstrap accounts
  - submit through `groundhog_sendRawTransaction`
  - record tx hash on actor state
  - record submit latency
  - record that HTTP/RPC submit success is separate from receipt execution
    success
- [x] `receipt-poll`
  - poll `groundhog_getTransactionReceipt` for actor's last submitted tx
  - record null receipt as a non-error poll result
  - record committed receipt status when present
  - do not require `ExecStatus::Ok` for the Phase 1 smoke transaction
  - record submit-to-receipt latency when possible
- [x] Defer `events-query`, `storage-read`, `status-poll`, and `view-call`
  until the first three workloads are stable unless Phase 1 remains small.
  `view-call` and `storage-read` landed later in Phase 3.

### Event Stream

- [x] Create result directory before traffic starts.
- [x] Write normalized `simulation.json`.
- [x] Copy normalized `runtime-manifest.json`.
- [x] Do not write `bootstrap-manifest.json` for the `existing` provider;
  instead record bootstrap manifest availability as unavailable/unknown.
- [x] Write `target-ownership.json` with `startedBySim = false`.
- [x] Stream `result.jsonl` records as newline-delimited JSON.
- [x] Include `schemaVersion` on each JSONL event.
- [x] Include record kinds:
  - run started
  - target resolved
  - phase started
  - workload completed
  - workload failed
  - workload skipped
  - assertion checked
  - run completed
- [x] Include metric availability records.
- [x] Flush `result.jsonl` frequently enough for live tailing.
- [x] Preserve enough context in every workload record to debug:
  - actor id
  - workload name
  - scheduled time
  - started time
  - completed time
  - latency
  - RPC method
  - tx hash when applicable
  - error class when applicable

### Result Aggregation

- [x] Write final `result.json`.
- [x] Include run id.
- [x] Include manifest hashes:
  - simulation manifest hash
  - runtime manifest hash
  - bootstrap manifest hash when present, otherwise null/absent with
    availability reason
- [x] Include protocol, genesis hash, producer binary/version/commit from the
  runtime manifest.
- [x] Do not claim a producer executable hash for `existing` targets unless an
  external provider supplied one.
- [x] Include target provider and target identity.
- [x] Include per-workload counts.
- [x] Include per-workload error counts and error classes.
- [x] Include per-workload latency percentiles:
  - p50
  - p95
  - p99
  - p99.9
  - max
- [x] Include submit-to-receipt latency percentiles.
- [x] Include chain progress timeline from observed `chain-tip` calls.
- [x] Mark storage-token metrics `unsupported`.
- [x] Mark NNInt compute metrics `unsupported`.
- [x] Include assertion results.
- [x] Include run outcome:
  - passed
  - failed
  - aborted (reserved in Phase 1; no current run path emits it)
- [x] Write `result.json` atomically at run end.

### Assertions

- [x] Implement `maxErrorRate`.
- [x] Implement `maxReceiptTimeoutRate`.
- [x] Implement `maxChainStall`.
- [x] Implement `p99SubmitLatencyMs`.
- [x] Record every assertion check in `result.jsonl`.
- [x] Include assertion pass/fail details in `result.json`.
- [x] Return a nonzero process exit code when assertions fail.

### Comparison

- [x] Add `hoglab compare <run-a> <run-b>`.
- [x] Accept either run directories or direct `result.json` paths.
- [x] Compare per-workload counts and errors.
- [x] Compare latency percentiles.
- [x] Compare assertion result transitions.
- [x] Compare chain progress and stall metrics.
- [x] Compare manifest hashes and mark cross-config diffs.
- [x] Compare producer identity tuple (`binary`, `version`, optional
  `commitHash`) and only compare executable hashes when both results include
  one.
- [x] Compare metric availability transitions.
- [x] Skip numeric deltas when either side is `unsupported` or `unavailable`
  once numeric deltas are implemented.
- [x] Emit structured JSON behind `--json`.
- [x] Emit a readable text summary for humans.

### Tests

- [x] Unit-test simulation manifest parsing.
- [x] Unit-test unsupported chaos rejection for `existing`.
- [x] Unit-test runtime manifest account validation.
- [x] Unit-test runtime manifest read-plane validation.
- [x] Unit-test weighted workload selection determinism.
- [x] Unit-test deterministic scheduling with same seed, load shape, and chaos
  schedule, comparing event ordering while ignoring wall-clock timestamps.
- [x] Unit-test histogram aggregation.
- [x] Unit-test assertion evaluation.
- [x] Unit-test workload failures with passing assertions still fail the run.
- [x] Unit-test max-chain-stall against out-of-order observations.
- [x] Unit-test append-only JSONL event writing.
- [x] Unit-test result schema-version rejection.
- [x] Unit-test mock-client orchestration from schedule dispatch through
  workload execution, event writing, and result aggregation.
- [x] Unit-test workload panic reporting with actor/workload context.
- [x] Unit-test utility hashing, atomic JSON writes, and JSON string field
  failures.
- [x] Unit-test nonce namespace uniqueness across run IDs.
- [x] Unit-test existing-provider target ownership artifact writing.
- [x] Unit-test draining completions while waiting for the next phase.
- [x] Unit-test compare output for:
  - same-config numeric deltas
  - cross-config manifest hash mismatch
  - metric availability transitions
  - unsupported/unavailable numeric-skip behavior
- [x] Add one ignored process-level smoke test that runs against a supplied
  `GROUNDHOG_SIM_RPC_URL` and `GROUNDHOG_SIM_RUNTIME_MANIFEST`.

### Documentation

- [x] Add `examples/sim/local-wallet-soak.json`.
- [x] Document the `existing` provider.
- [x] Document result directory contents.
- [x] Document comparison semantics.
- [x] Add quickstart commands to `docs/sim/README.md`.

## Phase 2: Local Managed Target

- [x] Add `local` target provider.
- [x] Request the required dev-account count from `groundhog node` based on
  the simulation manifest's max phase users.
- [x] Start `groundhog node` / `hogd-dev`.
- [x] Capture startup JSON and runtime manifest path.
- [x] Capture provider logs for simulator-owned local targets.
- [x] Write target ownership metadata with `startedBySim = true`.
- [x] Stop the target on completion.
- [x] Add sweeper command for sim-owned local targets.
  - [x] Slice E1: write handoff design:
    `docs/sim/specs/2026-05-27-local-target-sweeper-design.md`.
  - [x] Review gate E1: lifecycle and artifact review for PID safety,
    ownership refresh, and report shape.
  - [x] Slice E2: add ownership process capture and restart refresh.
  - [x] Slice E3: add `hoglab sweep`, tests, docs, and smoke coverage.
- [x] Implement `restart-target` chaos for the local provider as whole-target
  stop+restart, matching current `groundhog node` supervisor behavior.
- [x] Record chaos state transitions in `result.jsonl` and `result.json`.
- [x] Support scenario `baseline` and emit baseline diff inside `result.json`.
  - [x] Slice D1: write handoff design:
    `docs/sim/specs/2026-05-27-scenario-baseline-result-diff-design.md`.
  - [x] Review gate D1: spec review for artifact shape, path resolution, and
    deferred CI policy.
  - [x] Slice D2: wire baseline loading and embedded diff into the runner.
  - [x] Review gate D2: semantics review for outcome behavior, load errors,
    and compare reuse.
  - [x] Slice D3: add tests, docs/examples, and verification.
  - [x] Final review gate: full Slice D code review plus test/artifact review.
- [x] Add a local scenario exerciser catalog for putting the simulator through
  short read/write/receipt/restart runs before longer scenario development.
  - [x] Slice F1: write handoff design:
    `docs/sim/specs/2026-05-28-scenario-exerciser-catalog-design.md`.
  - [x] Review gate F1: scenario/catalog design review for scope, scenario
    ladder, and artifact contract.
  - [x] Slice F2: add runnable local scenario catalog examples.
  - [x] Review gate F2: schedule and artifact review for deterministic
    workload coverage.
  - [x] Slice F3: add `scripts/smoke-sim-scenarios.sh` and ignored smoke
    coverage for the fast catalog, optional soak, and existing-provider
    baseline.
  - [x] Slice F4: document the scenario ladder and tracker state.
  - [x] Final review gate: full Slice F scenario catalog review plus
    test/artifact review.

## Phase 3: Stateful Contract Workloads

- [x] Slice G1: write handoff design:
  `docs/sim/specs/2026-05-28-contract-deploy-view-scenario-design.md`.
- [x] Review gate G1: manifest/deploy/view design and artifact contract
  review.
- [x] Slice G2: add deploy-at-start contract artifact support and terminal
  deployment result records.
- [x] Slice G3: execute `view-call` workloads against deployed contracts.
- [x] Slice G4: add Fibonacci example scenario and extend scenario smoke
  artifact checks.
- [x] Final review gate: full Slice G contract scenario review plus gated
  smoke verification.
- [x] Slice H1: add a first-class scenario library catalog:
  `examples/sim/catalog.json`.
- [x] Slice H2: add `hoglab catalog` with tag filtering and JSON output.
- [x] Slice H3: document the source-first contract artifact convention.
- [x] Review gate H: code/API and test/artifact review for catalog metadata,
  CLI output, docs, and schema compatibility.
- [x] Slice I1: write handoff design:
  `docs/sim/specs/2026-05-28-contract-storage-read-scenario-design.md`.
- [x] Slice I2: add manifest validation and contract setup for
  `storageReads`.
- [x] Slice I3: execute `storage-read` workloads against deployed contracts.
- [x] Slice I4: add Fibonacci storage-read example and extend scenario smoke
  coverage.
- [x] Final review gate I: behavior/API and test/artifact review for
  storage-read semantics and artifacts.
- [x] Slice J1: write handoff design:
  `docs/sim/specs/2026-05-29-contract-events-query-scenario-design.md`.
- [x] Slice J2: add manifest validation and contract setup for
  `eventQueries`.
- [x] Slice J3: execute `events-query` workloads against deployed contracts.
- [x] Slice J4: add Fibonacci events-query example and extend scenario smoke
  coverage.
- [x] Final review gate J: behavior/API and test/artifact review for
  events-query semantics and artifacts.
- [x] Slice K1: write handoff design:
  `docs/sim/specs/2026-05-29-positive-event-indexing-scenarios-design.md`.
- [x] Slice K2: add manifest validation and contract setup for
  `contractCalls`.
- [x] Slice K3: execute `contract-call` workloads through dev-wallet `doAction`
  with receipt polling and additive artifacts.
- [x] Slice K4: add positive event-indexing catalog scenario and extend smoke
  coverage.
- [x] Add view-call workloads when the runtime manifest advertises view support.
- [x] Slice L1: write handoff design:
  `docs/sim/specs/2026-05-31-actor-state-read-after-write-design.md`. Couples
  the "richer actor state machines" and "read-after-write scenario presets"
  bullets into one coupled `read-after-write` workload preset rather than a
  manifest flow DSL.
- [x] Review gate L1: architecture + fixture decision reviewed. Approved coupled
  preset over flow DSL and the per-caller counter fixture; revised the actor
  state machine to rebase on known-committed writes (not whole-workload success)
  with a `NeedsReconcile` pre-read for receipt-unknown/restart-aborted writes.
- [x] Slice L2: per-caller `caller-counter` contract added to
  `../groundhog-examples` (compiles; selectors bump=`0x4fbf8bcef9232a3a`,
  get=`0xdfc5d5bd6c88f8c4`, derivation verified against fibonacci
  compute=`0xc573d6c80bb945ad`); caller identity verified at code level (both
  paths resolve to `actor.wallet_address`); manifest `readAfterWrite` +
  `read-after-write` workload kind + validation + the `ReadAfterWriteState`
  machine (`NeedsReconcile`/`Known(u64)`) landed with unit tests. Contract setup
  moved to L3 (it is consumed by the executor; standalone it would be dead code).
  `.wasm` artifact build + live caller-identity check deferred to the L4 smoke.
- [x] Slice L3: contract setup (`PreparedReadAfterWrite`) plus executor
  reconcile -> write -> await -> read -> assert, additive artifacts, and the
  `read-after-write-mismatch` error class. Rebases actor state when the write is
  known committed; keeps `Known` on a post-commit read RPC failure; sets
  `NeedsReconcile` on mismatch or unknown commit status. Read-back decodes the
  CXS `u64` via `xdrs::cxs::decode`. Six mock-client executor tests cover the
  transition matrix; clippy/fmt/tests green.
- [ ] Slice L4: read-after-write catalog scenario + smoke coverage (builds the
  `caller-counter` `.wasm`, runs the live `groundhog_view` caller-identity
  check) + docs.
- [ ] Slice L4: add the read-after-write catalog scenario, smoke coverage, and
  docs. (Covers "Add richer actor state machines" and "Add read-after-write
  scenario presets".)
- [x] Add positive event-indexing scenarios with contracts that emit events
  during scheduled load.
- [ ] Add storage-heavy, event-heavy, calldata-heavy, and compute-heavy
  workloads.
- [ ] Add NNInt-specific scenario presets.

## Phase 4: Single Remote Host

- [ ] Add `remote-ssh` target provider.
- [ ] Support manually provisioned EC2 or bare-metal hosts.
- [ ] Start or discover `hogd-dev` on one host.
- [ ] Fetch runtime manifest and logs.
- [ ] Preserve target ownership metadata.
- [ ] Implement restart-target chaos over SSH.

## Phase 5: Infra Provider Hooks

- [ ] Stabilize provider interfaces for future infra-backed targets.
- [ ] Add deployment identity fields where useful.
- [ ] Keep simulator core independent of OpenTofu state.
- [ ] Evaluate whether `ops` / `hogctl` should provide a target provider
  adapter or remain a separate orchestration layer.
