# Benchmark Scenario Library

This is the starter library for repeatable local benchmark runs that use the
runtime manifest handoff. The goal is to keep useful runs named and scripted so
we can compare results over time instead of rebuilding one-off shell commands.

All scenarios start a real `hogd-dev` through `groundhog node`, read
`data.runtime_manifest.path` from startup JSON, start a standalone
`hogd-gateway --runtime-manifest`, and run `groundhog-bench` against the
selected target.

```bash
HOGD_DEV_BINARY=/path/to/hogd-dev \
  BENCH_SCENARIO=smoke \
  scripts/smoke-runtime-manifest-bench.sh
```

Set `NODE_STARTUP_TIMEOUT_SECS` higher for slow debug `hogd-dev` builds. Set
`NODE_VERBOSE=1` when a scenario stalls before readiness; the script will print
forwarded hogd-dev logs on startup failure.
Set `HOGD_PROTOCOL_OVERRIDE=hogp/<hash>` when the local `hogd-dev` build is
ahead of the published Rust protocol crates. This only overrides x23 intro
tokens; generated XDR clients still fail if the wire shape has changed.

## Scenarios

### `smoke`

Fast correctness check for CLI, gateway, and bench integration.

```bash
HOGD_DEV_BINARY=/path/to/hogd-dev \
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
HOGD_DEV_BINARY=/path/to/hogd-dev \
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
HOGD_DEV_BINARY=/path/to/hogd-dev \
BENCH_SCENARIO=gateway-baseline \
scripts/smoke-runtime-manifest-bench.sh
```

### `submit-ramp`

Standalone-gateway submit ramp. This is the first pressure scenario for the
manifest-funded dev-wallet path.

```bash
HOGD_DEV_BINARY=/path/to/hogd-dev \
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
HOGD_DEV_BINARY=/path/to/hogd-dev \
BENCH_SCENARIO=submit-ramp \
scripts/smoke-runtime-manifest-bench.sh
```

## Current Local Reference

On a local `hogd-dev` runtime manifest from May 22, 2026, the standalone
gateway submit ramp completed with zero benchmark errors:

| Scenario | Requests | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| 250 rps, 30s | 7,504 | 1,409 us | 2,107 us | 2,347 us | 4,065 us |
| 500 rps, 30s | 15,004 | 1,023 us | 1,228 us | 1,762 us | 4,359 us |
| 1000 rps, 30s | 30,004 | 1,004 us | 1,252 us | 1,668 us | 2,545 us |

These numbers are a sanity baseline, not a performance claim. Treat them as a
local regression reference for the current development machine and hogd-dev
build.

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
