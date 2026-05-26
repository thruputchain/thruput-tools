# Groundhog Simulation Design

**Date:** 2026-05-25
**Status:** Draft design note.

## Purpose

`hoglab` is the CLI for Groundhog's stateful traffic and soak-test facility.
It should answer a different question than the current benchmark harness:

- `groundhog-bench`: how does a specific public RPC path behave under a known
  request schedule?
- `hoglab`: how does a bootstrapped Groundhog system behave over time
  under realistic users, contracts, reads, writes, bursts, and assertions?

The first implementation target is intentionally small: localhost
`hogd-dev`, then a single already-provisioned remote `hogd-dev` host. The
simulator should not wait for the EC2/control-plane story to settle, but its
target model should leave room for later infra providers.

## Wedge

Two things should set `hoglab` apart from a generic L1 soak harness.

First, it exercises the Groundhog storage-token economy directly. Storage
tokens consumed, NNInt compute used, and per-account fairness are first-class
result fields, not afterthoughts bolted onto an RPC bencher. In v1, some of
these fields may be present only as typed availability states until the public
receipt path surfaces the underlying measurements. "NNInt" here means
Groundhog's nonnegative-integer storage item and the contract programming model
built around it. Aptos, Near, and Solana cannot meaningfully test these for
Groundhog because the cost model is not theirs.

Second, every run produces an artifact that diffs cleanly against any prior
run, anchored on bootstrap-manifest availability, runtime-manifest hash, and
runtime producer identity. The comparison knows when it is and is not
apples-to-apples. Soaking and comparing runs is the default workflow, not an
external script.

## Non-Goals

- Do not integrate `hogd/loadgen` in v1. That facility is useful for
  protocol-native direct injection, but it is too specific for the simulator's
  first boundary.
- Do not make `hoglab` an infrastructure platform or a replacement for
  `ops`.
- Do not make simulation manifests another genesis format. Bootstrap intent is
  already owned by the bootstrap manifest, and observed runtime state is already
  owned by the runtime manifest.
- Do not make long-running simulation logic live in shell scripts.
- Do not add private keys to manifests. Sim should derive local dev signers from
  the runtime manifest's derivation metadata, like `groundhog-bench`.

## Boundary

The simulator owns:

- simulation scenario definitions;
- target acquisition and discovery;
- stateful virtual actors;
- public JSON-RPC traffic generation;
- deploy/call/read-after-write workflows;
- long-run progress monitoring;
- pass/fail assertions;
- chaos primitives that fault the target on a deterministic schedule;
- result artifacts and structured comparison across runs.

The simulator consumes:

- a bootstrap manifest when it is responsible for launching a target;
- a runtime manifest when a target is already running or once launch succeeds;
- a public JSON-RPC URL for all chain interaction;
- optional metrics/log endpoints from target providers.

The simulator does not own:

- genesis materialization;
- backend endpoint topology;
- production gateway configuration;
- cloud network, IAM, AMI, SSH, or VPN lifecycle;
- low-level direct transaction injection.

## Relationship To Existing Pieces

### Bootstrap Manifest

The bootstrap manifest is launcher input. Sim may write one, validate one, or
pass one to a launcher. It should not reinterpret it as simulation state.

For v1, sim only needs the existing local-dev shape:

```json
{
  "schemaVersion": 1,
  "kind": "groundhog-bootstrap",
  "networkId": "groundhog-local",
  "profile": "local-dev",
  "devWallet": {
    "profile": "hogd-dev-default"
  },
  "bootstrapAccounts": {
    "count": 100,
    "seed": "groundhog-bench-accounts",
    "initialStorageTokens": "1000000000"
  },
  "storageCostModel": {
    "profile": "hogd-default"
  }
}
```

The simulator can derive a bootstrap manifest from scenario fields such as
`bootstrap.accounts` and `bootstrap.initialStorageTokens`, but the derived file
should still be recorded in the run artifacts as the actual launcher input.

### Runtime Manifest

