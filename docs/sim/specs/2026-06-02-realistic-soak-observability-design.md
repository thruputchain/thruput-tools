# Realistic Soak + Local Observability (Phase 6) - Design

**Date:** 2026-06-02
**Status:** Draft. Proposes a new Phase 6 ahead of the remaining Phase 4
(`remote-ssh`) and Phase 5 (infra provider hooks) work. Phases 0-3 are complete;
this phase deliberately deprioritizes remote infrastructure in favor of making
the existing local/existing-target path able to run trustworthy, diagnostic
multi-hour soaks for memory-leak and performance-regression hunting.

This pairs with [Simulation design](../sim-design.md) (workload model, target
boundary, chaos taxonomy) and is tracked slice-by-slice in
[Simulation implementation plan](../sim-implementation-plan.md).

## Goal

Make `hogsim` able to generate long, realistic, multi-contract / multi-actor
**pure JSON-RPC traffic** against the **local** provider and produce a result
artifact that can detect slow leaks, simulator overload, and throughput
regressions rather than merely fall over when one happens.

The `existing` provider remains useful for traffic and assertion coverage, but
resource-leak detection is only available when the simulator has an explicit
process identity to sample. Today that means the simulator-owned `local`
provider.

Five capabilities, in priority order:

1. **Soak substrate hardening** — remove simulator-side unbounded buffers and
   avoid a fully materialized multi-hour schedule becoming the leak under test.
2. **Restart-aware target resource sampling** — sample the local target process
   group and embed the RSS/CPU/thread/fd curve in the run artifact.
3. **Regression gates** — fail saturated or leaking runs via explicit
   backpressure, simulator-overload, and RSS-growth assertions.
4. **Multi-contract corpus + hot-contract / hot-account traffic** — use the
   manifest's existing `Vec` surfaces with deterministic weighted skew.
5. **Long-soak presets + calibration path** — ship runnable presets that are
   kept out of fast smoke by default and can be reduced for optional CI smoke.

## Why Local-First, Not a Full Observability Stack

The deployed "validator" topology (full `hogd*` suite + `chuckd` coordinator +
`json-rpc-gateway` on EC2) will carry a Prometheus / Loki / Grafana stack, and
soak metrics there belong in that stack. This phase is **not** trying to
reimplement that.

The value here is the local loop: a developer (or CI) soaking `hogsim` at the
local provider should get leak/regression signal **in the run artifact itself**,
with no metrics infra to stand up. A self-contained RSS/CPU sampler in
`result.json` is the right altitude for "build up from the local provider before
needing the full stack." When the EC2 target lands, the same sampler can remain
a coarse cross-check only if that provider exposes a PID/process group or an
exporter URL; richer signal comes from scraping the real exporters.

**Design rule:** the sampler is best-effort and platform-gated. It never blocks a
run, never becomes a hard dependency, and degrades to `unavailable` (the existing
`MetricAvailability` mechanism) on any platform or target where it cannot read
process stats. No new external crates for the metrics backend; no scrape server.

**Traffic boundary:** `hogsim`'s durable job is to generate deterministic
JSON-RPC traffic, record what happened, and evaluate artifact-local assertions.
Process sampling and heap/profile adjuncts are diagnostics for targets that the
local provider owns. They must not turn the simulator into a replacement for the
deployed multi-process observability stack.

## Relationship to Deterministic Simulation Testing

Deterministic simulation testing (DST) in the style of FoundationDB,
TigerBeetle, sled, `madsim`, or `mysten-sim` is complementary to this phase, not
a replacement for it.

DST is the right long-term tool for finding scheduling, networking, storage, and
consensus logic bugs by running many seeds under a simulated clock, simulated IO,
and controlled task scheduling. It is especially compelling once the production
coordinator and node-facing components have Rust-native simulation seams.

Phase 6 has a different target: run the real binaries, with their real
allocators, real process boundaries, and real wall-clock behavior. That is the
only altitude that can catch the leak and wall-clock regression class this phase
is about. A future DST effort should get its own design doc; it should not
displace the local soak harness.

