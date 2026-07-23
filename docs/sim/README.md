# Groundhog Simulation

Simulation docs cover stateful end-to-end chain traffic, soak tests, workload
models, target orchestration, and result artifacts.

Start here:

- [Simulation design](sim-design.md) - draft boundary and phased plan for
  the `hogsim` CLI and Groundhog simulation facility.
- [Implementation plan](sim-implementation-plan.md) - execution checklist with
  Phase 1 details.

Current local-provider quickstart:

```bash
cargo run -p groundhog-cli -- --version

cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-chain-tip-smoke.json \
  --groundhog-binary target/debug/groundhog \
  --hog-devnode-binary /path/to/hog-devnode \
  --hogd-protocol hogp/<hash> \
  --port 0
```

The `local` provider launches `groundhog node --json`, requests the dev account
count required by the scenario, reads the node's startup JSON for the effective
RPC URL and runtime manifest path, runs workloads over public JSON-RPC, and
stops the node at the end of the run. `target-ownership.json` records
`startedBySim: true`, `cleanupAllowed: true`, and best-effort process metadata
for later orphan sweeping.

Local restart chaos can be exercised with:

```bash
cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-restart-chain-tip-smoke.json \
  --groundhog-binary target/debug/groundhog \
  --hog-devnode-binary /path/to/hog-devnode \
  --port 0
```

Scenario catalog smoke coverage is available as the next rung after the focused
local lifecycle smoke:

```bash
cargo run -p groundhog-sim --bin hogsim -- catalog
cargo run -p groundhog-sim --bin hogsim -- catalog --tag contract
scripts/smoke-sim-scenarios.sh
GROUNDHOG_SIM_RUN_SOAKS=1 scripts/smoke-sim-scenarios.sh
```

`examples/sim/catalog.json` is the scenario library index. It records each
scenario's title, provider, tier, tags, runner, and contract artifact
requirements. `hogsim catalog` lists that library and can filter by tag. The
human output includes the runnable scenario path resolved from the catalog's
directory. `hogsim catalog --json` emits the filtered list as an array of
catalog entries with the catalog-relative `path` plus a `resolvedPath` field
for scripts that want to invoke `hogsim run` directly. The
default smoke script runs the fast local ladder and an existing-provider
baseline scenario. It covers read-only chain-tip, write-only wallet submit,
submit-plus-receipt polling, mixed read/write/receipt polling, read-only
restart chaos, a mixed restart artifact exercise that may fail only when it
records restart-loss artifacts, and, when `groundhog-examples` is available,
Fibonacci deploy plus `view-call`, `storage-read`, `events-query`, and
`contract-call` scenarios, including a positive event-indexing smoke that calls
`compute(10)` and observes the emitted `FibComputed` event. It also builds the
`sim-fixtures/caller-counter` fixture and runs a read-after-write smoke that
submits `bump()`, waits for the receipt, reads `get()`, and asserts the
per-caller counter advanced. The opt-in soak runs
`examples/sim/local-mixed-wallet-receipt-soak.json`,
`examples/sim/local-multi-contract-soak.json`, and
`examples/sim/local-realistic-soak-short.json`.

Contract artifact scenarios declare `contractArtifacts` plus `viewCalls`,
`contractCalls`, `storageReads`, or `eventQueries` in the manifest.
Contract entrypoints can use raw eight-byte selector hex for compatibility, but
bundled scenarios use a symbolic identity that the simulator resolves through
the canonical selector implementation:

```json
"entrypoint": {
  "interface": "Fibonacci",
  "version": "0.1.0",
  "method": "compute"
}
```

Symbolic entrypoints avoid embedding derived selector bytes in scenario files.
Calldata remains `0x`-prefixed CXS hex.
`hogsim run --contract-artifact-root <dir>` resolves relative artifact paths from
that directory.
`scripts/smoke-sim-scenarios.sh` detects a sibling `../groundhog-examples`
checkout or `GROUNDHOG_EXAMPLES_ROOT`, builds `fibonacci` plus simulator
fixtures under `sim-fixtures/` into the script workdir, and passes the
resulting `target/groundhog`-style artifact root into the ignored local catalog
smoke.
The default policy is source-first: keep reusable contracts as source projects,
compile WASM on demand, and commit `.wasm` files only when exact byte identity
is the fixture under test.

