# Benchmark Scenario Library

This is the starter library for repeatable local benchmark runs that use the
runtime manifest handoff. The goal is to keep useful runs named and scripted so
we can compare results over time instead of rebuilding one-off shell commands.

All scenarios start a real `hog-devnode` through `groundhog node`, read
`data.runtime_manifest.path` from startup JSON, start a standalone
`hog-gateway --runtime-manifest`, and run `groundhog-bench` against the
selected target. The node produces blocks on a fixed 500 ms interval so
submissions through the standalone gateway also commit. Smoke runs build the CLI, gateway, and benchmark in debug mode
for a shorter correctness loop; `gateway-baseline` and `submit-ramp` use release
mode so their timings are comparable. Set `BENCH_BUILD_PROFILE=debug|release`
to override the scenario default.

The scenario runner passes `--force` to its `sendRawTransaction` benchmarks.
Direct interactive submit runs prompt before transaction generation; direct
JSON or non-TTY runs must explicitly pass `--force`. Read-only methods do not
prompt.

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  BENCH_SCENARIO=smoke \
  scripts/smoke-runtime-manifest-bench.sh
```

Set `NODE_STARTUP_TIMEOUT_SECS` higher for slow debug `hog-devnode` builds. Set
`NODE_VERBOSE=1` when a scenario stalls before readiness; the script will print
forwarded hog-devnode logs on startup failure.
Set `HOGD_PROTOCOL_OVERRIDE=hogp/<hash>` when the local `hog-devnode` build is
ahead of the published Rust protocol crates. This only overrides x23 intro
tokens; generated XDR clients still fail if the wire shape has changed.
Set `BENCH_REQUEST_TIMEOUT_MS` (default `10000`) and
`BENCH_MAX_RESPONSE_BYTES` (default `1048576`) to bound each complete request
and successful JSON-RPC response body. These limits are recorded in persisted
result configuration so comparisons can reject mismatched runs.

Set `BENCH_RESULTS_DIR` to persist one versioned JSON result per benchmark run.
Each invocation creates a unique scenario directory beneath this path and
writes results atomically. Filenames begin with a zero-padded invocation
sequence so repeated rates remain distinct and ordered:

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  BENCH_SCENARIO=gateway-baseline \
  BENCH_RESULTS_DIR="$PWD/benchmark-results/gateway-baseline" \
  scripts/smoke-runtime-manifest-bench.sh
```

Without `BENCH_RESULTS_DIR`, the benchmark retains its human-readable output.
Results use schema version 2. Both formats distinguish transport, HTTP, invalid
JSON-RPC, JSON-RPC error and semantic failures. The command writes the result
before exiting nonzero if measured or drained requests failed, or if no requests
started in the measured window. `outcome` is `passed`, `failed`, or `empty`. Setup failures
produce no measured result.

### Confirmed status and receipt reads

Prepare a corpus against a running node/gateway, outside the timed window.
For a standalone gateway backed by a local node, start the node with
`--block-driver interval --block-interval-ms 500`; instant mode is triggered
only by submissions through that node's own RPC endpoint:

```bash
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" \
  --method getTransactionReceipt --seed-count 12000 \
  --seed-output ./confirmed-hashes.json --accounts 10 --force \
  --outstanding 128 --workers 2 --warmup 3 --duration 20 --json > receipts.json

groundhog-bench --url http://127.0.0.1:9546 \
  --method getTransactionStatus --hashes-file ./confirmed-hashes.json \
  --outstanding 128 --workers 2 --warmup 3 --duration 20 --json > status.json
```

`--outstanding N` is a global maximum across all workers, independent of HTTP
idle-pool sizing. It conflicts with `--rate` and `--unthrottled`. Without any of
these options the harness retains its 100 rps open-loop default. The existing
`--unthrottled` mode retains its per-worker cap of twice the configured idle
connections. These are maximum concurrency limits, not a guarantee that every
slot stays occupied. `--connections-per-worker` sets the maximum **idle** HTTP
connections, not an active connection limit.