The implementation boundary matters:

- this repository's JSON-RPC gateway and developer supervisor code are Rust;
- `chuckd` is currently Go, with a Rust port in progress;
- `hogd-dev` and the full `hog*` process suite are C++.

That mixed-language target makes a single Rust simulation runtime insufficient
for end-to-end validator realism today. The local soak harness remains valuable
because it treats the target as a real process group.

## Design Decision: Harden the Simulator Before Trusting the Signal

The first version of this plan put target sampling before simulator buffer
hardening. That ordering is unsafe for true multi-hour soaks: a simulator memory
curve is not diagnostic while the simulator still retains unbounded observations,
pending receipt queues, or schedule entries.

Phase 6 therefore starts by bounding the simulator itself. The target sampler can
be developed in parallel with this slice, but the first long-soak preset must not
land until the simulator has:

- a bounded or streaming schedule representation for long runs;
- streaming chain-progress stall computation with only a capped public sample
  window retained;
- capped per-actor pending-receipt tracking with explicit overload accounting.

Short local smokes may exercise a partial sampler earlier, but those smokes are
not evidence that a multi-hour soak is trustworthy.

## Design Decision: Sample the Local Process Group, Not Just the Node PID

The `local` provider owns the `groundhog node` child handle in provider-owned
state. That node supervises `hogd-dev`, and the local target's leak surface is
the combined process group, not only the supervisor process.

- **`local` provider:** sample the currently owned process group rooted at the
  live `groundhog node` child. On Unix this uses the captured process-group id
  (`pgid`) and sums stats for readable processes in that group. Each sample
  records `scope: "process-group"`, `rootPid`, `pgid`, and `processCount`.
  If process-group aggregation is not available, the sampler may degrade to
  `scope: "node-process"` only when the artifact records that narrower scope.
- **Restart awareness:** the sampler must not be bound to the first PID. It must
  read the provider-owned live state at sample time, or use shared sampler state
  that restart chaos updates when `LocalTargetProvider::restart_target` replaces
  the child. Samples after restart should show the new `rootPid`/`pgid`.
- **`existing` provider:** resource sampling is `unavailable` unless a future
  provider supplies an explicit PID/process group or exporter URL. Do not guess
  a PID by port, process name, command line, or runtime-manifest path.