The catalog also includes heavier local presets that are not part of the fast
smoke ladder: `local-fibonacci-storage-miss-heavy`,
`local-fibonacci-event-heavy`, `local-fibonacci-compute-heavy`,
`local-calldata-heavy`, `local-nnint-heavy`, and the opt-in realistic soak
presets. They reuse the Fibonacci fixture to stress negative storage lookups,
indexed event traffic, recursive contract-call compute, weighted hot-contract
traffic, plus dedicated simulator fixtures for wide u32 calldata, NNInt churn,
and caller-aware read-after-write state. Run them directly with `hogsim run`
and `--contract-artifact-root` after building the required contract artifacts.
Use `local-realistic-soak-short.json` for optional smoke coverage and
`local-realistic-soak.json` for manual baseline collection.

Each `loadShape` phase may set `staggerActors: true` to deterministically spread
actors across the per-user tick interval. The default is `false` for exact
compatibility with older scenario schedules, where all actors at the same rate
tick on the same simulation millisecond. Use staggering for high-actor dapp
traffic so the aggregate rate is modeled as a smoother stream instead of large
synchronized bursts.

Existing-target quickstart:

```bash
cargo run -p groundhog-cli -- --json node \
  --hog-devnode-binary /path/to/hog-devnode \
  --hogd-protocol hogp/<hash> \
  > /tmp/groundhog-node.json

RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/groundhog-node.json)

cargo run -p groundhog-sim --bin hogsim -- validate examples/sim/local-wallet-soak.json --json

cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-wallet-soak.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --json
```

`hogsim validate --json` and `hogsim run --json` emit the same top-level command
envelope as the main CLI: `ok`, `command`, `warnings`, and a versioned `data`
payload. The run payload includes the run id, outcome, aggregate workload counts,
run directory, and paths to `result.json`, `result.jsonl`, and `simulation.json`.

`--hogd-protocol` is optional. Use it when a local `hog-devnode` build advertises a
newer `hogp/<hash>` than the Rust protocol crates linked into `groundhog node`.
It only overrides the intro token; incompatible XDR shapes still fail through
the generated client.

The `local` provider's dev-account path is interim; faucet-based actor funding
is expected to replace it for longer-lived devnet targets.

Phase 1 validates the scenario and runtime manifest, derives bootstrap account
signers from the runtime manifest, writes the result directory skeleton, and
executes deterministic open-loop traffic through the typed RPC client.
Supported workloads are `chain-tip`, `wallet-submit-smoke` (dev-wallet
compatible transactions), `receipt-poll`, `contract-call`, `view-call`,
`storage-read`, `events-query`, and `read-after-write` for manifests that
deploy at least one contract artifact before scheduled load starts.
The executor bounds in-flight work globally and per actor. Per-actor limits are
lane-based: nonce-bearing write flows (`wallet-submit-smoke`, `contract-call`,
and `read-after-write`) serialize on an actor write lane, receipt polling uses a
separate receipt lane, and read-only chain/event/storage/view workloads use a
read lane. Backpressure diagnostics include the lane name, for example
`actor-write`, `actor-receipt`, `actor-read`, or `global`.
`receipt-poll` tracks
actor-submitted transactions in a per-actor FIFO queue until their receipts
appear and records receipt presence, status, optional reverted/error detail,
and submit-to-receipt wall-clock latency.

`view-call` sends `groundhog_view` to the first configured `viewCalls` entry,
using the scheduled actor's wallet address as the simulated caller. Returned
view outputs record `viewResult`, `viewReturnData`, `viewAbortData`, and
`viewEventCount` fields in `result.jsonl`; expected-return mismatches fail the
workload with `errorClass: "view-return-mismatch"`.

