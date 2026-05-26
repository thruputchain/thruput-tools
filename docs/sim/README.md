# Groundhog Simulation

Simulation docs cover stateful end-to-end chain traffic, soak tests, workload
models, target orchestration, and result artifacts.

Start here:

- [Simulation design](sim-design.md) - draft boundary and phased plan for
  the `hoglab` CLI and Groundhog simulation facility.
- [Implementation plan](sim-implementation-plan.md) - execution checklist with
  Phase 1 details.

Current quickstart:

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

The first implementation slice validates the scenario and runtime manifest,
derives bootstrap account signers, writes the result directory skeleton, and
executes deterministic open-loop `chain-tip` traffic through the typed RPC
client. `wallet-submit-smoke` now submits dev-wallet-compatible transactions;
`receipt-poll` tracks actor-submitted transactions until their receipts appear
and records receipt presence, status, optional reverted/error detail, and
submit-to-receipt wall-clock latency. `result.json` aggregates both workload
latency and submit-to-receipt latency percentiles, and Phase 1 assertion
evaluation covers max error rate, max chain stall, p99 submit latency, and
receipt timeout rate. Timeout rate is computed from submitted transactions
that still have no observed receipt when the run ends; transient `null`
receipt polls are retained as `receiptPoll.absent` diagnostics but do not count
as timeouts once the receipt appears. Receipt-poll ticks before an actor has
submitted a transaction are emitted as skipped with `errorClass:
"no-submitted-transaction"`; error-rate consumers should treat that as
scheduler warm-up rather than RPC failure.

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

## Result Directory

Each run writes a self-contained result directory:

- `simulation.json` - normalized scenario after CLI overrides and resolved run
  metadata.
- `runtime-manifest.json` - copy of the runtime manifest used for validation
  and account derivation.
- `target-ownership.json` - provider ownership and cleanup metadata.
- `result.jsonl` - append-only event stream for run, target, phase, workload,
  metric availability, assertion, and completion events.
- `result.json` - final aggregate written atomically at run end.
- `histograms/` and `logs/` - reserved directories for future exports and
  provider-supplied logs.

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
assertion status/measurement transitions, and numeric metrics that are observed
on both sides. Storage-token and NNInt compute deltas are skipped when either
run marks the metric `unsupported` or `unavailable`; the availability transition
is still reported so instrumentation changes remain visible.
