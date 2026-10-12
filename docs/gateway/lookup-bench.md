# Scheduler lookup miss benchmark

`thruput-lookup-bench` isolates transaction-status lookup admission. It sends
`groundhog_getTransactionStatus` for a deterministic corpus of hashes that it
never submits. It creates no wallets and sends no transactions.

On the indexer-first gateway, a successful `Unknown` answer follows this path:

1. Indexer miss.
2. Scheduler `schedgateway_lookup_transaction_outcomes` miss.

This tests the whole status-miss path, including the indexer read. It is not a
pure scheduler microbenchmark. Random hash absence is not an authoritative
backend assertion; every successful response must be `Unknown`. A pending or
committed response is a semantic mismatch, not successful miss traffic.

## Run

Build the dedicated binary from the existing benchmark package:

```sh
cargo build --release -p thruput-bench --bin thruput-lookup-bench
```

Use an already running local gateway backed by a real devnode. No transaction
seeding is needed. Supply its public RPC and native metrics endpoints:

```sh
./target/release/thruput-lookup-bench \
  --url http://127.0.0.1:8545 \
  --metrics-url http://127.0.0.1:9646/metrics \
  --mode closed-loop --concurrency 4 --duration-secs 10 \
  --output lookup-4.json
```

Repeat with concurrency 16, 32, 64, 128, and 256, using distinct output files.
Increase one dimension at a time. This mode dispatches another request whenever
capacity becomes available; it deliberately finds saturation rather than
modeling an independent arrival rate.

Paced traffic offers requests at a fixed rate, subject to the concurrency cap:

```sh
./target/release/thruput-lookup-bench \
  --url http://127.0.0.1:8545 \
  --metrics-url http://127.0.0.1:9646/metrics \
  --mode paced --rate 500 --concurrency 64 --duration-secs 10 \
  --output lookup-paced-500.json
```

A missed scheduled request must remain visible as skipped load. Do not interpret
a run that cannot deliver its offered rate as proof of server capacity.

Periodic bursts concentrate arrivals while leaving a gap between bursts:

```sh
./target/release/thruput-lookup-bench \
  --url http://127.0.0.1:8545 \
  --metrics-url http://127.0.0.1:9646/metrics \
  --mode burst --burst-size 128 --burst-interval-ms 1000 \
  --concurrency 128 --duration-secs 10 --output lookup-burst-128.json
```

An explicit remote URL directs load to that server. Start remotely only with a
bounded, agreed traffic envelope; local throughput is not a safe remote rate.
The tool does not deploy or change gateway capacity.

## Interpret

Keep these outcomes separate:

- `Unknown`: the full miss path completed successfully.
- Gateway overload: rejected work, not successful lookup throughput.
- Other RPC, HTTP, transport, timeout, or semantic errors: inspect their counts.
- Client-side skipped work: offered traffic the generator did not dispatch.

With access to `/metrics`, compare lookup admission decisions, local wait
histograms, executing/queued levels, and backend lookup duration. An increasing
lookup-call count verifies that traffic exercised the intended path. Full
admission decisions happen before a scheduler call; they should not increase the
successful backend-call count.

Scrapes observe all traffic on the gateway. Counter deltas are not attributable
to this tool on a shared deployment; separate setup, load, and drain timestamps
also matter. Sampled gauge peaks can miss short bursts. Missing or reset metrics
must not be interpreted as zero contention.

Fast backend calls plus long execution-slot waits suggest local admission
pressure. Long backend calls still combine scheduling, x23 connection/pool wait,
transport, and scheduler work. This tool does not isolate those components.

A mempool miss and a mempool hit can cost different amounts. To test pending
hits, use a separately controlled transaction corpus and delayed block production;
this tool intentionally does not submit or manage that workload. Do not infer
submission, confirmation, or receipt-visibility capacity from a miss-only test.

## Result and exit status

The JSON artifact records configuration, wall-clock start/end times, dispatch
and total elapsed time, started/completed/skipped counts, completions during
drain, outcome counts, and separate Unknown/overload latency summaries. These
short diagnostic runs have no automatic warmup. Completion rates and latency
summaries include the bounded drain; request-start rate uses the dispatch window.
Do not use the all-outcome completion rate as successful lookup throughput.

The output path is reserved before sending requests and existing files are not
overwritten. Three corpus entries (or the full corpus when smaller) are checked
before load; failure to return Unknown stops the run and preserves preflight
results. Preflight calls are outside the measured counts and metric snapshots.

Overload is an expected diagnostic outcome and does not alone cause a nonzero
exit. Other measured errors do cause a nonzero exit after writing the result.
Always inspect outcomes and skipped counts; exit zero is not an assertion of
zero overload or of achieving the requested arrival rate. Request timeout is
controlled with `--timeout-ms`, and each response has a bounded body size.

Optional metrics include raw before/after scrapes, lookup admission/wait and
backend lookup counter deltas, and sampled lookup gauge peaks. Missing scrapes
are null; counter deltas without a baseline or with a decrease are null. A
restart that catches up between snapshots cannot be detected reliably from
counter values alone. The sampler is best effort and runs through drain.