The seed uses the existing dev-wallet self-call fixture with empty inner
calldata. It may commit with a deterministic execution failure; that still
produces a valid receipt for read measurement. Each submission's returned hash
must match its locally computed hash. Seed nonces start at the current Unix
time in nanoseconds unless `--seed-nonce-start` is supplied. Reusing a nonce base
can reuse existing transactions; it does not establish fresh submissions.
Seeding requires the same confirmation as direct submission (`--force` for
noninteractive runs). Importing an existing corpus is read-only.

A hashes file is a JSON object with `schemaVersion: 1`, the target `networkHash`
(as a `0x`-prefixed hash), and a nonempty `hashes` array of unique transaction
hashes. Files are bounded to 16 MiB and 100,000 hashes. The network must match
the live target and any supplied runtime manifest. All hashes must pass live
preflight checks: committed status, non-null matching receipt and equal height.
Seeding can wait for commitment; imports fail if a record is no longer available.
`--setup-timeout-secs` bounds seed/import validation as a whole (default 120).
The completed seed file is published atomically without replacing an existing
file. A timeout may leave already-submitted transactions on the target.

During timed reads, status must remain committed at the preflight height, and
receipts must decode and match both hash and height. Null receipts, pending or
unknown status, and mismatched records are semantic failures, not successful
requests. Each run measures one method; its counts and latency are identified
by `configuration.method`.

### Measurement and result contract

Warmup, measured window and drain are separate. `requests` and
`successfulLatencyUs` include only requests that **start and finish** inside the
measured window. Warmup requests are excluded even if they finish later.
Requests started in the measured window but completed after cutoff are recorded
in `drained`, with `measurement.drainMs` reported separately. Successful
throughput is `requests.successful / durationSeconds`; attempted rate includes
both measured and drained requests. No request is started after cutoff, and each
in-flight request has a complete-response timeout. These semantics differ from
schema v1; do not compare rates across the two versions.

The result records effective load controls, actual generator runtime worker
count, generator host CPU/OS and executable digest. `producer` describes the
benchmark; `runtime` retains backend identity from the manifest; and
`target.gatewayVersion` comes from the target's version RPC. Unavailable
observations carry a reason. Runtime-supplied benchmark Git labels remain
reported provenance; the executable digest identifies the actual running file.

Gateway pool sizes, build profile/features, dirty state, host and worker count
cannot be inferred from the public version RPC. A launcher can supply
`--target-info target-info.json` with this shape:

```json
{
  "schemaVersion": 1,
  "topology": "gateway, backend and generator share one host over loopback",
  "gateway": {
    "buildProfile": "release",
    "features": [],
    "workerThreads": 8,
    "effectiveConfig": {"indexerPoolSize": 16}
  },
  "backend": {}
}
```

Each component may additionally carry `artifactSha256`, `commitHash`,
`gitDirty`, and `host` (`os`, `arch`, `cpuModel`, `logicalCpus`, `osRelease`).
This data is explicitly stored as `target.runnerSupplied`. The launcher must
record the binaries and effective configuration it actually started. Supplied
commits are checked against observed gateway/manifest commits when available;
missing values remain unknown, not equal or verified. Record all effective
settings material to an experiment, not just the pool-size example above.

Comparison contract:

- Match schema and measurement semantics, `configuration` load controls,
  generator runtime worker count/host, topology and relevant target settings.
- Keep `run` labels, URLs, paths, timestamps, raw manifest hashes and source
  identities as provenance, not automatic workload mismatch reasons.
- `corpus.sha256` identifies the ordered network/hash corpus. Its count and
  `shapeSha256` describe the observed distribution of execution status,
  return/abort sizes and per-event topic/data sizes. Matching shape permits a
  declared equivalent re-seeding experiment; it does not prove equivalent
  backend state, locality or cache warmth.
- Declare the varied dimension (gateway revision, pool size, build settings or
  allocator); reject other material differences and surface unknown metadata.
  Never use a broad force flag to hide ordinary experimental differences.

The executable scenario and comparison commands are documented below. This
confirmed-read support makes no performance claim by itself.

## Scenarios

### `smoke`

