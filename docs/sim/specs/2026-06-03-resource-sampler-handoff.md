# Restart-Aware Target Resource Sampler (Phase 6, Slice 2) - Handoff

**Date:** 2026-06-03
**Status:** Implemented. Slice 1 hardening is landed, and Slice 2 now records
resource-sample events plus a best-effort `resourceTimeline` for provider-owned
local targets.

## Goal

Embed a best-effort target resource timeline in the local run artifact so a
developer running `hogsim` against the simulator-owned local target can see RSS,
CPU, thread, and fd behavior without standing up Prometheus or the deployed
validator observability stack.

The sampler is diagnostic. It must not change workload dispatch, make `existing`
targets guess process identity, or turn `hogsim` into a general process monitor.
The durable simulator job remains deterministic JSON-RPC traffic generation plus
artifact-local assertions and summaries.

## Non-Negotiable Semantics

- **Local-only v1:** `local` samples the currently owned process group rooted at
  the live `groundhog node` child. `existing` returns unavailable. Do not infer
  by port, command line, manifest path, or process name.
- **Best effort:** sampling never fails the run. Unsupported platform,
  permission errors, races, exited processes, and partial read failures become
  unavailable fields or unavailable samples.
- **Restart-aware:** segment resource math when `(scope, rootPid, pgid)` changes.
  Restart chaos must not let one first/last RSS slope hide a leak inside a
  post-restart segment.
- **Wall-clock growth:** RSS growth is bytes per wall-clock minute, not
  simulation-clock minute. Record both scheduled simulation time and wall-clock
  timestamps on raw samples.
- **Per-field availability:** keep RSS/CPU/thread observations when fd reads
  fail. Do not discard a whole sample because one field was unavailable.
- **No workload interference:** resource ticks do not acquire workload permits,
  do not count as scheduled workloads, and do not affect error-rate or latency
  metrics.
- **Stable ordering:** when resource ticks are added, equal simulation times are
  ordered `chaos -> resource -> actor`. Existing actor tie-breaks remain
  unchanged.

## Result Shape

Add a top-level additive field to `SimResult`:

```rust
pub resource_timeline: MetricAvailability<ResourceTimeline>,
```

Use a default of `MetricAvailability::Unavailable { reason: ... }` so old
`result.json` files still deserialize. Do not put this inside
`SimMetricAvailability`; that type is currently a narrow record of protocol
metrics that are present/absent independently of the local target sampler.

Suggested structs:

```rust
struct ResourceTimeline {
    scope: ResourceSampleScope,
    sample_interval_ms: u64,
    sample_count: u64,
    first: ResourceSamplePoint,
    last: ResourceSamplePoint,
    max_rss_bytes: Option<u64>,
    max_segment_rss_growth_bytes_per_minute: Option<i64>,
    last_segment_rss_growth_bytes_per_minute: Option<i64>,
    peak_threads: Option<u64>,
    peak_open_fds: Option<u64>,
    total_cpu_ms: Option<u64>,
    segments: Vec<ResourceSegment>,
    samples: Vec<ResourceSamplePoint>,
}

struct ResourceSamplePoint {
    simulation_time_ms: u64,
    observed_at_unix_ms: String,
    scope: ResourceSampleScope,
    root_pid: u32,
    pgid: Option<u32>,
    process_count: u32,
    rss_bytes: Option<u64>,
    cpu_user_ms: Option<u64>,
    cpu_system_ms: Option<u64>,
    threads: Option<u64>,
    open_fds: Option<u64>,
}
```

Use `Option` for per-field availability in observed points. Use
`MetricAvailability<ResourceTimeline>` for whole-timeline availability.
`processCount` is informational only; compare/assertion code must not treat it
as a correctness invariant.

Cap the public `samples` window at 1,024 points. Aggregates, first/last,
segments, peaks, and total sample count must remain exact across all observed
samples after the cap is reached.

## Event Shape

Add `result.jsonl` variant `resource-sample`. It should include:

- `scheduledTimeMs`
- `observedAtUnixMs`
- `status`: `observed` or `unavailable`
- for observed samples: the same sample-point fields recorded in the result
  timeline
- for unavailable samples: a reason string

Use the existing `EventWriter` version wrapper so `schemaVersion` is included
automatically.

## Implementation Task List

### Agent A: Manifest + Scheduler

Files:

- `crates/dev/sim/src/manifest.rs`
- `crates/dev/sim/src/validation.rs`
- `crates/dev/sim/src/scheduler.rs`
- manifest parse/validation tests

Tasks:

- [ ] Add `ObservabilitySpec` to `SimulationManifest`:
  `#[serde(default, skip_serializing_if = "Option::is_none")] pub observability:
  Option<ObservabilitySpec>`.