`contract-call` submits a dev-wallet `doAction` transaction to the first
configured `contractCalls` entry, using a raw or symbolically derived entrypoint
and raw calldata hex from the manifest. The workload waits for the transaction
receipt before completing. Events record `contractCallName`,
`contractCallContract`, `txHash`, `txNonce`, receipt fields, and
submit-to-receipt latency. Non-`ok` receipts fail the workload with
`errorClass: "contract-call-receipt-status"`.

`read-after-write` uses the first configured `readAfterWrite` entry as a coupled
round-trip. On the first actor tick, or after an invalidation, it reconciles by
calling the configured read view to establish the actor's baseline. It then
submits the configured write through dev-wallet `doAction`, waits for an `ok`
receipt, calls the configured read view again, and requires the observed `u64`
to match the actor's expected value. Both entrypoints accept raw or symbolic
forms. The post-receipt read retries stale values
for up to one second so short receipt/index/view visibility gaps are measured
as workload latency instead of immediate mismatches. JSONL events record
`readAfterWriteName`, `readAfterWriteContract`, `readAfterWriteExpected`,
`readAfterWriteObserved`, `readAfterWriteMatched`, and
`readAfterWriteRebased`. Mismatches fail the workload with
`errorClass: "read-after-write-mismatch"` and force the actor to reconcile on
its next tick.

`storage-read` sends `groundhog_getStorageAt` to the first configured
`storageReads` entry, using the deployed address for that entry's
manifest-local contract name. Storage workload events record
`storageReadName`, `storageContract`, `storageKey`, `storageItemPresent`, and
`storageItemKind` when present. `chainHeight` records the storage snapshot
height. `expectPresent` mismatches fail the workload with
`errorClass: "storage-presence-mismatch"`.

`events-query` sends `groundhog_getEvents` to the first configured
`eventQueries` entry, using a deployed-address filter for that entry's
manifest-local contract name. Event query workload events record
`eventQueryName`, `eventContract`, `eventCount`, `eventCursor`,
`eventLatestBlock`, and `eventOldestBlock`. `expectMinEvents` mismatches fail
the workload with `errorClass: "event-count-below-minimum"`.

After the scheduled load ends, the simulator runs a bounded receipt drain on
remaining pending transactions (10 second budget, 250 ms retry interval). Drain
polls reuse the same `receipt-poll` event shape and are counted in the compact
`run-completed.scheduledWorkloads` summary. Consumers that need to distinguish
scheduled load from shutdown reconciliation should inspect `result.jsonl`
timing/workload details until a dedicated drain counter is added.

`result.json` aggregates per-workload latency, success-only submit latency,
submit-to-receipt latency percentiles, and `receiptPoll.{present, absent,
pendingAtShutdown}` counts. It also includes `diagnostics.errorClasses` when
failures or skipped workloads occur, with count, first/last scheduled time,
affected workloads, affected RPC methods, and one sample error string per error
class. `diagnostics.resourceSampleFailures` summarizes unavailable
`resource-sample` reasons such as `/proc` read failures or file descriptor
pressure. Phase 1 assertions cover:

- `maxErrorRate` over `completed + failed` workloads (skipped excluded).
- `maxChainStall` over all chain-tip observations in simulation-time order; the
  public observation timeline is capped, but `maxStallMs` is computed before
  that cap is applied.
- `p99SubmitLatencyMs` over successful `wallet-submit-smoke` submissions only;
  failed submits contribute to the broader `latencyMs` histogram but never to
  this assertion's percentile input.
- `maxReceiptTimeoutRate` computed as
  `pendingAtShutdown / (present + pendingAtShutdown)`. Transient `null` receipt
  polls remain diagnostic `receiptPoll.absent` samples and do not count as
  timeouts once a later drain or scheduled poll observes the receipt.

Receipt-poll ticks before an actor has submitted a transaction are emitted as
skipped with `errorClass: "no-submitted-transaction"`; error-rate consumers
should treat that as scheduler warm-up rather than RPC failure.
Skipped events with `errorClass: "simulator-backpressure"` are scheduler
capacity diagnostics. Their `error` field identifies whether the limit was the
global in-flight guard or an actor lane such as `actor-write`, `actor-receipt`,
or `actor-read`.

## Existing Provider