The runtime manifest is target discovery. Sim should treat it as the
authoritative source for:

- bootstrapped wallet addresses;
- dev-account derivation scheme and seed;
- protocol identity and genesis hash;
- runtime producer identity;
- backend-advertised endpoints when starting a standalone gateway is part of a
  scenario.

For local and remote `hogd-dev` targets, the simulator's normal traffic still
goes through public JSON-RPC. The runtime manifest is out-of-band
orchestration data, not a public RPC surface.

Workload validation must account for optional read-plane endpoints advertised
by the runtime manifest. For example, receipt/status/event workloads require an
indexer endpoint, storage reads require a database gateway endpoint, and view
calls require a view gateway endpoint. If a scenario requests one of those
workloads against a target that does not advertise the corresponding plane, the
run should fail validation before sending traffic.

### groundhog-bench

`groundhog-bench` should remain the low-level open-loop benchmark tool. Sim may
shell out to it for a dedicated benchmark phase, but it should not delegate
stateful simulation to it.

Good split:

- Bench phase: fixed method, fixed rate, fixed duration, focused histograms.
- Sim phase: actor workflows, receipts, events, storage reads, retries, bursts,
  and assertions.

### `groundhog test --integration`

The CLI already ships a short, deterministic, single-actor integration
harness in `crates/cli/src/test_cmd.rs`. It builds the project to WASM,
starts an ephemeral `hogd-dev` on a random port, deploys with `dev-0`, runs
native libtest with `rpc_url`, `contract_address`, and `runtime_manifest`
injected via env, and exits pass/fail.

`hoglab` lives on the opposite axis: long-running, multi-actor,
statistical assertions against thresholds rather than booleans, deterministic
by seed rather than by construction, with a durable result artifact outside
the contract project.

The two tools should share substrate but not implementation. The launcher
path in `node-supervisor`, signers in `signing`, runtime-manifest discovery
in `api-types`, and the typed client in `client` stay common crates so that
neither tool forks them.

Whether `--integration` eventually becomes a scenario preset over the sim
engine is an explicit open question, not a roadmap item. Convergence is
cheap to do later if the shared substrate stays disciplined; it is expensive
to back out of if rushed before sim's actor and result model have
stabilized.

### ops / hogctl

`ops` and `hogctl` contain useful operator vocabulary: run definitions,
deployment identity, discovery by tags, shared observability, and provider
separation. The simulator should borrow those concepts, not the current
implementation.

For v1, sim should support target providers that are simpler than `hogctl`:

- `local`: start and stop `groundhog node` / `hogd-dev` on this machine.
- `existing`: use a supplied RPC URL and runtime manifest.
- `remote-ssh`: start or connect to `hogd-dev` on one dedicated host.

Later providers can discover or create EC2 resources, but the core simulator
should depend on a provider trait, not on OpenTofu state.

## Target Model

A target provider returns a running target:

```text
Target {
  id
  rpc_url
  runtime_manifest
  started_by_sim
  ownership
  metrics_endpoints
  log_handles
  stop()
}
```

Provider responsibilities:

- materialize or locate a target;
- wait for readiness;
- retrieve or receive the runtime manifest;
- expose enough observability handles for sim assertions;
- declare which chaos primitives it supports (none for `existing`, process
  restart for `local`, process restart over SSH for `remote-ssh` later);
- clean up only resources it created.

`ownership` is explicit target metadata, not just a simulator PID. For a
sim-created local target it may include child PID, process-group ID, and
PID-file path. For an `existing` target it records that the simulator owns no
target lifecycle and must not attempt cleanup.

Initial providers:

### `local`

Starts `groundhog node` with a generated bootstrap manifest through the existing
supervisor path. This is the default developer workflow.

Required inputs:

- `hogdDevBinary`
- account count / seed / funding
- optional RPC port
- startup timeout

Output:

- public RPC URL from `groundhog node --json`;
- runtime manifest path and hash from startup JSON;
- local process handle.