- [ ] Add optional `resourceSampleInterval: DurationSpec`; default to `5s` at
  execution time when `observability` or the field is absent.
- [ ] Validate positive integer-millisecond intervals using the existing
  `DurationSpec` parser. Reject zero/invalid strings through the existing
  validation style.
- [ ] Add `ScheduledSimulationEvent::ResourceSample(ScheduledResourceSample)`.
  Include `resource_sample_index` and `simulation_time_ms`.
- [ ] Extend `ScheduleCursor` with a regular resource stream. It should not
  allocate every sample for multi-hour runs.
- [ ] Preserve deterministic merge order:
  `(simulationTimeMs, rank, tieBreaker)` where ranks are chaos `0`, resource
  `1`, actor `2`.
- [ ] Update `generate_schedule` only as the test/small-run oracle. It may
  materialize resource ticks for tests; production must remain cursor-based.
- [ ] Update `schedule_summary` so shutdown drain includes the last resource
  tick if it is later than the last workload/chaos tick.

Verify:

- [ ] Existing schedule cursor parity tests still pass with no observability
  config.
- [ ] New cursor/generate parity test with resource interval, chaos, multiple
  phases, and actor ticks at the same timestamp.
- [ ] Manifest normalization test confirms omitted `observability` stays absent,
  not serialized as `{}`.
- [ ] A long-duration schedule test confirms cursor construction does not
  allocate a vector proportional to resource sample count.

### Agent B: Platform Resource Reader

Files:

- create `crates/dev/sim/src/resource.rs`
- update `crates/dev/sim/src/lib.rs`

Tasks:

- [ ] Define a small public-in-crate sampler API that can sample either a root
  process or a process group:
  `sample_process_group(root_pid, pgid, scheduled_time_ms, now)`.
- [ ] On Linux, implement `/proc` readers using only std/tokio as appropriate:
  `/proc/<pid>/stat`, `/proc/<pid>/status`, and `/proc/<pid>/fd`.
- [ ] Convert RSS pages to bytes using system page size. If avoiding `libc`
  here is awkward, use the crate's existing `libc` dependency rather than adding
  a new crate.
- [ ] Convert CPU ticks to milliseconds using `_SC_CLK_TCK`.
- [ ] Enumerate process groups by walking `/proc/[0-9]+` and reading each
  candidate's pgid. Drop processes that disappear mid-sample.
- [ ] Keep per-field failures local. Example: fd enumeration failure yields
  `open_fds: None` while RSS/CPU/thread fields still contribute.
- [ ] On non-Linux, return unavailable with a stable reason.

Verify:

- [ ] Unit-test parsers against fixture strings for `stat` and `status`.
- [ ] Unit-test aggregation across multiple fixture processes.
- [ ] Unit-test partial fd failure keeps RSS/CPU/thread fields.
- [ ] Unit-test non-Linux fallback shape behind `cfg` where practical.
- [ ] Unit-test races by making fixture process reads disappear mid-sample.

### Agent C: Target Provider Hook

Files:

- `crates/dev/sim/src/target.rs`
- `crates/dev/sim/src/run.rs`

Tasks:

- [ ] Add an optional `resource_sampler()` hook to `TargetProvider`. Default is
  `None`.
- [ ] Add a trait or handle type that executor can call without depending on
  `LocalTargetProvider` internals.
- [ ] For `ExistingTargetProvider`, leave the default `None`.
- [ ] For `LocalTargetProvider`, implement a sampler that locks current state at
  sample time and reads the live child PID plus current captured process-group
  identity. Do not capture only the first PID.
- [ ] Update restart handling so sampler-visible state changes when
  `restart_target` replaces the child. The existing ownership refresh path is
  the reference behavior.
- [ ] Pass the optional sampler into `execute_loaded_simulation`.

Verify:

- [ ] Unit-test local sampler returns unavailable before acquire/after cleanup.
- [ ] Unit-test sampler reports a different root PID after restart state is
  refreshed, using existing local-provider restart test patterns.
- [ ] Unit-test existing provider has no sampler and produces unavailable
  resource timeline.

### Agent D: Executor + Aggregator + Result

Files:

- `crates/dev/sim/src/executor.rs`
- `crates/dev/sim/src/metrics.rs`
- `crates/dev/sim/src/result.rs`
- `crates/dev/sim/src/event.rs`
- `crates/dev/sim/src/run.rs`

Tasks:

- [ ] Add result structs for `ResourceTimeline`, `ResourceSamplePoint`,
  `ResourceSegment`, and `ResourceSampleScope`.
