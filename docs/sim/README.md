# Groundhog Simulation

Simulation docs cover stateful end-to-end chain traffic, soak tests, workload
models, target orchestration, and result artifacts.

Start here:

- [Simulation design](sim-design.md) - draft boundary and phased plan for
  the `hoglab` CLI and Groundhog simulation facility.
- [Implementation plan](sim-implementation-plan.md) - execution checklist with
  Phase 1 details.

Current local-provider quickstart:

```bash
cargo run -p groundhog-cli --features dev-node -- --version

cargo run -p groundhog-sim --bin hoglab -- run examples/sim/local-chain-tip-smoke.json \
  --groundhog-binary target/debug/groundhog \
  --hogd-dev-binary /path/to/hogd-dev \
  --port 0
```

The `local` provider launches `groundhog node --json`, requests the dev account
count required by the scenario, reads the node's startup JSON for the effective
RPC URL and runtime manifest path, runs workloads over public JSON-RPC, and
stops the node at the end of the run. `target-ownership.json` records
`startedBySim: true` and `cleanupAllowed: true`.

Local restart chaos can be exercised with:

```bash
cargo run -p groundhog-sim --bin hoglab -- run examples/sim/local-restart-chain-tip-smoke.json \
  --groundhog-binary target/debug/groundhog \
  --hogd-dev-binary /path/to/hogd-dev \
  --port 0
```

Existing-target quickstart:

```bash
cargo run -p groundhog-cli --features dev-node -- --json node \
  --hogd-dev-binary /path/to/hogd-dev \
  --hogd-protocol hogp/<hash> \
  > /tmp/groundhog-node.json

RUNTIME_MANIFEST=$(jq -r '.data.runtime_manifest.path' /tmp/groundhog-node.json)

cargo run -p groundhog-sim --bin hoglab -- validate examples/sim/local-wallet-soak.json

cargo run -p groundhog-sim --bin hoglab -- run examples/sim/local-wallet-soak.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest "$RUNTIME_MANIFEST"
```

`--hogd-protocol` is optional. Use it when a local `hogd-dev` build advertises a
newer `hogp/<hash>` than the Rust protocol crates linked into `groundhog node`.
It only overrides the intro token; incompatible XDR shapes still fail through
the generated client.

The `local` provider's dev-account path is interim; faucet-based actor funding
is expected to replace it for longer-lived devnet targets.

Phase 1 validates the scenario and runtime manifest, derives bootstrap account
signers from the runtime manifest, writes the result directory skeleton, and
executes deterministic open-loop traffic through the typed RPC client.
Supported workloads are `chain-tip`, `wallet-submit-smoke` (dev-wallet
compatible transactions), and `receipt-poll`. `receipt-poll` tracks
actor-submitted transactions in a per-actor FIFO queue until their receipts
appear and records receipt presence, status, optional reverted/error detail,
and submit-to-receipt wall-clock latency.

After the scheduled load ends, the simulator runs a bounded receipt drain on
remaining pending transactions (10 second budget, 250 ms retry interval). Drain
polls reuse the same `receipt-poll` event shape and are counted in the compact
`run-completed.scheduledWorkloads` summary. Consumers that need to distinguish
scheduled load from shutdown reconciliation should inspect `result.jsonl`
timing/workload details until a dedicated drain counter is added.

`result.json` aggregates per-workload latency, success-only submit latency,
submit-to-receipt latency percentiles, and `receiptPoll.{present, absent,
pendingAtShutdown}` counts. Phase 1 assertions cover:

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

## Existing Provider

The `existing` target provider runs against an already-started JSON-RPC target.
It does not launch, restart, or clean up the target process. The provider
requires:

- an RPC URL, supplied by `target.rpcUrl` or `--rpc-url`;
- a runtime manifest path, supplied by `target.runtimeManifest` or
  `--runtime-manifest`.

CLI overrides win over manifest fields. After resolution, `hoglab` writes the
effective RPC URL, runtime manifest path, and nonce namespace into
`simulation.json` under `resolved`. `target-ownership.json` records
`startedBySim: false` and `cleanupAllowed: false`.

## Local Provider

The `local` target provider launches and owns a local `groundhog node` process.
It rejects `target.rpcUrl`, `target.runtimeManifest`, `--rpc-url`, and
`--runtime-manifest` because those values are outputs from the launched node.
Use `--groundhog-binary` to select the supervisor binary, `--hogd-dev-binary`
to forward a specific backend binary, `--port` to request a port, and
`--startup-timeout-secs` to bound startup.

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
with `errorClass: "target-restarted"`. Sweeper ownership fields and baseline
diffing remain later Phase 2 work.

## Result Directory

Each run writes a self-contained result directory:

- `simulation.json` - normalized scenario after CLI overrides and resolved run
  metadata.
- `runtime-manifest.json` - copy of the runtime manifest used for validation
  and account derivation.
- `target-ownership.json` - provider ownership and cleanup metadata.
- `result.jsonl` - append-only event stream for run, target, phase, workload,
  chaos transition, assertion, and completion events, plus one
  `metric-availability-recorded` event per run with the run-level availability
  snapshot.
- `result.json` - final aggregate written atomically at run end.
- `histograms/` and `logs/` - reserved directories for future exports and
  provider-supplied logs; local-provider runs write node stdout/stderr logs.

## Compare Semantics

`hoglab compare <run-a> <run-b>` accepts either run directories or direct
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

## Baselines

A scenario can set `baseline` to a committed `result.json` snapshot or a run
directory containing one. Relative baseline paths resolve from the scenario
file's directory. When present, `hoglab run` embeds `baselineDiff` in the final
`result.json`; the diff uses the same shape as `hoglab compare --json` and is
computed as current run minus baseline.

Baseline comparison is diagnostic only. It does not change the run outcome or
apply regression thresholds. If a configured baseline is missing, invalid, or
uses an unsupported result schema, the run returns an error before writing the
final `result.json`; target cleanup still runs.

Phase 1 runs emit `passed` or `failed` final results. The `aborted` outcome is
reserved in the schema for a future path that writes a final result after
mid-run aborts.
