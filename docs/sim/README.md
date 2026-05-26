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
