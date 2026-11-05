# Run and compare benchmarks

Use a benchmark to answer a specific workload question. Keep the workload,
binary versions, host details, topology, and gateway configuration with the
results. Local throughput does not establish a hosted network's capacity.

## Local scenarios

From the repository root, provide a compatible real devnode:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  BENCH_SCENARIO=smoke scripts/smoke-runtime-manifest-bench.sh
```

The script creates disposable fixture accounts and starts its own interval-driven
node and gateway. `smoke` checks both endpoints, including confirmed receipt and
status reads. `gateway-baseline` measures gateway reads and submissions;
`submit-ramp` increases the submission rate. The latter two use release builds
by default. See the [script](../../scripts/smoke-runtime-manifest-bench.sh) for
load settings and environment overrides.

Save results outside the checkout:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  BENCH_SCENARIO=gateway-baseline \
  BENCH_RESULTS_DIR=/path/to/benchmark-results \
  scripts/smoke-runtime-manifest-bench.sh
```

Each run creates a unique result directory. Use `KEEP_WORKDIR=1` to retain
fixtures and logs. The script authorizes its submissions with `--force`.

## Select the workload

Use `thruput-bench --help` for current options. For direct runs:

- Write traffic needs fixture keys and matching public bootstrap allocations.
  Local runtime manifests supply no signing credentials. The scenario script shows
  how to create the fixtures. Use disposable chains for deterministic test keys.
- Confirmed reads need a validated hash corpus. `--seed-count` and `--seed-output`
  create one before timing; this submits transactions and needs confirmation or
  `--force`. `--hashes-file` imports one and checks it against the target.
- `--outstanding` bounds concurrent requests across workers. `--rate` sets an
  offered request rate. HTTP idle-pool size is a separate setting.

A failed execution can still supply a committed receipt for a read benchmark.
A seeding timeout can leave transactions on the target. For status misses, use
[thruput-lookup-bench](lookup-bench.md); committed reads may avoid the scheduler
lookup path entirely.

## Compare results

```sh
thruput-bench compare \
  --baseline baseline-r0.json --baseline baseline-r1.json \
  --candidate candidate-r0.json --candidate candidate-r1.json \
  --vary gateway-revision
```

Match method, load, corpus, generator host, build settings, topology, and logging
policy. Declare the dimension that changed. Use separate comparisons for each
method and concurrency setting. See `thruput-bench compare --help` for supported
dimensions and corpus matching.

Inspect failures and drain counts as well as successful throughput. The result
records its measurement window and inclusion rule; compare compatible result
schemas. A missing version or configuration field means it was not observed.
Use `--target-info` to record deployment details that public RPC cannot report.
Result types and measurement rules live in
[results.rs](../../crates/bench/src/results.rs); comparison checks live in
[compare.rs](../../crates/bench/src/compare.rs).

Use `--metrics-url` for private gateway observations and, on Linux,
`--gateway-pid` for that process's resource use. See
[health and metrics](observability-smoke-test.md) for interpretation. Keep trace
and profiler captures separate from baseline runs: instrumentation changes the
measured workload. Store historical measurements with the result artifacts.
