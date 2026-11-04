# Measure transaction-status misses

`thruput-lookup-bench` sends `getTransactionStatus` for hashes it never submits.
It measures the indexer miss followed by scheduler lookup, including gateway and
transport costs. It creates no wallets and submits no transactions. Each
successful response must be `Unknown`.

## Run

Build from the repository root:

```sh
cargo build --locked --release -p thruput-bench --bin thruput-lookup-bench
```

Start a [local gateway](runtime-manifest.md), then supply its public RPC URL and
private metrics URL:

```sh
target/release/thruput-lookup-bench \
  --url http://127.0.0.1:9546 \
  --metrics-url http://127.0.0.1:9646/metrics \
  --mode closed-loop --concurrency 4 --duration-secs 10 \
  --output lookup-4.json
```

This mode sends a new request when capacity becomes available. Increase
concurrency one step at a time, with a new output file for each run. To test an
arrival rate, use `--mode paced --rate 500`. For periodic bursts, use
`--mode burst --burst-size 128 --burst-interval-ms 1000`. All modes use the
concurrency cap. See `--help` for options. Agree on load limits before directing
traffic to a shared or remote gateway.

## Read the result

Keep successful `Unknown` responses, gateway overload, other errors, and skipped
client work separate. Exit zero can include overload or skipped load. A pending
or committed response is a workload mismatch and fails the run. Existing output
files are not overwritten.

These short runs have no automatic warmup. Latencies and completion rates
include drain; the request-start rate uses the dispatch window. Do not compare
the all-outcome completion rate with successful throughput from the main
[benchmark tool](bench-scenarios.md).

Metrics cover all gateway traffic. On an isolated target, an increase in backend
lookup calls confirms that requests reached the intended path. Compare admission
and wait metrics with backend duration to locate queueing. Backend duration
includes transport and scheduling; it does not isolate scheduler execution.
Missing samples and counter resets are not zero contention, and sampled gauge
peaks can miss bursts.

Miss traffic does not measure pending hits, submission, or receipt capacity.
See the [runner](../../crates/bench/src/lookup_bench/runner.rs) for result fields,
preflight checks, and exit rules.
