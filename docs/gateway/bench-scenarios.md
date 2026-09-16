# Benchmark Scenario Library

This is the starter library for repeatable local benchmark runs that use the
runtime manifest handoff. The goal is to keep useful runs named and scripted so
we can compare results over time instead of rebuilding one-off shell commands.

All scenarios start a real `hog-devnode` through `groundhog node`, read
`data.runtime_manifest.path` from startup JSON, start a standalone
`hog-gateway --runtime-manifest`, and run `groundhog-bench` against the
selected target. Smoke runs build the CLI, gateway, and benchmark in debug mode
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
Both formats count a request as successful only after receiving a complete,
matching JSON-RPC 2.0 `result`; transport failures, HTTP failures, malformed or
mismatched responses, and JSON-RPC `error` responses are reported separately
in JSON. Persisted results include the scenario and target label, source commit
and dirty-worktree status, runtime-manifest hash and identity, complete run
configuration, and successful-response latency sample count.
Producer metadata also records whether the benchmark binary was built in debug
or release mode.

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

## Indexer connection pool comparison

`hog-gateway --indexer-pool-size N` sets the number of serial indexer data
connections. The positive integer defaults to **4** and can also be set as
`indexer_pool_size = 8` at the top level of the gateway TOML config. The CLI
value overrides TOML. Startup and reconnect use the same setting. A separate
health connection is excluded from this count. Scheduler pools are unchanged.

Build once and restart the gateway with each setting against the same backend:

```bash
cargo build --release -p groundhog-json-rpc-gateway

target/release/hog-gateway \
  --runtime-manifest /path/to/runtime-manifest.json \
  --listen 127.0.0.1:8545 \
  --indexer-pool-size 8
```

The setting also works with the existing `--config` and explicit backend-address
options when testing a hosted gateway. It is logged at startup. Restart the
process to change the value; use 4 to restore the original behavior.

For a controlled comparison, use one gateway and one load generator, hold client
concurrency fixed, and test 4, 8, 16, 16, 8, 4 connections. Test
`groundhog_getTransactionStatus` and `groundhog_getTransactionReceipt` separately
with `params: [txHash]`, rotating through at least 10,000 confirmed transactions.
Verify status is committed and receipts are non-null before the load run.
The stock `groundhog-bench --method getTransactionStatus` currently uses a zero
hash and measures misses, so it does not reproduce this confirmed-record workload.
Use a generator configured with real hashes. Do not substitute `chainTip`: its
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
supports making the pool tunable, without establishing a new production default.
All processes shared one macOS host over loopback, and the transactions were
recently written. These are short, warm-data measurements, not hosted capacity
or a cold-history test. Gateway-observed backend duration includes transport
queueing and is not a pure backend execution-time measurement.
