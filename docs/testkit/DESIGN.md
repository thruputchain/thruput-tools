# Groundhog integration-test harness (`groundhog-testkit`)

**Date:** 2026-07-03
**Status:** Draft design note.

## Purpose

`groundhog-testkit` is a library for writing **multi-agent integration tests as
ordinary application code**. A test connects to a network and runs several
independent agents that each run arbitrary logic against the chain.

It fills a specific gap in the existing tooling:

- `groundhog test --integration` runs a *single* actor: it builds the project,
  starts an ephemeral local node, deploys with the lowest-index `local_user`, and runs
  `tests/integration.rs`. There is no way to model several independent users
  interacting concurrently.
- `hogsim` runs *many* actors, but their behavior is statically configured
  workload profiles named in a manifest, not arbitrary code. Its question
  is statistical ("how does the system behave under this traffic mix over
  time"), answered against thresholds.

In contrast, `groundhog-testkit` is for running a small number of agents, each
running real application logic and asserting on concrete observed results — the
multi-agent generalization of `groundhog test --integration`, not a soak tool.

## Non-Goals

- **Not a load or soak tool.** Traffic mixes, weighted workloads, long-run soak
  diagnostics, and statistical threshold assertions belong to `hogsim`. This
  harness is for correctness assertions by a handful of agents.
- **Not a deterministic-simulation framework (for now).** It drives a real,
  external `hog-devnode` over JSON-RPC, so the network is nondeterministic and
  can only be observed, not forced into a prior ordering. See
  [Determinism and coverage](#determinism-and-coverage) for what we do instead
  and what we leave room for.
- **Not a genesis or manifest format.** Bootstrap intent stays in the bootstrap
  manifest; observed runtime state stays in the runtime manifest.

## Boundary

The harness owns:

- the agent programming model (agent code, agent-to-agent messaging, the agent
  handle onto the network);
- scenario setup and teardown (acquire a target, fund/assign agents, run,
  collect results);
- a virtual clock and the block-production control surface exposed to tests;
- assertion ergonomics (how a test reports pass/fail);
- per-operation measurement (block-latency and block-bandwidth attribution).

The harness consumes:

- a target provider to acquire or locate a network (reused from the existing
  supervisor/provider path);
- dev-account signers derived from the runtime manifest;
- the typed JSON-RPC client, `signing` transaction builders, and the `xdrs`
  codec for building typed calldata;
- a runtime manifest for account derivation and endpoint discovery.

The harness does not own:

- genesis materialization, backend topology, or gateway configuration;
- the workload/soak/comparison model (that is `hogsim`);
- cloud/host lifecycle.

## Concepts

### Network

A *network* is a handle onto a running Groundhog target, acquired through the
same provider path the rest of the tooling uses. Phase 1 supports two providers:

- `existing`: an already-running target given an RPC URL and a runtime manifest.
- `local`: an ephemeral `hog-devnode` launched and owned by the harness via
  `groundhog-node-supervisor`, exactly as `groundhog test --integration` does
  today. Setting the `local` target's `log_level` starts the devnode at that
  verbosity and installs a stderr `tracing` subscriber (idempotent `try_init`,
  honoring `RUST_LOG`) so the supervisor's forwarded devnode logs print, as
  `groundhog node` does.

The network handle exposes public JSON-RPC for all chain interaction, plus two
control surfaces beyond ordinary reads:

- **Query block production**: current tip, and enough block metadata to attribute
  operations to blocks.
- **Control block production**: advance the chain on demand. This is the primary
  lever for reproducibility and for interleaving control (see below).

> **Block control — findings (2026-07-06).** Block production for a local target
> is decided by the Rust supervisor (`groundhog-node-supervisor`), not by the
> C++ `hog-devnode`. The devnode only exposes primitives over its internal x23
> `SchedulerCoord1` service: `schedcoord_get_tip`, `schedulercoord_nominate(tip)`
> (sweeps the whole vetted mempool into a candidate block, capped at
> `kMaxBlockSize = 1_000_000` txs), and `schedulercoord_commit_block(block_id)`.
> The supervisor's `block_driver` owns *when* to nominate+commit. Automatic
> modes are `Instant` (each successful submit wakes a coalescing single-block
> drive) and `Interval` (a timer drives one block per tick, so whatever is
> vetted at the tick batches together — timing-dependent); `Manual` accepts only
> explicit in-process production requests.
>
> Because the policy is Rust we own, **repeatable multi-tx block boundaries need
> no C++ change.** A `Manual` block-driver mode plus a "run one block-production
> cycle now" control on the supervisor lets the harness submit transactions,
> wait until each appears as `Pending`, then trigger one nominate+commit cycle.
> Receipt heights establish which intended transactions committed in that block.
> The trigger controls the nomination boundary; it does not promise exact
> membership or ordering when unrelated submissions race nomination, nor that
> conflicting transactions all commit. Block control stays a dev-only
> supervisor API, off public JSON-RPC, and applies to `local` targets only;
> `existing`/remote targets are observe-only.
>
> **GH-204 implementation shape (2026-07-22).** The supervisor exposes a
> cloneable `ManualBlockProducer` backed by a bounded request/reply queue. Each
> request is serialized and acknowledged after scheduler commit. Request timeout,
> shutdown during a cycle, unconfirmed nomination, and unconfirmed commit RPC
> failure are explicit indeterminate errors; the producer becomes terminal after
> an indeterminate outcome so queued work cannot overlap scheduler work still
> running remotely.
> A failed commit response is reconciled against the scheduler tip before being
> reported indeterminate. The scheduler protocol does not distinguish an empty
> mempool from other nomination rejection, so empty production currently returns
> `SchedulerRejected`.
>
> Testkit exposes `produce_block()` only when its owned local supervisor uses
> manual mode. `SubmittedTransaction` and `SubmittedDeploy` separate submission
> from `await_pending()` and `await_receipt()`, while existing call/deploy helpers
> remain submit-and-wait wrappers. Registry-backed deployment retains the signed
> binary-carrying transaction so an advisory failure or unknown status can safely
> re-submit the same hash rather than create a second registration attempt.
>
> **Optional devnode change:** a configurable per-block cap (plumb `kMaxBlockSize`
> to a CLI flag or bootstrap field, default unchanged) would let tests exercise
> natural multi-block splitting and block-full behavior. It is complementary to
> the manual trigger, not required for repeatability.

> **In-process embedding — findings (2026-07-06).** Embedding `node-supervisor`
> as a library (rather than shelling `groundhog node`) is sound. The CLI already
> drives it this way via `NodeSupervisorHandle::start()` / `stop()` /
> `run_until(stop_signal)`, and the library has no process-global footguns: no
> `process::exit`, no installed signal handlers (its signal code only targets the
> child's process group), no global statics, and no runtime env mutation. The
> child is spawned with `kill_on_drop(true)`, and stop is driven by a
> caller-supplied future. testkit would, however, be the first embedder to run
> concurrent instances, a shared long-lived node, and repeated start/stop in one
> process — patterns the CLI never exercises — so a few sharp edges apply:
>
> - **Bind port 0.** `NodeSupervisorConfig` defaults to a fixed `127.0.0.1:9545`.
>   Concurrent instances must set `with_rpc_listen_address("127.0.0.1:0")` and
>   read the bound `rpc_addr()`. hogd-dev's internal ports are already
>   OS-assigned.
> - **A shared node needs its own runtime.** `start()` spawns the RPC server,
>   block driver, and child stderr forwarder on the *calling* tokio runtime. A
>   node shared across parallel `#[tokio::test]` scenarios must live on a
>   dedicated, long-lived runtime, not a per-test one. This works cleanly because
>   scenarios reach the node over HTTP and trigger block control through a `Send`
>   handle — the client side needs no shared runtime.
> - **Prefer explicit stop; drop is only a backstop.** Graceful `stop()` signals
>   the child's whole process group (SIGTERM, then SIGKILL after a grace period).
>   A plain drop relies on `kill_on_drop` alone (SIGKILL to the direct pid, and
>   only while the runtime is alive to reap it). Wrap each scenario so a panic
>   still runs `stop()`.
> - **Disable the PID file.** The `~/.cache/groundhog` PID-file path is a CLI
>   concern, not the library's. Embed with `LocalNodePidFile::Disabled` (as
>   `groundhog test --integration` does) so instances don't collide on it.
> - **Mind the resource footprint.** `start()` raises `RLIMIT_NOFILE` toward
>   `rpc_max_connections` (default ~2000). The raise is monotonic and
>   process-global but errors under a low hard limit, so pick a modest cap for
>   tests. Each hog-devnode also maps ~15 GiB of virtual address space, which
>   bounds how many scenarios run in parallel.

### Agents

An agent models one independent external entity (a user or app). It is written
as ordinary async Rust — an `async fn` handed an agent context — and may:

- submit deploys and contract calls, and await their receipts;
- await blockchain conditions (a block height, a receipt, an emitted event);
- await a passage of (virtual) time;
- communicate off-chain with other agents through harness-provided channels;
- assert on anything it observes.

Agents draw their identity and funds from **bootstrap dev accounts** derived
from the runtime manifest (the same scheme `hogsim` and `groundhog-bench` use).
Each agent is assigned a bootstrap account (and a nonce namespace; see
[Substrate reuse](#substrate-reuse)) at scenario start.

> **Note — future funding.** The planned devnet would let agents use freshly
> generated keys with funds deposited from a faucet into the associated
> accounts. When that lands, the identity model can widen beyond bootstrap
> accounts.

The harness owns nonce allocation, key access, and submit/receipt pairing on
the agent's behalf; agent code works at the level of "call this method, await
the result," not raw transaction plumbing.

### Contracts: registration and deployment

A contract binary must be *registered* on the network before it can be deployed
by hash: the binary is sent in a transaction's `contracts_to_deploy` field,
where it is visible to a `deploy` syscall in that same transaction and to other
transactions only once the registering transaction commits. The harness retains
one unique binary-carrying transaction per registration attempt and then deploys
further *instances* by hash. If submission outcome is unknown, it may re-submit
the exact retained bytes and hash to recover the attempt idempotently.

A scenario declares contracts by name on the builder; each is read and hashed at
`build()` (fail-fast on a missing file, unrecognized extension, or duplicate
name):

- `.artifact(name, path)` — load from a file, inferring the kind from the
  `.rv32a` extension;
- `.artifact_with_kind(name, path, kind)` — explicit kind;
- `.artifact_bytes(name, bytes, kind)` — embedded bytes (e.g. `include_bytes!`).

Agents then work by name. `ctx.deploy_artifact(name)` is the common path: the
first deploy of a contract carries the binary (registering it and creating that
agent's first instance in one transaction), guarded so concurrent first-deploys
share one unique registration transaction and wait for its receipt to commit;
every later deploy — by any agent — is hash-only and fully concurrent. The returned `Deployed`
reports whether that transaction performed the registration. The steps are also
exposed explicitly for tests that assert on them: `register_artifact(name)`
(force the binary-carrying deploy), `deploy_registered(name)` (hash-only),
`contract_hash(name)`, and `is_registered(name)`. `ctx.deploy(bytes, kind)`
remains a raw escape hatch that always carries the binary.

Calldata and return values are Rust-native: agent code encodes and decodes with
`xdrs` (re-exported as `groundhog_testkit::xdrs`) and passes raw entrypoint
selectors plus calldata to `ctx.call` / `Network::view_call`. The CLI's
`groundhog call` string→calldata codec is CLI-internal (`pub(crate)`) and is not
reused; a Rust test builds typed calldata directly.

> **Open question — Rust proxy generation.** A `codegen ts` path already emits
> typed TypeScript bindings. Whether we want an analogous *Rust* proxy generator
> — typed `struct`s with methods per contract, and named-method selectors
> instead of raw 8-byte entrypoints — or whether the thin `xdrs` path is enough
> for the foreseeable tests, is open. Phase 1 does not require it.

### Virtual clock

Time is a logical clock owned by the harness, not wall-clock. Agents read time
and `sleep` only through the harness handle. With scheduler-controlled block
production, virtual time gives reproducible "wait N ms then act" behavior and is
the foundation for the optional deterministic driver later. Agents must not read
`std::time` directly.

### Assertions and the test harness shape

All that is required now is standard testing: application-specific code makes
assertions about observed results, and the test passes or fails accordingly.
An agent panic propagates immediately. Returned agent errors are collected after
the non-panicking agents finish and make the scenario run return `Err`, retaining
each failed agent's name, index, and original error; callers do not need a
separate report check to avoid false-green tests.

Scenarios are hosted by libtest, so `cargo test` integration, name filtering,
IDE run buttons, and CI result parsing come for free. The only thing a scenario
needs from the harness is an async context plus standard pass/fail reporting,
and a plain `#[tokio::test]` already provides exactly that.

Runtime flavor stays an explicit per-test choice (`#[tokio::test]`
current-thread by default, `flavor = "multi_thread"` when a scenario wants real
parallelism); either works because scenarios reach the node over HTTP.

Setup and teardown are ordinary library code, explicit and configurable:

- **Network acquisition is explicit.** A scenario constructs (or is handed) a
  fixture from an explicit config — provider (`existing`/`local`), listen port,
  seed, block-production mode, and how bootstrap accounts map to agents. A test
  is never surprised by a default it did not state.
- **Teardown is explicit.** The fixture is a value the scenario holds; stopping
  it, or dropping it, tears the node down. Because it is a real owned value, an
  RAII guard means a panicking assertion still triggers cleanup (with the
  supervisor's `kill_on_drop` as the final backstop) — but the ownership is
  visible in the test, not injected behind a macro or harness hook.
- **A shared node is an explicitly-constructed fixture**, not a hidden global.
  Tests that want to amortize node startup build one shared fixture — which owns
  the dedicated, long-lived runtime the node's background tasks require (see the
  embedding findings above) — and hand each scenario a handle to it. Scenarios
  sharing a node stay isolated by taking distinct nonce namespaces and bootstrap
  accounts.

A plain `fn main()` binary form stays available for long-running or soak-style
use.

## Determinism and coverage

Deterministic record/replay of network behavior is not achievable against a
real node, which has its own clock and its own transaction ordering, and cannot
be forced to reproduce a recorded interleaving. We instead treat coverage as a
ladder:

1. **Observed-result assertions (baseline).** Agents run against the live
   (nondeterministic) network and assert on what they observe. This is standard
   integration testing. A single run exercises one interleaving of
   agent/network operations.

2. **Repetition with jitter.** Because one run samples only one interleaving, the
   harness re-runs a scenario many times and, where possible, injects stochastic
   jitter (randomized small delays between agent steps, randomized submit
   ordering, randomized block-production timing under manual control) to expose
   different interleavings. This is a cheap, high-value first step for
   confidence, and pairs naturally with the virtual clock and manual block
   driver.

3. **Deterministic driver over state-machine agents (optional, future).** If we
   later require agent code to be written as async state machines that touch the
   outside world *only* through harness primitives (no raw I/O, no `std::time`,
   no thread spawning), the harness can run them on a controlled single-threaded
   executor and drive their interleaving deterministically from a seed. That
   enables seeded/controlled interleaving exploration (shuttle-style) and
   record/replay of the *agent side* against recorded network observations for
   agent-logic regression. This is left as room to grow; the async agent model
   in Phase 1 is chosen so we can adopt it without a rewrite.

The harness can make *agent-observable ordering* controllable and, with the
manual block driver, make *block boundaries* controllable; it does not make the
network internally deterministic.

## Measurement

Beyond pass/fail, the harness attributes cost to labeled application-level
operations:

- **Block-latency**: for a labeled operation (e.g. "deposit", "settle"), how many
  blocks elapse between submit and commit/receipt, and the virtual time spanned.
- **Block-bandwidth**: the share of a block an operation's transactions consumed
  (transaction count and, where the receipt surfaces it, size/gas).

Manual block production makes this precise: the harness knows exactly which
block each operation landed in. Latency-histogram collection can reuse the same
approach `hogsim` uses for its per-workload latency percentiles rather than
inventing a parallel one.

> **Open question — bandwidth inputs.** Which block-bandwidth inputs the public
> receipt/block path actually surfaces (transaction count vs. byte size vs.
> gas) needs confirmation. Report only measured fields; mark the rest
> unavailable rather than zero-filling, following the `hogsim`
> observed/unsupported/unavailable convention.

## Component reuse

`groundhog-testkit` is a new crate that reuses many existing components:

- `groundhog-node-supervisor` — launch/own a local `groundhog node`, the same
  path `groundhog test --integration` uses.
- `signing` — signer trait, local Ed25519 signer, address derivation, tx
  builders, XDR encoding.
- `groundhog-json-rpc-client` — typed JSON-RPC client.
- `groundhog-json-rpc-types` — runtime manifest schema and shared wire types;
  dev-account derivation metadata.
- `xdrs` — the contract (CXS) and transaction (XDR) codecs, used to build
  calldata and hash contracts. The CLI's `groundhog call` string→calldata codec
  is CLI-internal (`pub(crate)`) and is not reused; a Rust test encodes directly.

Approaches worth borrowing, whose code currently lives inside `groundhog-sim`
rather than in a shared crate:

- **Nonce namespaces** — a run-unique high-32-bit prefix with a per-account low
  32-bit sequence, so concurrent scenarios and reruns don't collide.
- **Receipt-poll pairing** — per-agent FIFO tracking of submitted transactions
  until their receipts appear.
- **Result event stream** — an append-only `result.jsonl`-style record.

Although extracting such mechanisms into a shared crate can be considered, we
prefer simply copying/adapting smaller features to avoid a proliferation of
crates. Phase 1 reimplemented the nonce-namespace scheme and receipt polling
in-crate; the result event stream is not built yet.

## Phased plan

### Phase 0: Skeleton and shape (completed)

- Add the `groundhog-testkit` crate to the workspace (no proc-macro crate).
- Define the core types: the explicit fixture builder, the network handle it
  yields, the RAII teardown guard, the agent context (network access, clock,
  RNG, inter-agent channels), the agent `async fn` shape, and the virtual clock.
- Scenarios are plain `#[tokio::test]` functions. Settle the fixture/teardown
  ergonomics and the assertion/reporting path against a trivial example.

### Phase 1: Live multi-agent scenarios (completed)

- Support `existing` and `local` targets behind an explicit target config:
  - `existing`: RPC URL plus runtime manifest.
  - `local`: embed `groundhog-node-supervisor` as a library (not a `groundhog
    node` subprocess). Bind the RPC facade on port 0, disable the PID file, and
    give a shared node its own long-lived runtime. Teardown is the fixture's
    RAII guard, with the supervisor's `kill_on_drop` as the final backstop.
- Assign bootstrap dev accounts and nonce namespaces to agents; derive signers
  from the runtime manifest.
- Submit/deploy/call built from the `signing` transaction builders with
  Rust-native `xdrs` calldata; await receipts and read chain tip, storage,
  events, and views.
- Load contracts through a named artifact registry (`.artifact` /
  `.artifact_bytes`), registering each binary once and deploying instances by
  hash (`ctx.deploy_artifact`, with explicit `register_artifact` /
  `deploy_registered` / `contract_hash` / `is_registered`).
- Run several agents concurrently in one scenario against the live network and
  assert on observed results. Nondeterministic execution; one interleaving per
  run (coverage ladder rung 1).
- Offline tests cover the harness machinery (mailbox, RNG, clock, account
  assignment, registry validation) in CI without a node. A live contract smoke
  — `scripts/smoke-testkit-local.sh` driving the `#[ignore]`d
  `tests/contract_smoke.rs` — launches a real local node, deploys the
  `caller-counter` fixture, and asserts register-once → deploy-by-hash plus a
  `bump()`/`get()` read-after-write round-trip.

### Phase 2: Clock, block control, and measurement

- Add a `Manual` mode to the supervisor's `BlockDriverMode` plus an acknowledged
  "run one block-production cycle now" request, and expose it on the `local`
  fixture. Split transaction submission from pending/receipt waits so scenarios
  can stage work before the request. This is a supervisor/testkit change (no
  devnode change) and delivers on-demand block boundaries and repeatable
  multi-transaction tests, subject to scheduler conflict and block-size behavior.
- Virtual clock exposed to agents; block-production queries for all targets.
- Per-operation block-latency and block-bandwidth attribution.
- Repetition-with-jitter coverage mode (coverage ladder rung 2).
- Optional devnode change: a configurable per-block cap (`kMaxBlockSize`) to
  exercise natural multi-block splitting and block-full behavior.

### Phase 3 (optional): Deterministic driver

- Require state-machine agents that touch the world only through harness
  primitives; run them on a controlled executor.
- Seeded/controlled interleaving exploration.
- Record network observations and replay the agent side for agent-logic
  regression (coverage ladder rung 3).