- **Reading stats:** prefer `/proc/<pid>/stat`, `/proc/<pid>/status`, and
  `/proc/<pid>/fd` on Linux (the EC2 target's OS and the likely CI runner OS).
  macOS dev loops may return `unavailable`; that is acceptable for the leak
  signal that matters. Keep platform reads behind one small internal module so
  the executor and result layers stay platform-agnostic.

## Design Decision: Sampling Clock and Schedule Volume

Sampling is driven by the same simulation clock the scheduler/executor already
use, with resource ticks interleaved in deterministic order. The sampler must
not perturb workload dispatch: resource reads run as scheduled non-workload
events and are recorded without acquiring workload permits or contributing to
workload counts.

The sample interval is additive manifest configuration:

```json
{
  "observability": {
    "resourceSampleInterval": "5s"
  }
}
```

If absent, `hogsim` uses `5s`. `observability` is optional and must be
`#[serde(default, skip_serializing_if = "Option::is_none")]` so existing
scenarios normalize exactly as they do today rather than gaining
`"observability": {}`. The resolved interval is recorded in `result.json` as
`sampleIntervalMs`.

Resource events should not force an otherwise-streamable long soak to allocate a
larger in-memory schedule. Slice 1 introduces a deterministic schedule cursor
for long runs. `generate_schedule` can remain as a small-run/test collection
helper, but production execution should iterate events in order instead of
retaining every actor tick and resource tick for a multi-hour run.

The cursor is a k-way merge, not a new single loop. It merges independently
ordered sub-streams:

- phase/actor workload ticks;
- chaos events;
- resource samples.

The merged order must be byte-for-byte equivalent to the current
`generate_schedule` + global `sort_by` ordering when resource samples are absent.
With resource samples present, the internal rank tuple shifts from
`chaos=0, actor=1` to `chaos=0, resource=1, actor=2`. The full comparison key is:

```text
(simulationTimeMs, eventRank, phaseIndex, actorId, actorSequence or eventIndex)
```

The actor sub-stream must preserve the existing actor tie-break exactly. Do not
assume a simple actor-sequence-outer loop is equivalent at very high rates where
multiple actor sequences can land in the same millisecond.

Deterministic ordering for events with equal `simulationTimeMs`:

1. chaos transition trigger;
2. resource sample;
3. actor ticks, ordered by phase, actor id, actor sequence.

This makes restart windows visible while preserving stable actor ordering.

## Result and Event Shape

`result.json` gains an additive field:

```json
{
  "resourceTimeline": {
    "status": "observed",
    "value": {
      "scope": "process-group",
      "sampleIntervalMs": 5000,
      "sampleCount": 720,
      "first": {
        "simulationTimeMs": 0,
        "rootPid": 12345,
        "pgid": 12345,
        "processCount": 2,
        "rssBytes": 104857600,
        "cpuUserMs": 120,
        "cpuSystemMs": 40,
        "threads": 22,
        "openFds": 88
      },
      "last": {
        "simulationTimeMs": 3600000,
        "rootPid": 12501,
        "pgid": 12501,
        "processCount": 2,
        "rssBytes": 134217728,
        "cpuUserMs": 32000,
        "cpuSystemMs": 11210,
        "threads": 24,
        "openFds": 96
      },
      "maxRssBytes": 134217728,
      "maxSegmentRssGrowthBytesPerMinute": 524288,
      "lastSegmentRssGrowthBytesPerMinute": 131072,
      "peakThreads": 28,
      "peakOpenFds": 112,
      "totalCpuMs": 43210,
      "segments": [
        {
          "scope": "process-group",
          "rootPid": 12345,
          "pgid": 12345,
          "sampleCount": 360,
          "startedAtUnixMs": "1780000000000",
          "endedAtUnixMs": "1780001800000",
          "firstRssBytes": 104857600,
          "lastRssBytes": 120586240,
          "maxRssBytes": 121110528,
          "rssGrowthBytesPerMinute": 524288,
          "totalCpuMs": 21000
        }
      ],
      "samples": []
    }
  }
}
```

The public `samples` window is capped, initially with the same 1,024-sample cap
used for chain progress. At the default `5s` interval that preserves about
85 minutes of raw points; longer soaks keep the full aggregate/segment math but
not a full plottable raw curve. A downsampled retained window can be added later
if a UI needs multi-hour shape in the artifact. The aggregator preserves
`first`, `last`, max RSS, peak threads, peak fds, total sample count, per-segment
CPU, and RSS-growth math across all samples even after the public sample window
is full.

`result.jsonl` gains a `resource-sample` event. The event records the scheduled
simulation time, wall-clock timestamps, sample scope, root process identity, and
either an observed sample or an unavailable reason. New event variants inherit
`schemaVersion` through the existing `EventWriter` wrapper.

Resource samples are best-effort point-in-time snapshots. Process-group
enumeration is inherently racy: a process can fork or exit while the sampler is
walking `/proc`, and PID reuse is possible. Processes that disappear mid-sample
are dropped from that sample. `processCount` is informational and is not a
correctness invariant or compare metric.

Availability is per field where practical. For example, `/proc/<pid>/fd`
enumeration may fail while `/proc/<pid>/stat` and `/proc/<pid>/status` succeed;
that sample should still contribute RSS/CPU/thread observations while recording
`openFds` as unavailable. A transient fd-read failure must not punch a hole in
the RSS curve.

RSS growth is expressed first as absolute bytes per minute:

```json
{
  "assertions": {
    "maxRssGrowthBytesPerMinute": 1048576
  }
}
```

The assertion is `unsupported` when fewer than two observed resource samples are
available in any segment. Fraction-of-baseline assertions can be added later
after real soak baselines show whether absolute thresholds are too noisy.

Restart chaos creates resource segments. A segment starts when the sampled
`rootPid`/`pgid` changes, and a non-restarted run has exactly one segment.
RSS-growth math uses wall-clock elapsed time between observed samples, not
simulation time, because allocator growth is a real-time phenomenon and the
simulation clock can lag wall-clock under saturation. `maxRssGrowthBytesPerMinute`
evaluates the worst segment's wall-clock growth. `lastSegmentRssGrowthBytesPerMinute`
is recorded for diagnosis but is not the default assertion target.

CPU counters are per process lifetime and reset on restart. `totalCpuMs` is the
sum of per-segment CPU deltas, computed from monotonic counters within each
segment. It is not `last.cpuUserMs + last.cpuSystemMs` when restarts occur.

## Regression and Overload Semantics

Backpressure and simulator overload are separate signals:

- **`simulator-backpressure`** means a scheduled workload was skipped because a
  global or per-actor in-flight limit was reached. Per-actor diagnostics name
  the lane (`actor-write`, `actor-receipt`, or `actor-read`). It contributes to
  `backpressureRate`.
- **`simulator-overload`** means the simulator intentionally stopped tracking
  some internal state to preserve bounded memory, currently per-actor pending
  receipts. It contributes to `simulatorOverloadRate` and should be zero for
  reference soaks.

Both rates use the same denominator: manifest-scheduled actor ticks for the run,
equal to completed + failed + skipped scheduled workload ticks. Resource samples,
chaos events, setup work, and tail receipt-drain polls are excluded. The
backpressure numerator is scheduled workload ticks skipped with
`errorClass: "simulator-backpressure"`. The simulator-overload numerator is
overload incidents associated with scheduled workload ticks. If the denominator
is zero, these assertions are `unsupported`.

`maxBackpressureRate` and `maxSimulatorOverloadRate` are additive assertions.
If configured and the observed count is zero, the assertion passes with
`observed: "0.000000"`; zero backpressure is not an unsupported condition.

Reference long-soak presets should set:

```json
{
  "assertions": {
    "maxBackpressureRate": 0.001,
    "maxSimulatorOverloadRate": 0.0,
    "maxReceiptTimeoutRate": 0.001,
    "maxRssGrowthBytesPerMinute": 1048576
  }
}
```

The exact RSS threshold must be calibrated from at least one baseline soak on
the same target class before it becomes a CI gate.

## Optional Attribution and Perf Adjuncts

RSS/process sampling answers **whether** the target process group is growing. It
does not explain **where** allocations come from. Phase 6 keeps attribution
optional and target-specific so the core JSON-RPC traffic harness does not
depend on one language runtime or allocator. These hooks are most useful during
simplified localhost runs and can be expanded later when `hogsim` is pointed at
the real deployed multi-process system.

- **Heap attribution:** add an opt-in local-provider diagnostic environment only
  when the selected target binary supports it. For jemalloc-linked binaries,
  `MALLOC_CONF` profiling can emit heap profiles beside the soak artifact. Do
  not assume this applies to the current target: the Rust gateway, Go `chuckd`,
  and C++ `hogd-dev` may use different allocators and profiling mechanisms.
  Unsupported attribution should be recorded as unavailable, not treated as a
  run failure. A quick check of the current sibling `hogd` checkout found
  sanitizer-related notes but no obvious committed jemalloc/tcmalloc/mimalloc
  hook for `hogd-dev`; confirm against the exact target build before adding a
  `MALLOC_CONF` preset.
- **Rust instruction-count benchmarking:** `iai-callgrind` is useful for
  deterministic instruction-count regression tests of Rust libraries and
  binaries, including gateway or simulator hot paths. It is not a substitute for
  soak testing the C++/Go process group, but it is a strong local/CI adjunct for
  the Rust parts of this repo.
- **Continuous benchmark tracking:** Bencher or CodSpeed-style tracking belongs
  to the CI-gating decision, not the Phase 6 core. The Phase 6 job is to produce
  stable artifacts and local gates; publishing benchmark histories can come
  after the benchmark set and thresholds are credible.

## Phase 6 Slices

Slices are ordered. Slices 1-3 are the minimum to make a multi-hour soak both
trustworthy and diagnostic. Slice 4 makes the traffic representative. Slice 5
ships human-friendly presets and calibration scaffolding. Each non-trivial slice
writes its own dated handoff spec under `docs/sim/specs/` before code (matching
the Phase 2/3 cadence).

### Slice 1: Soak Substrate Hardening

**Goal:** ensure the simulator remains bounded before asking it to detect target
leaks.

**Files:**
- Modify: `crates/dev/sim/src/scheduler.rs` — add a deterministic schedule
  cursor used by production execution so long soaks do not require a fully
  materialized `Vec<ScheduledSimulationEvent>`. The first hardening slice
  streams workload and chaos events while keeping collection helpers for tests
  and small smokes; resource-sample events join the cursor in Slice 2.
- Modify: `crates/dev/sim/src/run.rs` + `executor.rs` — execute via the cursor
  while preserving phase-start handling, deterministic actor ordering, chaos
  ordering, and scheduled-workload counts.
- Modify: `crates/dev/sim/src/metrics.rs` — replace
  `ChainProgressBuilder`'s uncapped `Vec<ChainTipObservation>` with streaming
  max-stall computation, keeping the public capped timeline.
- Modify: `crates/dev/sim/src/executor.rs` + `actor.rs` — cap each actor's
  pending-receipt tracking. When submit rate outruns receipt capacity, emit
  `simulator-overload` and stop adding to the queue instead of growing it.
- Modify: `crates/dev/sim/src/result.rs` + `event.rs` — surface overload counts
  and events without changing existing workload event shapes.

**Verify:**
- Unit: schedule cursor emits the same ordered events as `generate_schedule` for
  existing scenarios, including a multi-phase case with chaos at the same
  timestamp as actor ticks; streaming max-stall preserves uncapped stall
  semantics; pending-receipt cap emits overload and stops growing.
- Smoke: existing local smokes still produce byte-stable deterministic event
  ordering where snapshots assert it.

### Slice 2: Restart-Aware Target Resource Sampler

**Goal:** embed a target process-group memory/CPU timeline in the run artifact
for `local` targets, behind `MetricAvailability`.

**Files:**
- Create: `crates/dev/sim/src/resource.rs` — platform read of `(rss_bytes,
  cpu_user_ms, cpu_system_ms, num_threads, open_fds)` for a PID or process
  group; Linux `/proc` path plus a `cfg`-gated fallback returning unavailable.
  No external crates.
- Modify: `crates/dev/sim/src/manifest.rs` + validation — add optional
  `observability.resourceSampleInterval` with default `5s`.
- Modify: `crates/dev/sim/src/scheduler.rs` — add `ResourceSample` events to
  the schedule cursor with stable ordering after chaos and before actor ticks.
- Modify: `crates/dev/sim/src/target.rs` — expose a restart-aware
  `resource_sampler()` hook. `LocalTargetProvider` samples current owned state;
  `existing` returns `None`.
- Modify: `crates/dev/sim/src/executor.rs` — handle resource sample events:
  read the sampler, emit `resource-sample`, and feed the aggregator without
  acquiring workload permits or blocking workload dispatch.
- Modify: `crates/dev/sim/src/metrics.rs` + `result.rs` — add
  `MetricAvailability<ResourceTimeline>` with first/last/max RSS, RSS growth,
  peak threads/fds, total CPU, total samples, and capped sample window.
- Modify: `crates/dev/sim/src/event.rs` — add the `resource-sample` event
  variant.
- Optional: add a local-provider diagnostic-env hook for allocator heap profiles
  when a target binary explicitly supports it. v1 may document the hook without
  enabling it by default; allocator-specific output is an adjunct artifact, not
  part of the core `resourceTimeline` contract. For `hogd-dev`, first verify the
  target build's allocator or sanitizer mode rather than assuming jemalloc.

**Verify:**
- Unit: `/proc` parser against fixed fixture strings; process-group aggregation
  sums multiple fixture processes; fallback returns unavailable; restart-aware
  sampler changes PID after provider state changes; per-field availability keeps
  RSS/CPU when fd enumeration fails; timeline segment/slope/peak/CPU math
  preserves first/last/max after the sample cap and across restart boundaries.
- Smoke: extend `scripts/smoke-sim-local.sh` / local scenario smoke to assert
  `result.json` carries `resourceTimeline.status == "observed"` with at least
  two samples for a multi-second local run on Linux. Non-Linux smoke accepts
  `unavailable` with a reason.

### Slice 3: Regression Gates

**Goal:** make saturation, simulator overload, and RSS growth observable and
assertable.

**Files:**
- Modify: `crates/dev/sim/src/manifest.rs` — add `maxBackpressureRate`,
  `maxSimulatorOverloadRate`, and `maxRssGrowthBytesPerMinute` to
  `AssertionSpec` (additive; absent means unasserted).
- Modify: `crates/dev/sim/src/metrics.rs` + `result.rs` — surface
  `backpressureRate` and `simulatorOverloadRate` in `result.json`.
- Modify: `crates/dev/sim/src/assertions.rs` — evaluate the three new
  assertions, recording checks in `result.jsonl` like existing assertions and
  failing the run / exit code when exceeded.
- Modify: `crates/dev/sim/src/compare.rs` — compare RSS growth, peak RSS, total
  CPU, backpressure rate, and overload rate as numeric deltas; skip resource
  deltas when either side is unsupported or unavailable.

**Verify:**
- Unit: `maxBackpressureRate` and `maxSimulatorOverloadRate` pass at zero, pass
  below threshold, fail above threshold; `maxRssGrowthBytesPerMinute` is
  unsupported with fewer than two observed samples in any segment and
  fails/passes against the worst wall-clock segment with enough samples; compare
  numeric-skip behavior for unavailable resource timelines.

### Slice 4: Multi-Contract Corpus + Hot Contract / Hot Account

**Goal:** exercise the already-present `Vec` manifest fields with more than one
contract and a skewed access distribution, so soaks resemble real validator
traffic and can surface fairness/isolation issues.

The manifest already accepts multiple `contractArtifacts`, `contractCalls`,
`viewCalls`, `storageReads`, and `eventQueries`; no scenario uses more than one
contract artifact. This slice is scenario + selection work plus a small
additive schema for target weights.

**Files:**
- Modify: `crates/dev/sim/src/manifest.rs` — add optional per-call/per-query
  `weight` fields, defaulting to `1`, for `contractCalls`, `viewCalls`,
  `storageReads`, `eventQueries`, and `readAfterWrite`. Add an explicit
  hot-account distribution only if the current static actor/account mapping is
  insufficient for the desired traffic model.
- Modify: `crates/dev/sim/src/contract.rs` — deploy-at-start support for a
  contract corpus (N artifacts), keyed by name, with terminal deployment records
  per contract.
- Modify: `crates/dev/sim/src/scheduler.rs` / `executor.rs` — when a workload
  kind has multiple prepared targets, pick the target entry by a seeded weighted
  distribution derived from seed, scenario name, workload kind, phase, actor id,
  and actor sequence. Keep selection pure (no stateful RNG).
- Create: `examples/sim/local-multi-contract-soak.json` — deploys existing
  fibonacci + caller-counter + events fixtures and drives a mixed read/write/
  events workload across them with skewed target distribution.
- Modify: `examples/sim/catalog.json` + `crates/dev/sim/src/catalog.rs` tests —
  add the corpus scenario with `multi-contract` / `hot-contract` / `soak` tags.

**Verify:**
- Unit: corpus deploy produces one deployment record per artifact; weighted
  target selection is deterministic for a fixed seed and matches configured
  skew within tolerance over many ticks; default weight preserves existing
  single-target scenario behavior.
- Smoke: scenario-catalog smoke deploys more than one contract and records calls
  against each.

### Slice 5: Long-Soak Presets + Calibration

**Goal:** ship runnable multi-minute and multi-hour soak presets without making
fast local smokes expensive.

**Files:**
- Create: `examples/sim/local-realistic-soak.json` — a multi-minute mixed
  read/write/receipt/events soak with `maxBackpressureRate`,
  `maxSimulatorOverloadRate`, `maxReceiptTimeoutRate`, and
  `maxRssGrowthBytesPerMinute` wired.
- Create: `examples/sim/local-realistic-soak-short.json` or add a script-level
  duration override path for optional reduced-duration smoke.
- Modify: `examples/sim/catalog.json` — add the soak preset with `soak` /
  `regression` tags; keep it out of the fast smoke default.
- Modify: `scripts/smoke-sim-scenarios.sh` — optional soak mode runs the reduced
  duration preset and asserts `backpressureRate`, `simulatorOverloadRate`, and
  `resourceTimeline` appear in `result.json`.
- Add docs note: RSS thresholds are calibrated per target class from baseline
  artifacts before being promoted to mandatory CI gates.
- Add docs note: Rust instruction-count benchmarks (`iai-callgrind`) and
  continuous tracking (Bencher/CodSpeed-style) are candidates for CI perf gates
  after Phase 6 establishes the local artifact contract.

**Verify:**
- Smoke: optional/soak path runs the preset at reduced duration and emits the
  full diagnostic surface.
- Manual baseline: one real long soak produces a baseline artifact used to tune
  the reference `maxRssGrowthBytesPerMinute` threshold.

## Explicitly Out of Scope for Phase 6

- **Prometheus / Loki / Grafana integration.** That belongs to the deployed
  validator topology and its observability stack, not the local simulator.
- **`remote-ssh` provider (Phase 4)** and **infra provider hooks (Phase 5).**
  The chaos controller trait and `restart-target` already exist for the local
  provider; remote-ssh re-tunnels existing capability over SSH and stays
  deferred behind this phase.
- **Network-condition / resource-pressure chaos families.** Listed in the design
  doc's future chaos order; not required to detect leaks under steady multi-hour
  load, which is this phase's wedge. If local network chaos becomes a priority,
  evaluate `turmoil` for Rust in-process network/filesystem hardship before
  reaching for cluster-level tools.
- A trait-registry **workload extension API.** The closed enum is still
  the right size; corpus targeting is data, not a new extension surface.
- **PID guessing for existing targets.** A future provider can accept an
  explicit PID/process group or exporter URL, but this phase does not infer one.

## Artifact Compatibility

All new `result.json` fields are additive and carry `MetricAvailability` where
target support may be absent. New `result.jsonl` event variants inherit
`schemaVersion` through the existing `EventWriter` wrapper. `compare` skips
resource numeric deltas whenever either side is unsupported or unavailable,
matching current behavior. Existing baselines and scenario smokes remain valid.

## Open Questions

- **Process-group aggregation details on non-Linux:** macOS can remain
  unavailable for v1. If local macOS signal becomes important, add a small
  platform reader later rather than complicating the first Linux path.
- **Hot-account schema:** first try skewing target contracts/calls while keeping
  the current static actor/account mapping. Add explicit account skew only if
  landscape analysis shows account locality is a first-order behavior to model.
- **CI cadence:** Phase 6 ships assertions and presets; whether a full soak runs
  in CI (and at what duration/cadence) is a separate ops decision. Reduced
  optional smoke should exist before any mandatory long-run gate.
- **Heap attribution surface:** decide per target binary which profiler, if any,
  is available. Rust/jemalloc, Go runtime profiling, and C++ allocator tooling
  should be integrated as optional artifact producers, not as a single generic
  Phase 6 dependency.
