# Running simulations

`hogsim` drives scheduled, stateful JSON-RPC traffic against a real node. Read
[the design](sim-design.md) for target ownership and timing assumptions. The
[scenario catalog](../../examples/sim/catalog.json) and
[scenario files](../../examples/sim/) are the executable workload references.

Run these commands from the repository root with private registry access and a
compatible `hog-devnode` binary. Build the CLI first so the local provider can
launch it:

```sh
cargo build -p groundhog-cli --locked
cargo run -p groundhog-sim --bin hogsim -- catalog
cargo run -p groundhog-sim --bin hogsim -- validate examples/sim/local-chain-tip-smoke.json
cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-chain-tip-smoke.json \
  --groundhog-binary target/debug/groundhog \
  --hog-devnode-binary /absolute/path/to/hog-devnode --port 0
```

The local provider reads the node's startup JSON, sends public RPC traffic, and
stops the node after the run. `--hogd-protocol` overrides only the x23 intro token
for local development; it cannot repair incompatible wire types and should not
be part of the normal quickstart.

## Choose a scenario

```sh
cargo run -p groundhog-sim --bin hogsim -- catalog --tag contract
GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-sim-local.sh
GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-sim-scenarios.sh
```

The catalog lists target provider, tags, and artifact requirements. Contract
scenarios need artifacts built from the corresponding example source. Set
`GROUNDHOG_EXAMPLES_ROOT` for the smoke script, or supply
`--contract-artifact-root` for a direct run. Keep reusable fixtures as source;
commit binary artifacts only when their exact bytes are the test fixture.

The scenario smoke script includes longer runs only when
`GROUNDHOG_SIM_RUN_SOAKS=1`. Run a short scenario first and examine skipped work
and errors before trusting a long soak. A generator that cannot deliver the
scheduled traffic has not established target capacity.

## Use an existing target

Start a node separately as described in the
[runtime-manifest guide](../gateway/runtime-manifest.md), then run:

```sh
RUNTIME_MANIFEST=$(jq -er '.data.runtime_manifest.path' /tmp/groundhog-node.json)
cargo run -p groundhog-sim --bin hogsim -- run examples/sim/local-wallet-soak.json \
  --rpc-url http://127.0.0.1:9545 --runtime-manifest "$RUNTIME_MANIFEST" --json
```

The existing provider checks public network identity and chain-tip availability
before traffic. It requires the runtime manifest for setup and provenance; it
does not own the target process and will not restart or clean it up. A network
hash alone does not establish reset-specific chain identity or the server's
executed protocol version.

Native-account scenarios explicitly select `accounts.kind: "native"`; see
[existing-native-wallet.json](../../examples/sim/existing-native-wallet.json).
Derived identities are public test keys. Pre-funded mode verifies balances;
`funding: "faucet"` authorizes funding attempts and remains subject to quotas.
Native contract deployment requires explicit `accounts.deploymentCollateralRaw`.
Ambiguous funding or deployment requests retain evidence for reconciliation;
do not blindly rerun them to retry network writes.

Scenarios without `accounts` retain their legacy bootstrap-wallet behavior.
Native accounts currently use the existing provider. Do not silently reinterpret
legacy addresses when changing scenario defaults. Exact account and funding
rules live in [native.rs](../../crates/sim/src/native.rs) and scenario
validation.

## Local faults and cleanup

Local scenarios may schedule `restart-target` chaos. The simulator stops and
relaunches its owned node, records the transition, and classifies transactions
lost across an ephemeral restart separately from normal receipt polling.
The [restart example](../../examples/sim/local-restart-chain-tip-smoke.json)
provides a small starting point.

Runs record ownership in `target-ownership.json`. To inspect or remove orphaned
local targets when no simulations are active:

```sh
cargo run -p groundhog-sim --bin hogsim -- sweep hogsim-runs --dry-run
cargo run -p groundhog-sim --bin hogsim -- sweep hogsim-runs
```

The sweeper checks live process identity before signalling the recorded group.
It has no active-run heartbeat protection; do not run it concurrently with
simulations. It does not clean borrowed existing targets, and a backend orphan
whose recorded node has already exited is outside its cleanup coverage.

## Read the results

Each run directory contains:

| Artifact | Purpose |
| --- | --- |
| `simulation.json` | Normalized scenario and resolved run metadata |
| `runtime-manifest.json` | Runtime input retained with the run |
| `target-ownership.json` | Target lifecycle and cleanup evidence |
| `result.jsonl` | Observations and lifecycle events |
| `result.json` | Final aggregate and assertion outcomes |
| `logs/` | Provider-owned process logs |

Inspect workload failures, skipped ticks, unresolved receipts, and assertion
results together. Empty receipt polls during warmup differ from receipts still
missing at shutdown. Admission-only workloads do not prove successful contract
execution. Missing or unsupported resource measurements are not zero values.
The [result types](../../crates/sim/src/result.rs) define exact fields.

```sh
cargo run -p groundhog-sim --bin hogsim -- compare /path/to/run-a /path/to/run-b --json
```

Comparison reads saved artifacts and reports B minus A. It separates exact file
identity from stable configuration and runtime identity. Cross-config deltas
are diagnostic, not a controlled regression result. Matching seeds or manifest
shapes alone do not establish equivalent backend state.

A scenario's optional `baseline` uses the same comparison machinery. Baseline
diffs are diagnostic and do not automatically change the run outcome; explicit
assertions provide gates. A missing or invalid baseline fails finalization while
target cleanup still runs. Calibrate resource-growth thresholds for the target
class before making them CI gates; unsupported measurements cannot establish a
pass. See [compare.rs](../../crates/sim/src/compare.rs) and
[assertions.rs](../../crates/sim/src/assertions.rs).
