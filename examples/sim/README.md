# Simulator Scenarios

Scenario files for `hogsim`, the stateful simulation and soak-test harness in
`crates/dev/sim`. `catalog.json` lists every scenario with its intent and
target requirements.

```bash
cargo run -p groundhog-sim --bin hogsim -- validate examples/sim/local-wallet-soak.json
cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-wallet-soak.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest "$RUNTIME_MANIFEST"
```

See [`docs/sim/README.md`](../../docs/sim/README.md) for the scenario
contract, target providers, and the result-artifact layout.
