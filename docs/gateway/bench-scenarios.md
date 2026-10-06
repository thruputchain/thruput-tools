# Gateway benchmarks

Use a benchmark to answer a specific workload question. Keep method semantics,
load controls, binary identities, topology, and logging policy with the results.
A successful local run does not establish safe capacity for a hosted network.
For stateful mixed traffic and fault scenarios, use [hogsim](../sim/README.md).

## Local scenario runner

From the repository root, provide a compatible real devnode and registry access:

```sh
GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  BENCH_SCENARIO=smoke scripts/smoke-runtime-manifest-bench.sh
```

The script starts its own interval-driven local node and standalone gateway.
It creates and retains explicit benchmark fixture keys, supplies their public
initial allocations, and exercises both endpoints through the runtime-manifest handoff. Available
scenario names are `smoke`, `gateway-baseline`, and `submit-ramp`; the latter two
use release builds by default. Use the [script](../../scripts/smoke-runtime-manifest-bench.sh)
for current rates, durations, account counts, and environment overrides.

Save results outside the source tree or in an ignored results directory:

```sh
GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  BENCH_SCENARIO=gateway-baseline \
  BENCH_RESULTS_DIR="$PWD/benchmark-results" \
  scripts/smoke-runtime-manifest-bench.sh
```

Each invocation gets a unique directory. Failed measured work remains visible
in the result and exit status. Setup failures may produce no measured result.
The scenario runner explicitly authorizes its submit traffic with `--force`;
direct non-interactive submit benchmarks must do the same.

## Confirmed receipt and status reads

Start the [node and gateway](runtime-manifest.md#manual-local-loop) with interval
block production. Seed and validate a corpus outside the timed window:

```sh
groundhog-bench --url http://127.0.0.1:9546 \
  --runtime-manifest "$RUNTIME_MANIFEST" --account-seed benchmark-fixtures-v1 \
  --method getTransactionReceipt --seed-count 12000 \
  --seed-output ./confirmed-hashes.json --accounts 10 --force \
  --outstanding 128 --workers 2 --warmup 3 --duration 20 --json > receipts.json

groundhog-bench --url http://127.0.0.1:9546 \
  --method getTransactionStatus --hashes-file ./confirmed-hashes.json \
  --outstanding 128 --workers 2 --warmup 3 --duration 20 --json > status.json
```

Local writes and corpus seeding require `--account-seed` and matching public
initial allocations. A local manifest cannot supply signing credentials. This
check proves allocation identity, not the current available balance; fund the
fixtures for the chosen load. Read-only imported corpora need no fixture keys.
Hosted schema-3 targets retain their separate bootstrap adapter.

The seed submits real transactions. A committed execution failure still supplies
a valid receipt for a read benchmark; it is not evidence of a successful
application call. An imported corpus is read-only and must match the target
network and pass live receipt/status preflight checks. A timeout during seeding
may leave submitted transactions on the target.

`--outstanding` caps global request concurrency across workers. It is different
from an offered request rate and from the HTTP idle-connection pool size. During
measurement, reads must still match the validated corpus; null receipts or
unexpected statuses are failures, not successful throughput.

For pending/unknown lookup admission, use the separate
[status-miss benchmark](lookup-bench.md). Committed reads can return from the
indexer without exercising scheduler lookup capacity.

## Measurement and comparison

Results distinguish warmup, the measured window, and drain. In schema v2,
successful throughput counts requests that both start and finish inside the
measured window. Requests completing after cutoff are recorded separately.
Do not compare these rates with older schemas or mix successful throughput with
all-outcome completion rates.

Match workload controls, generator host and runtime, target topology, build
settings, logging policy, and relevant gateway configuration. Binary hashes and
observed versions identify what ran; runner-supplied labels remain declarations.
Missing metadata is unknown, not verified equality. A caller can supply
`--target-info` when the public RPC cannot describe the deployed environment.
See the [benchmark source](../../crates/bench/src/) for artifact types.

```sh
groundhog-bench compare \
  --baseline baseline-r0.json --baseline baseline-r1.json \
  --candidate candidate-r0.json --candidate candidate-r1.json \
  --vary gateway-revision
```

Use separate comparisons for each method and outstanding count. The comparator
reports repeat spread and retains failed outcomes. Declare the dimension being
varied; do not hide unrelated differences. `--equivalent-corpus` allows matching
count and receipt shape instead of identical hashes, but cannot establish equal
cache warmth or backend data locality. See `groundhog-bench compare --help` for
the supported experiment dimensions.

For transport experiments, restart the gateway with the selected
`--x23-multiplex-connections` value and record `--dump-effective-config`.
Socket count and call admission are separate controls. Scheduler lookup
execution width has its own setting; choose the workload that exercises it.

The older [bench-read-confirmed.py](../../scripts/bench-read-confirmed.py) runner
still passes the retired `--indexer-pool-size` option. Use the direct commands
above for the current gateway until that runner is updated; its old pool sweeps
are not measurements of the current multiplexed transport.

## Server observations and diagnostics

`--metrics-url` samples private gateway metrics. On Linux, `--gateway-pid`
can also sample the local gateway process. Those samples describe that process,
not its children or an entire deployment. Scrape latency, missed peaks, resets,
and unavailable observations limit what can be inferred from them. See
[observability](observability-smoke-test.md) for interpretation.

Keep trace/perf captures separate from baseline measurements. Instrumentation,
formatting, log output, and sampling affect the workload. Preserve exact binary
identity and symbols for useful attribution; a profiled rate is not a baseline
rate. Keep historical results with their result artifacts and experiment context,
not as a table of supposedly current capacity in this guide.