Fast correctness check for CLI, gateway, and bench integration.

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  scripts/smoke-runtime-manifest-bench.sh
```

Default shape:

- CLI node `chainTip`
- CLI node `sendRawTransaction`
- standalone gateway `chainTip`
- standalone gateway `sendRawTransaction`
- confirmed receipt seeding and status import on both endpoints (8 outstanding)
- `BENCH_RATE=3`
- `BENCH_DURATION=2`
- `BENCH_ACCOUNTS=3`

Use this before and after runtime-manifest or benchmark-account changes.

### `gateway-baseline`

Simple standalone-gateway baseline with one read pass and one submit pass.

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  BENCH_SCENARIO=gateway-baseline \
  scripts/smoke-runtime-manifest-bench.sh
```

Default shape:

- `chainTip`: 250 rps, 30s, 2s warmup, 4 workers
- `sendRawTransaction`: 100 rps, 30s, 2s warmup, 4 workers, 10 accounts

Useful knobs:

```bash
GATEWAY_BASELINE_CHAIN_RATE=500 \
GATEWAY_BASELINE_SUBMIT_RATE=200 \
GATEWAY_BASELINE_DURATION=60 \
GATEWAY_BASELINE_ACCOUNTS=20 \
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
BENCH_SCENARIO=gateway-baseline \
scripts/smoke-runtime-manifest-bench.sh
```

### `submit-ramp`

Standalone-gateway submit ramp. This is the first pressure scenario for the
manifest-funded dev-wallet path.

```bash
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  BENCH_SCENARIO=submit-ramp \
  scripts/smoke-runtime-manifest-bench.sh
```

Default shape:

- `sendRawTransaction`: 250 rps, 30s, 2s warmup, 4 workers, 10 accounts
- `sendRawTransaction`: 500 rps, 30s, 2s warmup, 4 workers, 10 accounts
- `sendRawTransaction`: 1000 rps, 30s, 2s warmup, 4 workers, 10 accounts

Useful knobs:

```bash
SUBMIT_RAMP_RATES="100 250 500 1000 2000" \
SUBMIT_RAMP_DURATION=60 \
SUBMIT_RAMP_ACCOUNTS=25 \
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
BENCH_SCENARIO=submit-ramp \
scripts/smoke-runtime-manifest-bench.sh
```

## Legacy Local Reference

On a local `hog-devnode` runtime manifest from May 22, 2026, the standalone
gateway submit ramp completed with zero benchmark errors:

| Scenario | Requests | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| 250 rps, 30s | 7,504 | 1,409 us | 2,107 us | 2,347 us | 4,065 us |
| 500 rps, 30s | 15,004 | 1,023 us | 1,228 us | 1,762 us | 4,359 us |
| 1000 rps, 30s | 30,004 | 1,004 us | 1,252 us | 1,668 us | 2,545 us |

These numbers predate release-mode scenario builds and complete JSON-RPC body
validation. Keep them as historical context, not as a directly comparable
baseline. Regenerate this table from persisted JSON results before setting
regression thresholds.

## Toward Real Traffic Simulation

The current scenarios are method-specific. A stronger traffic simulator should
add first-class mixed workloads to `groundhog-bench`, probably as a
manifest-backed scenario file rather than more shell flags.

Candidate workload dimensions:

- Weighted method mix: submit, chain tip, receipt polling, transaction status,
  events, and view calls.
- Read-after-write flows: submit a transaction, poll status, fetch receipt,
  then query related events.
- Account fanout: many cold accounts, a few hot accounts, and configurable
  per-account nonce streams.
- Burst profiles: steady state, ramp, pulse, and recovery windows.
- Contract diversity: dev-wallet no-op calls first, then multiple deployed
  contracts with different calldata sizes.
- Background read pressure while submit load is running.

Keep the shell scenarios for smoke and coarse regressions. Move realistic
traffic mixes into the benchmark harness once the method mix needs shared state
between requests.

## Scheduler lookup execution width comparison

`hog-gateway --scheduler-lookup-pool-size N` sets the dedicated scheduler
transaction-lookup execution permits. The range is **1..=68**, with a default
of **16**. Set `scheduler_lookup_pool_size = 16` at the top level of the
gateway TOML config; the CLI overrides TOML. Startup and health reconnect use
the same setting. The value is logged at startup and included in
`--dump-effective-config`. Restart the gateway to change it.