The `existing` target provider runs against an already-started JSON-RPC target.
It does not launch, restart, or clean up the target process. The provider
requires:

- an RPC URL, supplied by `target.rpcUrl` or `--rpc-url`;
- a runtime manifest path, supplied by `target.runtimeManifest` or
  `--runtime-manifest`.

CLI overrides win over manifest fields. After resolution, `hogsim` writes the
effective RPC URL, runtime manifest path, and nonce namespace into
`simulation.json` under `resolved`. `target-ownership.json` records
`startedBySim: false` and `cleanupAllowed: false`.

## Local Provider

The `local` target provider launches and owns a local `groundhog node` process.
It rejects `target.rpcUrl`, `target.runtimeManifest`, `--rpc-url`, and
`--runtime-manifest` because those values are outputs from the launched node.
Use `--groundhog-binary` to select the supervisor binary, `--hog-devnode-binary`
to forward a specific backend binary, `--port` to request a port, and
`--startup-timeout-secs` to bound startup. Use `--hogd-protocol` when a local
`hog-devnode` build is ahead of the protocol token compiled into `groundhog node`;
it only overrides the intro token and does not make incompatible XDR shapes
compatible.

Local targets may set `target.blockDriver` to `instant` or `interval`.
`instant` is the supervisor default and wakes block production after successful
submits. `interval` forwards `--block-driver interval` to `groundhog node` and
uses `target.blockIntervalMs` when supplied; the interval default is 500 ms.
`target.blockIntervalMs` is valid only with `target.blockDriver: "interval"`.
Local targets may also set `target.maxRpcConnections` to forward
`--rpc-max-connections` to `groundhog node`; the local-node default is 2000.
All target providers may set `client.maxConcurrentRequests` to cap
simulator-side concurrent JSON-RPC requests. If omitted, hogsim records and
uses the default 512-request cap in normalized `simulation.json`. The simulator
builds a pool of typed HTTP clients and round-robins permitted requests across
them; `client.poolSize` overrides the pool size, otherwise the default is the
peak simulated user count capped by `client.maxConcurrentRequests`. The legacy
`target.maxRpcClientConcurrency` field is still accepted as an alias when it
does not conflict with `client.maxConcurrentRequests`. This prevents high-rate
scenarios from turning initial HTTP connection-pool expansion into spurious
connect failures while still exercising many-client socket behavior; gateway
saturation still shows up as latency, workload failures, 429s, or simulator
backpressure in the result artifacts.

The provider sends SIGTERM to the node for graceful shutdown, waits a bounded
grace period, and escalates to SIGKILL only if the node does not exit.
Simulator-owned local target stdout/stderr is captured under `logs/` as
`local-node-<launch-index>.stdout.log` and
`local-node-<launch-index>.stderr.log` for the initial launch. Restart
relaunch attempts use `local-node-<launch-index>.attempt-<attempt>.stdout.log`
and `.stderr.log`, preserving failed retry-attempt logs instead of truncating
them.
`restart-target` chaos is supported for `local`: the simulator stops and
relaunches the whole `groundhog node` process on the same resolved RPC port,
pauses later scheduled ticks until startup JSON is parsed, records
`chaos-transition` events, and classifies pending receipt queues lost at restart
with `errorClass: "target-restarted"`. The refreshed
`target-ownership.json` process block tracks the latest restarted node.

## Sweeper

`hogsim sweep [root]` scans a runs root, defaulting to `hogsim-runs/`, for
orphaned simulator-owned local targets:

```bash
cargo run -p groundhog-sim --bin hogsim -- sweep hogsim-runs --dry-run
cargo run -p groundhog-sim --bin hogsim -- sweep hogsim-runs --json
```

The sweeper only considers run directories whose `target-ownership.json` says
`startedBySim: true`, `cleanupAllowed: true`, and includes a `process` block.
Before signaling, it validates the live PID's `ps` start-time token, process
group, and command marker against the recorded values, then sends SIGTERM to
the recorded process group and escalates to SIGKILL after the bounded grace
period if needed.