### `existing`

Uses a target that is already running.

Required inputs:

- public RPC URL;
- runtime manifest path, URL, or inline runtime manifest payload.

This mode is important for a single dedicated machine or a manually started
EC2 instance. It lets us run simulations before remote orchestration is good.

### `remote-ssh`

Optional early remote provider. It should be deliberately boring:

- SSH to one known host;
- start `hogd-dev` or `groundhog node`;
- copy or fetch the runtime manifest;
- optionally forward metrics/logs;
- stop the process when the run ends, if sim started it.

No EC2 creation is required for this provider.

## Simulation Manifest

The simulation manifest describes traffic and assertions, not chain genesis.
It may include a bootstrap section as input to a target provider, but that
section lowers into the canonical bootstrap manifest.

Example v1 shape:

```json
{
  "schemaVersion": 1,
  "kind": "groundhog-simulation",
  "name": "local-wallet-soak",
  "seed": "local-wallet-soak-v1",
  "target": {
    "provider": "local",
    "hogdDevBinary": "../hogd/build/hogd/hogd_dev/hogd-dev"
  },
  "bootstrap": {
    "accounts": 100,
    "accountSeed": "hoglab-accounts",
    "initialStorageTokens": "1000000000",
    "devWalletProfile": "hogd-dev-default",
    "storageCostProfile": "hogd-default"
  },
  "duration": "30m",
  "loadShape": [
    { "duration": "2m", "users": 10, "ratePerUserHz": 0.5 },
    { "duration": "10m", "users": 50, "ratePerUserHz": 1.0 },
    { "duration": "18m", "users": 100, "ratePerUserHz": 0.5 }
  ],
  "workloads": [
    { "name": "chain-tip", "weight": 10 },
    { "name": "wallet-submit-smoke", "weight": 35 },
    { "name": "receipt-poll", "weight": 25 },
    { "name": "events-query", "weight": 15 },
    { "name": "storage-read", "weight": 15 }
  ],
  "assertions": {
    "maxErrorRate": 0.001,
    "maxReceiptTimeoutRate": 0.001,
    "maxChainStall": "10s",
    "p99SubmitLatencyMs": 250
  },
  "chaos": [
    { "at": "12m", "kind": "restart-target" }
  ],
  "baseline": "baselines/local-wallet-soak.json"
}
```

`chaos` is a deterministic schedule of fault events. Unsupported chaos events
are a validation error by default, because a scenario that silently skips its
fault is usually lying. A later schema may add an explicit `ifSupported` or
`optional` flag for exploratory runs. `baseline` is an optional path (relative
to the manifest) pointing at a committed `result.json` snapshot — see
Comparison below.

After the scheduled load ends, the simulator runs a bounded receipt drain for
submitted transactions that still have no observed receipt. The Phase 1 drain
polls the same per-actor FIFO receipt queues used by `receipt-poll`, retries for
up to 10 seconds with a 250 ms interval, and emits normal `receipt-poll`
completion events at the final simulated timestamp. These polls are
post-schedule reconciliation traffic: they are counted in the compact
`run-completed.scheduledWorkloads` summary for now, but consumers that need to
distinguish scheduled load from shutdown reconciliation should use
`result.jsonl` timing/workload details until a dedicated drain counter is added.
`maxReceiptTimeoutRate` is computed only from submitted transactions still
pending after this drain: `pendingAtShutdown / (present + pendingAtShutdown)`.
Transient `null` receipt polls remain diagnostic `receiptPoll.absent` samples
and do not count as timeouts once a later drain or scheduled poll observes the
receipt.

Open questions before freezing the schema:

- Use JSON for consistency with manifests, or TOML for operator ergonomics?
- Should load phases use actor count, global RPS, or both?
- Should workload weights be global or per actor class?
- How should contract artifacts be referenced once non-dev-wallet workloads
  exist?
- Should the chaos schema admit per-event duration, target selectors, and
  recovery checks from day one, or evolve into them?

## Workload Model