Total admitted lookups remain capped at **68**, including queued and executing
requests, independently of the execution width. Excess lookups fail with
`Overloaded`. This setting does not change scheduler submission/tip connections,
health connections, or the indexer pool.

Indexer and scheduler lookup clients both use `rpc_driver::MultiplexPool`
through `x23-transport`, with many calls in flight per connection over
`x23_multiplex_connections` sockets. They use separate pool instances;
scheduler lookups additionally have gateway admission and execution
semaphores, which is where `scheduler_lookup_pool_size` applies.

Compare 4, 8, and 16 execution permits with the same unknown-hash status
workload and fixed client concurrency. Confirm metrics show
`schedgateway_lookup_tx` calls:
committed hashes can return through the indexer without exercising this pool.
Record successful lookups/s separately from overload responses, execution-permit
wait, backend duration, and errors. Repeat in reverse order. Backend duration
includes transport and gateway scheduling, not just scheduler service time.

Before choosing a deployment value, repeat with release builds, representative
backend network latency, and concurrent submissions. Check submission and tip
latency as well as lookup throughput. Increasing this pool increases scheduler
concurrency; it does not increase the admission bound or guarantee overload-free
operation. Set the lookup pool to 4 to restore the previous execution width.

## Indexer connection pool comparison

The indexer no longer has a pool size of its own. Every x23 data pool is
multiplexed over `x23_multiplex_connections` sockets with no cap on calls in
flight. The runs below predate that change and swept the serial pool's
`--indexer-pool-size`; the method still applies to a sweep of
`--x23-multiplex-connections`.

Build once and restart the gateway with each setting against the same backend:

```bash
cargo build --release -p groundhog-json-rpc-gateway

target/release/hog-gateway \
  --runtime-manifest /path/to/runtime-manifest.json \
  --listen 127.0.0.1:8545 \
  --x23-multiplex-connections 4
```

The setting also works with the existing `--config` and explicit backend-address
options when testing a hosted gateway. It is logged at startup. Restart the
process to change the value; use 4 to restore the original behavior.

For a controlled comparison, use one gateway and one load generator, hold client
concurrency fixed, and test 4, 8, 16, 16, 8, 4 connections. Test
`groundhog_getTransactionStatus` and `groundhog_getTransactionReceipt` separately
with `params: [txHash]`, rotating through at least 10,000 confirmed transactions.
Verify status is committed and receipts are non-null before the load run.
Without a corpus, `groundhog-bench --method getTransactionStatus` uses a zero
hash and measures misses. Supply `--hashes-file` or `--seed-count` as described
above to measure confirmed records. Do not substitute `chainTip`: its
backend requests are coalesced.

Record successful requests/s, errors, p50/p99 and backend metrics; repeat across
separate gateway/backend hosts before selecting a deployment value. More
connections increase backend concurrency and resource use, and can expose other
bottlenecks.

### Local comparison, September 15, 2026

A gateway built from RPC commit `506335dae613057307b5a94585e32efd6dbcbe19`
against `hog-devnode 0.1.28-rc.2` was tested with only the indexer pool constant
changed. The same 12,000 confirmed transaction hashes, one gateway process,
two load-generator workers and 128 maximum outstanding requests were used in
all runs. Each run had 2 seconds warmup and 8 measured seconds; two runs per
setting, in the order above. The final baseline reproduced the initial result.

| Connections | Status reads/s, two runs | Receipt reads/s, two runs |
|---|---|---|
| 4 | 18,212 / 18,150 | 20,516 / 20,429 |
| 8 | 28,976 / 28,836 | 32,620 / 32,651 |
| 16 | 39,871 / 39,724 | 44,535 / 44,882 |

All 2,955,225 measured reads succeeded. Receipt p99 improved from about 7.9 ms
at 4 connections to 3.6 ms at 16; status p99 remained around 9–10 ms. This
initially supported making the pool tunable. The follow-up below informed the new
default of 16.
All processes shared one macOS host over loopback, and the transactions were
recently written. These are short, warm-data measurements, not hosted capacity
or a cold-history test. Gateway-observed backend duration includes transport
queueing and is not a pure backend execution-time measurement.