Run `hogsim sweep` only when no simulations are active. It has no heartbeat or
lock-file protection and intentionally ignores the case where the node has
already exited but a separately orphaned backend survives.

## Result Directory

Each run writes a self-contained result directory:

- `simulation.json` - normalized scenario after CLI overrides and resolved run
  metadata.
- `runtime-manifest.json` - copy of the runtime manifest used for validation
  and account derivation.
- `target-ownership.json` - provider ownership and cleanup metadata, including
  best-effort process identity for simulator-owned local targets.
- `result.jsonl` - append-only event stream for run, target, phase, workload,
  contract deployment, chaos transition, assertion, and completion events, plus one
  `metric-availability-recorded` event per run with the run-level availability
  snapshot.
- `result.json` - final aggregate written atomically at run end.
- `histograms/` and `logs/` - reserved directories for future exports and
  provider-supplied logs; local-provider runs write node stdout/stderr logs.

## Compare Semantics

`hogsim compare <run-a> <run-b>` accepts either run directories or direct
`result.json` paths. It is a pure artifact comparison; it does not read the
original target, runtime manifest path, or event stream.

The diff reports whether the simulation manifest, runtime manifest, runtime
identity, and producer identity match. When any of those differ,
`sameConfig` is false and numeric deltas should be read as cross-config
diagnostics rather than a strict regression signal.

For matching or cross-config runs, compare emits `b - a` deltas for workload
counts, workload error classes, latency percentiles, observed chain progress,
chain-stall summaries, assertion status/measurement transitions, and numeric
metrics that are observed on both sides. Storage-token and NNInt compute deltas
are skipped when either run marks the metric `unsupported` or `unavailable`; the
availability transition is still reported so instrumentation changes remain
visible.

The JSON output carries a `schemaVersion` at the root. Bumps follow the same
rules as `result.json`: only on field rename/removal/meaning change. New
optional fields are additive and do not bump the version. The diff also
surfaces `outcomeA`/`outcomeB` so a `Passed → Aborted` transition is visible
without inferring it from derived assertion deltas.

Future work: the simulator is already easy to drive as a scenario matrix by
generating JSON manifests with different load shapes, running each variant, and
comparing the resulting `result.json` artifacts. A first-class matrix runner
could make that workflow durable: accept parameter sets such as actor counts,
rates, durations, workloads, and target binaries; launch each case with stable
run naming; and emit one comparison table over counts, skips, error classes,
latency percentiles, assertion results, and selected workload-specific fields.

## Baselines

A scenario can set `baseline` to a committed `result.json` snapshot or a run
directory containing one. Relative baseline paths resolve from the scenario
file's directory. When present, `hogsim run` embeds `baselineDiff` in the final
`result.json`; the diff uses the same shape as `hogsim compare --json` and is
computed as current run minus baseline.

Baseline comparison is diagnostic only. It does not change the run outcome or
apply regression thresholds. If a configured baseline is missing, invalid, or
uses an unsupported result schema, the run returns an error before writing the
final `result.json`; target cleanup still runs.

RSS-growth thresholds in the soak presets are intentionally calibration-bound:
collect a baseline artifact for each target class before treating
`maxRssGrowthBytesPerMinute` as a mandatory CI gate. End-to-end RSS gating is
Linux-only for now; on macOS the local resource sampler reports
`resourceTimeline` as unavailable and RSS-growth assertions evaluate as
unsupported. For CPU/perf regression tracking outside the run artifact,
instruction-count benchmarks such as `iai-callgrind` and continuous Criterion
tracking through Bencher/CodSpeed-style systems are good follow-ons once the
local soak signal is stable.

Phase 1 runs emit `passed` or `failed` final results. The `aborted` outcome is
reserved in the schema for a future path that writes a final result after
mid-run aborts.

Contract-aware runs add `result.json.contracts`, defaulting to `[]`, with one
record per startup deployment. Records include `artifactKind`, resolved
`artifactPath`, `artifactSha256`, contract hash, transaction hash, deployed
address, address source, commit height, and failure error when applicable.
Deploy failures produce a failed run without entering scheduled workload
dispatch, while still writing terminal result artifacts.