V1 workloads should be public-RPC and manifest-backed:

- `chain-tip`: poll `groundhog_chainTip`.
- `wallet-submit-smoke`: submit dev-wallet-compatible transactions from
  bootstrapped accounts. This workload proves submit/auth/mempool/commit
  plumbing; it must record receipt execution status separately and should not
  assume the Phase 1 transaction body is a successful contract no-op.
- `receipt-poll`: poll receipts for submitted transactions.
- `status-poll`: poll transaction status when that path is meaningful.
- `events-query`: query events around submitted transactions.
- `storage-read`: read known wallet or contract storage keys when available.
- `view-call`: call `groundhog_view` when the runtime advertises a view plane.

For submit workloads, sim owns transaction nonce allocation. A nonce namespace
is the high 32-bit prefix of the unsigned 64-bit transaction nonce; each actor
uses the low 32 bits as its per-account sequence number. When
`--nonce-namespace` is supplied it must fit in `u32`; the actual submitted nonce
is `(namespace << 32) | actor_sequence`. When the namespace is omitted, sim
generates a run-unique prefix and records it in `simulation.json` and
`result.json`. Fixed namespaces are reproducible, but they require a fresh
target or an empty replay cache for the participating accounts.
The emitted `simulation.json` is normalized after CLI overrides are applied;
its hash in `result.json` refers to that emitted artifact, not necessarily the
source scenario file.

V2 workloads should add richer contract scenarios:

- deploy a small contract corpus at run start;
- call compute-heavy, event-heavy, storage-heavy, and calldata-heavy methods;
- add `nnint`-specific workloads that exercise the programming model and
  higher compute limits directly;
- model hot contracts and hot accounts to study fairness and isolation.

### Workload Extensibility

V1 workloads are a closed Rust enum maintained inside the simulator. This
keeps the first cut small, lets the workload model evolve without breaking
external definitions, and avoids prematurely committing to a public
extension API.

Plausible evolution paths, in rough order of cost:

- a trait-based registry of workloads compiled into the crate, with
  third-party crates adding workload modules through a builder;
- a WASM-driven workload hook that sits naturally alongside Groundhog's
  execution model and lets contract authors ship custom workloads with
  their projects.

The closed-enum start is deliberate. It should not be locked in.

### Actors

An actor is the deterministic step source the simulator's scheduler drives.
V1 actors are step-emitting closures: each step returns the next workload
to execute given local state (assigned key, last submitted tx hash, last
observed receipt) the runtime owns on the actor's behalf. The runtime
handles nonce, key access, submit/poll pairing, and retries. Actors are
not responsible for any of that.

The closure language is an implementation detail. The simulation manifest
should name actor/workload profiles, not embed code.

Richer actor models — explicit state machines, happens-before constraints
across actors, shared world models — are anticipated but deferred. The v1
closure shape leaves room to evolve into a state-machine trait without a
manifest break.

## Chaos

Chaos is typed from the first simulator slice even though no provider executes
chaos in Phase 1. The first executable primitive lands in Phase 2. The goal is
not to turn the simulator into a general failure injection framework
immediately; it is to avoid architectural assumptions that only steady-state
runs matter.

Three architectural hooks land early:

- a `ChaosEvent` enum in the simulation manifest, with a single `kind`
  variant in v1 (`restart-target`) and additive room for future variants;
- a `ChaosController` capability on each target provider that declares the
  set of events it can execute. `existing` declares none, `local` declares
  `restart-target`, `remote-ssh` will declare `restart-target` later;
- chaos state transitions recorded in `result.json` and `result.jsonl` with
  observed timestamps, so assertions can reason about them (for example,
  "p99 submit latency excluding 5s after each restart").

Chaos validation happens before the run starts. If the manifest requests an
event the selected provider cannot execute, the run fails before sending
traffic. Faults should be deterministic by simulation time, not by wall-clock
arrival races.