### Follow-up on macOS and Linux

PR #367 head `43b7081` combined with main `df0c057` was tested in release mode
against hog-devnode 0.1.28-rc.2 on an Apple M4 Max and an AMD EPYC 9554 Linux host.
Each host ran the backend, one gateway and the generator over loopback. The CLI
produced blocks every 500 ms; Tokio worker counts were fixed at eight.

Each sweep used 12,000 verified committed hashes. Pools ran in order
4, 8, 16, 32, 64, 64, 32, 16, 8, 4, with 128 maximum outstanding requests,
three seconds warmup and 20 seconds measurement per method. Mean successful
reads per second across the two trials:

| Pool | Mac status | Mac receipt | Linux status | Linux receipt |
|---|---|---|---|---|
| 4 | 30,651 | 34,392 | 57,007 | 62,175 |
| 8 | 49,264 | 55,645 | 82,828 | 96,102 |
| 16 | 62,943 | 71,231 | 90,617 | 100,105 |
| 32 | 64,609 | 76,504 | 94,062 | 98,428 |
| 64 | 65,615 | 77,699 | 91,224 | 97,500 |

A further single sweep at 512 outstanding requests exercised pools 4, 16, 32 and
64. Server gauges observed 512 in-flight RPCs on Mac and 508 on Linux. Linux
receipt throughput at 16 / 32 / 64 was 95,195 / 96,342 / 96,175 reads/s, confirming
that more client concurrency did not uncover a material advantage for 64.
All 81,650,079 measured requests across the 56 trials returned correct committed
status or matching receipts, with zero public RPC errors.

Sixteen is the default and the selected Devnet configuration target: it removes
most of the four-connection bottleneck with fewer backend connections than 32 or
64. The upper bound of 256 guards against accidental connection storms; it is
not a qualified performance recommendation. These short, warm-data loopback
runs do not establish cross-host, cold-history or mixed-traffic capacity.