- [ ] Add `ResultEvent::ResourceSample`.
- [ ] Add `ResourceTimelineBuilder` in metrics or a small adjacent module. It
  should preserve exact aggregates after the public sample cap.
- [ ] Segment on `(scope, rootPid, pgid)`.
- [ ] Compute segment RSS growth using wall-clock elapsed between first and last
  observed RSS sample in that segment. Return `None` if fewer than two usable
  RSS points or zero elapsed wall-clock.
- [ ] Compute `totalCpuMs` as the sum of monotonic per-segment CPU deltas. Do
  not subtract across restart boundaries.
- [ ] Track `maxRssBytes`, `peakThreads`, `peakOpenFds`, first/last observed
  sample, sample count, and capped sample window.
- [ ] In executor, handle resource events by sampling and writing
  `resource-sample` without acquiring workload permits.
- [ ] Whole-run timeline availability:
  - sampler absent: unavailable
  - sampler present but no observed sample: unavailable
  - at least one observed sample: observed

Verify:

- [ ] Unit-test sample cap preserves aggregate math after 1,024 raw samples.
- [ ] Unit-test restart segmentation with sawtooth RSS does not hide growth in
  either segment.
- [ ] Unit-test CPU counters resetting on restart still sum per-segment deltas.
- [ ] Unit-test fd-only failures do not break RSS slope.
- [ ] Unit-test `result.json` deserializes old fixtures with default unavailable
  resource timeline.
- [ ] Unit-test `resource-sample` event JSON shape.

### Agent E: Smoke + Docs

Files:

- `crates/dev/sim/tests/local_target_smoke.rs`
- `examples/sim/local-chain-tip-smoke.json` or a new short resource smoke
- `docs/sim/sim-implementation-plan.md`
- this handoff doc

Tasks:

- [ ] Add a smoke assertion for Linux local runs:
  `resourceTimeline.status == "observed"` and `sampleCount >= 2`.
- [ ] Make non-Linux smoke accept `unavailable` with a reason.
- [ ] Use a short enough scenario to keep regular local smoke fast, but long
  enough for two samples at the default interval. If that is too slow, set
  `observability.resourceSampleInterval` to `1s` in the smoke fixture.
- [ ] Update implementation tracker checkboxes as tasks land.
- [ ] Document that heap/allocator profiling is deferred unless the selected
  target binary explicitly exposes a supported profiling mode.

Verify:

- [ ] `cargo test -p groundhog-sim`
- [ ] `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- [ ] Local smoke on a matching `hogd-dev`:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
GROUNDHOG_SIM_GROUNDHOG_BINARY=/path/to/groundhog \
GROUNDHOG_SIM_HOGD_PROTOCOL=hogp/<token-if-needed> \
cargo test -p groundhog-sim --test local_target_smoke -- --ignored --test-threads=1
```

## Suggested Agent Split

If delegating, split work at module boundaries:

1. **Agent A first:** manifest + scheduler. This establishes the resource event
   contract and prevents downstream agents from inventing incompatible event
   timing.
2. **Agent B in parallel:** `/proc` reader and aggregation fixtures. It can use
   local pure fixtures and does not need executor wiring.
3. **Agent D in parallel after result shape is agreed:** result structs and
   timeline builder. It can consume synthetic samples before real sampling
   exists.
4. **Agent C after B/D interfaces settle:** provider sampler hook and restart
   state wiring.
5. **Agent E last:** smoke and docs once the observable artifact shape is stable.

One agent should own final integration because the scheduler, executor, provider,
and result builder all meet at `execute_loaded_simulation`.

## Definition of Done

- Existing scenarios without `observability` produce the same workload behavior
  and do not gain an empty `observability` object in normalized
  `simulation.json`.
- Production execution remains streaming. No new long-run vector proportional to
  workload ticks or resource ticks is introduced.
- `existing` target runs finish with `resourceTimeline.status == "unavailable"`.
- Linux `local` runs with an owned process group finish with
  `resourceTimeline.status == "observed"` and a capped raw sample window.
- Restart chaos produces at least two segments when samples are observed on both
  sides of the restart.
- Resource sample failures never abort a run.
- `cargo fmt --check`, `cargo test -p groundhog-sim`, and focused clippy pass.

## Explicit Deferrals

- `maxRssGrowthBytesPerMinute`, `maxBackpressureRate`, and
  `maxSimulatorOverloadRate` assertions are Slice 3. Slice 2 only preserves the
  data needed for them.
- Compare resource deltas are Slice 3.
- Heap profile generation is optional future attribution, not part of the core
  sampler contract.
- Remote/cluster/exporter integration remains deferred to later provider work.