The first executable primitive is target-level stop+restart of the local
managed target at a scheduled time. With today's `groundhog node` supervisor,
`hogd-dev` exit also tears down the JSON-RPC facade, so this primitive models a
whole-target outage and recovery rather than child-process restart behind a
stable gateway. Testing gateway reconnection across backend restarts requires a
topology with a separately managed standalone gateway and backend plane.

Important future chaos families, in likely order:

- Process lifecycle: restart `hogd-dev`, restart the standalone gateway, stop
  and resume a backend plane when the topology has separately addressable
  scheduler/indexer/db/view processes.
- Network conditions: inject latency, jitter, packet loss, or temporary
  disconnects between simulator and gateway, and later between gateway and
  backend planes.
- Backend-plane degradation: make receipt/event/storage/view backends slow or
  unavailable while submit traffic continues, to verify public error mapping
  and recovery.
- Resource pressure: CPU, memory, disk, file-descriptor, or IO pressure on the
  target host, especially during soaks where leaks and backlogs surface.
- Chain progress disruptions: block production pauses, delayed catchup, or
  indexer lag where the target topology can expose those controls.

Each future primitive should have a typed event shape with an explicit target
selector, start time, optional duration, and result records for scheduled,
started, completed, failed, and recovered states. This avoids baking in a
single "restart the whole target" worldview while still keeping v1 small.

## Metrics And Results

Every run should emit one result directory:

```text
hoglab-runs/<timestamp>-<name>/
  simulation.json
  bootstrap-manifest.json        # present only when sim launched the target
  runtime-manifest.json
  result.json
  result.jsonl
  histograms/
  logs/                          # simulator logs for existing targets; target logs only when provider supplies them
  target-ownership.json
```

For early `existing` runs, `histograms/` and `logs/` may be present as
placeholders. They become populated when histogram export and simulator-side
logging land; target logs are only expected when a provider supplies them.

`result.json` should include:

- run id;
- simulation manifest hash;
- bootstrap manifest hash and availability, if a bootstrap manifest is known;
- runtime manifest hash;
- protocol, genesis hash, producer binary/version/commit;
- target provider and target identity;
- per-workload counts, errors, and latency histograms;
- receipt-poll metrics, including receipt presence, transient absence,
  pending-at-shutdown count, and submit-to-receipt latency percentiles under
  the `receipt-poll` workload entry;
- chain-progress timeline;
- storage tokens consumed per workload and in total;
- NNInt compute used per workload where the receipt surfaces it;
- chaos event state transitions and their observed timestamps, including at
  least scheduled, started, completed, failed, and recovered where applicable;
- metric availability for storage-token and NNInt compute fields;
- assertion results;
- baseline-anchored diff when `baseline:` is set on the scenario;
- pointers to logs and metrics snapshots.

`result.jsonl` is the append-only event stream — one versioned record per
workload completion, chaos event, metric availability snapshot, and assertion
check. Workload records include workload-specific correlation fields when
available, such as submit transaction hash and nonce. Live progress readers tail
it; `result.json` is the aggregated snapshot written at run end. The
`target-ownership.json` file records which target resources, if any, the
simulator created. A sweeper may only clean up resources marked
`startedBySim`; it must never infer ownership from a stale PID alone.

Storage-token and NNInt compute metrics are never zero-filled when the public
receipt path cannot surface them. Result fields should distinguish at least:

- `observed`: the metric was measured from chain output;
- `unsupported`: the selected target or RPC surface cannot provide it;
- `unavailable`: the metric should have been available, but collection failed.

The result artifact is the durable product. Console output can be friendly, but
it should never be the only source of truth.

### Comparison

`hoglab compare <run-a> <run-b>` is a pure function on two
`result.json` files. It produces a structured diff covering:

- per-workload count and error deltas;
- latency percentile deltas (p50/p95/p99/p99.9);
- assertion result transitions;
- chain progress and stall deltas;
- storage-token and NNInt compute deltas;
- metric availability transitions, reported separately from numeric deltas;
- manifest/runtime/producer identity match flags. Runtime manifest bytes can be
  hash-compared. Producer comparison uses the runtime manifest's producer
  identity tuple (`binary`, `version`, and optional `commitHash`) unless a
  managed provider records an executable hash separately. When any of these
  differ, the comparison is flagged as cross-config; the diff is still emitted
  but should be read with that context.

Comparisons skip numeric deltas for any metric where either side is
`unsupported` or `unavailable`. The availability transition itself remains in
the structured diff because it usually indicates a topology or instrumentation
change, not a performance regression.

A scenario may set `baseline: <path>` pointing at a committed `result.json`
snapshot. When set, `hoglab run` automatically emits a
baseline-anchored diff inside the final `result.json`. Baseline files are
just committed result snapshots; there is no separate baseline schema.

CI integration patterns — auto-updating baselines on merge to main,
expressing regression thresholds as percentage deltas, HTML diff
rendering — are deliberately out of scope for v1. The simulator exposes
the primitive and the artifact; downstream tooling composes them.

## Phased Plan

### Phase 0: Spec And Skeleton

- Write this design note.
- Add a simulator crate and `hoglab` binary name to the workspace plan.
- Define typed simulation manifest and result structs, including the
  `ChaosEvent` enum, target ownership metadata, metric availability, and the
  optional `baseline` field.
- Define the `ChaosController` capability trait on the target provider
  interface, even though no provider executes a chaos event yet.
- Implement manifest parsing and validation.

### Phase 1: Local Existing Target

- Support `existing` target provider with RPC URL and runtime manifest path
  (declares no chaos capability).
- Derive dev signers from runtime manifest.
- Run `chain-tip`, `wallet-submit-smoke`, and `receipt-poll` workloads when the
  target advertises the required read plane.
- Emit machine-readable results, including `result.jsonl` event stream and
  storage-token / NNInt compute availability fields. Mark those metrics
  `unsupported` until the public receipt path surfaces them.
- Ship `hoglab compare <run-a> <run-b>` as a pure pairwise diff.

This phase can run against a manually started local node or a dedicated remote
machine without owning process lifecycle.

### Phase 2: Local Managed Target

- Support `local` provider.
- Generate a bootstrap manifest from the simulation manifest.
- Start `groundhog node` / `hogd-dev`.
- Capture runtime manifest and logs.
- Stop the target on completion.
- Implement the first chaos primitive: scheduled stop+restart of the whole
  local managed target. Record chaos events in `result.json` and
  `result.jsonl`.
- Honor the optional scenario `baseline:` field and emit the
  baseline-anchored diff inside `result.json`.

### Phase 3: Stateful Contract Workloads

- Add deploy-at-start support for a small contract corpus.
- Add storage, event, and view workloads tied to those contracts.
- Add actor state machines and read-after-write flows.

### Phase 4: Single Remote Host

- Add `remote-ssh` provider.
- Support a manually provisioned EC2 or bare-metal host.
- Fetch runtime manifest and logs.
- Keep cleanup scoped to processes the simulator started.

### Phase 5: Infra Provider Hooks

- Define provider interfaces for later `ops` or EC2-backed target creation.
- Adopt deployment identity fields where useful.
- Avoid requiring OpenTofu state or `hogctl` internals in the simulator core.

## First Implementation Bias

Start with `existing` before `local`. It keeps the first simulator slice
focused on traffic semantics and result shape. Once that is right, process
launching becomes a provider implementation detail instead of the center of the
project.

The first useful command should look like:

```bash
hoglab run examples/sim/chain-tip-smoke.json \
  --rpc-url http://127.0.0.1:9545 \
  --runtime-manifest /tmp/hogd-dev/runtime-manifest.json
```

That small path is enough to prove the module boundary: public RPC traffic,
runtime-manifest discovery, deterministic actors, assertions, and durable
results. `local-wallet-soak.json` is the Phase 1 target scenario once
`wallet-submit-smoke` and `receipt-poll` execution land.