Status reads also exercise a separate scheduler lookup pool. Its admission
limit was frequently exceeded during these tests; committed answers still
succeeded through the supported indexer fallback. Larger indexer pools do not
fix that scheduler pressure. [GH-377](https://linear.app/grndhg/issue/GH-377)
tracks pending/unknown workloads, admission behavior and warning volume.

### Server observations

Pass `--metrics-url http://127.0.0.1:9090/metrics` for the gateway's private
metrics endpoint and `--gateway-pid <pid>` when the benchmark runs on the same
Linux host as the gateway. `--sample-interval-ms` defaults to 250. Process
sampling uses that PID alone, excludes children, and is unavailable on macOS.

`serverObservations` retains samples with actual start/completion offsets from
the measured-window start. Sampling begins after warmup and takes a final
sample at the cutoff; scrape latency and scheduling can overlap the boundaries.
It does not include the subsequent request drain intentionally. Backend
request/reconnect counters and CPU time are deltas; RSS and in-flight gauges
are sampled peaks, not guaranteed instantaneous maxima. Scrape failures,
missing series, process failures and counter resets produce unavailable
observations with reasons. A supplied PID verifies process continuity; without
it, a restart whose counters catch up between samples cannot be detected.
Health reconnect counters do not count rpc_driver's internal connection
replacements, which remain explicitly unavailable.

### Confirmed-read acceptance run and pool sweep

Build `groundhog`, `hog-gateway` and `groundhog-bench` in release mode, then run:

```bash
python3 scripts/bench-read-confirmed.py \
  --cli target/release/groundhog --gateway target/release/hog-gateway \
  --bench target/release/groundhog-bench --devnode /path/to/hog-devnode \
  --out /path/to/new-results
```

The `read-confirmed` default owns a fresh interval-driven local node, seeds and
verifies 12,000 hashes, and runs status and receipt reads at 128 and 512 global
outstanding requests. It pins eight Tokio runtime threads and two load workers,
uses three seconds of warmup and twenty measured seconds, and repeats twice.
Every trial imports and validates the corpus before timing, so these are warm
reads. Artifacts and logs survive failures; the runner stops only its own
processes. The output directory must be new.

Use `--scenario pool-sweep` for pools 4, 8, 16, 32 and 64 in forward/reverse
repeat order. `summary.md` contains throughput medians and ranges; individual
JSON files retain latency, errors, identity and server observations. The
`trials.json` index records execution order. `--candidate-gateway /path/to/binary`
adds alternating baseline/candidate runs on the same corpus and produces
comparison JSON and Markdown per pool, concurrency and method.

The runner asks each gateway for `--dump-effective-config` before launch. This
resolves runtime-manifest and CLI overrides without opening listeners. The
runner records the resolved configuration, artifact hashes, observed commits,
host and thread controls. `--build-profile` is a runner declaration, defaulting
to release; callers must supply artifacts built with that profile. Backend
thread counts and unreported build settings remain unspecified.

The runner fixes gateway logging with `--gateway-log-filter` (default `warn`)
and `--gateway-log-sink file|discard` (default `file`). It records the filter,
sink and span-event policy in `target.runnerSupplied.gateway.logging`. File
logging is part of measured cost and can be substantial under fallback load.
Discarding output still formats enabled events; `off` disables them. Comparisons
require matching policies unless `--vary logging` explicitly names the experiment.
Older results have unknown logging policy.

### Comparing saved trials

```bash
groundhog-bench compare \
  --baseline baseline-r0.json --baseline baseline-r1.json \
  --candidate candidate-r0.json --candidate candidate-r1.json \
  --vary gateway-revision
```

Add `--json` for the machine-readable comparison with per-trial server
observations. Throughput and latency summaries show repeat spread; errors
include drain failures. Failed outcomes remain visible and do not establish a
performance improvement. Available experiment dimensions are `gateway-revision`,
`pool-size`, `build-settings`, `allocator` and `logging`; undeclared material differences
are rejected. Use separate comparisons for each method and outstanding count.
Paths, timestamps, labels and endpoint addresses are provenance. Topology,
hosts, benchmark binary, workload controls and resolved non-endpoint gateway
settings are comparison controls. Omitted identities are reported as unknown,
not treated as verified equal. Endpoint normalization assumes the declared
topology is accurate; comparison cannot verify external deployment claims.

The default requires the exact corpus digest. `--equivalent-corpus` instead
requires equal count and receipt-shape digest; it does not establish equal
cache state or backend data layout. No comparison command infers cold-cache
behavior or production capacity from this loopback workload.

### Short diagnostic captures

Use the same runner with `--diagnostic trace` or `--diagnostic perf` and
`--outstanding 128` for a single status workload. Select receipt reads with
`--diagnostic-method getTransactionReceipt`. Diagnostics require one pool and
one outstanding count, accept no candidate, and produce no comparison table.
The node and corpus preparation remain identical to the normal runner.

Capture begins after corpus validation, when the benchmark creates its
`--load-start-file` marker immediately before warmup. The runner observes this
marker asynchronously; capture includes warmup and may overlap measurement.
It is not an exact measured-window profile. The default is five seconds and
16 MiB; `--diagnostic-seconds` and `--diagnostic-max-mib` are capped at 30 and 64.
`diagnostic.json` records bounds, truncation and errors. Raw benchmark results
carry a diagnostic marker, and `compare` rejects them even with `--vary`.

Trace mode enables the gateway's `--trace-spans` option with `RUST_LOG=info`.
It retains bounded `trace.log` output and drains excess output until gateway
shutdown. Enabled spans emit close events with busy and idle durations;
these mean time entered versus time alive but not entered, not CPU time.
The capture contains existing probes, including ordinary log events. It is
not a complete distributed trace: spawned tasks may lack parent context,
and gateway spans do not reveal execution inside hogd. Trace instrumentation,
formatting and pipe backpressure affect this diagnostic workload.

Perf mode requires Linux and `perf` on PATH. It attaches only to the owned
gateway, samples user CPU at 99 Hz with DWARF call stacks, and writes
`perf.data` plus `perf.stderr`. Perf's size limit may overshoot by a record
buffer. Capture errors fail the run rather than masquerading as empty evidence.
Use a build with symbols for useful attribution and keep its artifact identity;
profiled rates are not baseline rates. Neither mode starts a tracing service.
