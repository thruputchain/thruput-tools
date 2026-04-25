# Dev-Node Layering: Combining What We Already Have

**Date:** 2026-04-24
**Status:** Working position, not a decision. Seeking feedback.

**Companion to:**
- [`localdev-rpc-node.md`](localdev-rpc-node.md) — original Level B prototype design
- [`specs/2026-04-24-embedded-dev-node-backend.md`](specs/2026-04-24-embedded-dev-node-backend.md) — durable hogd-side backend direction

## Why this doc

`localdev-rpc-node.md` defined Levels A/B/C and shipped Level B against
`native_exec` / `test_ffi`. The 04-24 embedded-backend spec named the durable
target as a hogd-side `libhogdev` library extracted from the
`native_contract_test_framework` seed.

Both are right but neither names the **Rust-side aperture** that ties the
dev-node, the chuckd-rs coordinator, the gateway, and any future embedded
backend together. The maintenance / DRY story is mostly determined by
that choice.

This doc captures the current take after looking at:

- `chuckd-rs/crates/scheduler/src/client.rs` — wide trait covering the
  whole coord→mempool surface, with a working TCP impl validated by
  `chuckd-rs/crates/sched-smoke`.
- `chuckd-rs/crates/testkit/src/mock_scheduler.rs` — in-process mock
  implementation, used by `chuckd bench coordinator`.
- `hogd/hogd/mempool/test/infra.h` — Jack's `MempoolTestInfra`: real
  mempool + scheduler + executor + auth + hogdb-RAMDB stitched in-process
  with a fake coordinator, no consensus or gossip.
- `groundhog-rpc/crates/gateway/src/{submitter,tip_coalescer}.rs` —
  narrow per-feature traits (`SubmitSink`, `TipSink`) wrapping the raw
  XDR client directly.

Short version: **`chuckd_scheduler::SchedulerClient` is the right
abstraction boundary for the dev-node. The path forward is a TCP backend
against `hogd --no-chuckd` plus a sibling trait for dev-only mutations.
A C-FFI / `libhogdev` embedded backend is contingent — possible behind
the same trait, but probably not worth the cost for the dev-node use
case.**

## Two requirements pulling in the same direction

1. **Anvil-like DX unlock.** Single fast binary, instant startup, mining
   controls, snapshot/revert, deterministic pre-funded accounts, dev-only
   RPC namespace, real execution semantics underneath.
2. **Solid maintenance / DRY story.** Reuse hogd's executor, reuse
   chuckd-rs's coordinator-side machinery, reuse the gateway's RPC
   plumbing where useful. Don't fork. Don't mock what's already real.

These pull in opposite directions only if we pick the wrong abstraction
boundary. Pick the right one and they reinforce: real fidelity falls out
of going through the production trait, and DRY falls out of having one
trait everywhere.

## The aperture: `chuckd_scheduler::SchedulerClient`

`chuckd-rs/crates/scheduler/src/client.rs:64+` defines a wide async trait
covering the entire coord→mempool RPC surface from
`protocol/sched_coord.x`:

- `get_tip`, `nominate`, `validate`, `commit`
- `submit_unvetted`, `submit_vetted`, `lookup_transactions`

That trait *is* the "play coordinator" surface. Anything that wants to
drive blocks through hogd's mempool+executor goes through it.

Already-validated implementations live alongside it:

- **`TcpSchedulerClient`** — connects over TCP, performs the `*_intro_*`
  handshake, drives a real hogd via the production wire. Validated
  end-to-end by `chuckd-rs/crates/sched-smoke/src/main.rs`, which is
  effectively a working "play coordinator" reference impl: connect →
  intro → load_genesis → get_tip → `(nominate → commit)` loop.
- **Mock scheduler** in `chuckd-rs/crates/testkit/src/mock_scheduler.rs` —
  in-process implementation with no exec, used by
  `chuckd bench coordinator --num-validators 1`.

The hogd side has the complementary existence proof:
`hogd/hogd/mempool/test/infra.h` runs **real** mempool + scheduler +
executor + auth + hogdb-RAMDB in one process, with the test playing
coordinator via `coordinator_sched_server_` (`infra.h:359`). That's the
seed for an in-process `SchedulerClient` backend that doesn't require a
hogd subprocess. **No consensus, no gossip, no catchup** — those are
chuckd's job, not in scope. That's almost certainly why Jack built it
this way.

## Proposed layering

```
groundhog node CLI
  ↓
groundhog-rpc dev-node
  · JSON-RPC surface
  · groundhog_dev_* RPC namespace
  · mining policy (auto / manual / interval)
  · pre-funded accounts
  · backend selection
  ↓
chuckd_scheduler::SchedulerClient   ← the aperture
  ↓
┌───────────────────────────┬──────────────────────────────┬──────────────────┐
│ TcpSchedulerClient        │ EmbeddedSchedulerClient      │ MockScheduler    │
│ → hogd --no-chuckd subproc│ → libhogdev FFI in-process   │ → testkit mock   │
│ The path forward          │ Contingent — see below       │ CLI integration  │
│                           │                              │ tests            │
└───────────────────────────┴──────────────────────────────┴──────────────────┘
```

The dev-node holds a `Box<dyn SchedulerClient>`. The `--backend` flag
selects which implementation. **Mining policy lives above the trait, in
the dev-node** — not in any backend. That keeps backends interchangeable.

Dev-only mutations (snapshot, revert, set_storage_tokens, write_item,
register_contract) need a sibling trait — call it `DevControl` for now —
because they're not part of the production coord→mempool surface. In
the TCP-backend world, `DevControl` is implemented as a small dev-only
XDR-RPC program added to hogd (only enabled with `--no-chuckd` or a
similar dev flag). The trait is the thing the dev-node depends on; the
hogd-side RPC is the thing that fulfills it.

```rust
struct DevBackend {
    sched: Box<dyn SchedulerClient>,  // production-shaped trait
    dev:   Box<dyn DevControl>,       // dev-only mutations
    accounts: Accounts,
    mining: MiningPolicy,
}
```

## DRY map: what lives where

| Capability                          | Owner                                           | Status                                           |
|-------------------------------------|-------------------------------------------------|--------------------------------------------------|
| Coord→mempool RPC trait             | `chuckd_scheduler::SchedulerClient`             | Exists                                           |
| TCP impl against real hogd          | `chuckd_scheduler::TcpSchedulerClient`          | Exists, validated by `sched-smoke`               |
| In-process mock                     | `chuckd-rs testkit::mock_scheduler`             | Exists                                           |
| In-process embedded impl            | `EmbeddedSchedulerClient` over libhogdev FFI    | Contingent — may never be built (see below)      |
| In-process mempool+exec+auth+hogdb  | `MempoolTestInfra` in hogd C++ tests            | Exists; library extraction only if FFI is built  |
| Mining policy / block driver        | dev-node                                        | New, on top of trait                             |
| Dev RPC namespace                   | dev-node                                        | Partially exists                                 |
| Snapshot/revert                     | hogd-side dev RPC + `DevControl` trait          | New, requires hogdb support                      |
| Pre-funded accounts                 | dev-node `accounts.rs`                          | Exists                                           |
| JSON-RPC surface                    | dev-node `rpc.rs`                               | Exists                                           |
| `submit_unvetted` plumbing          | gateway `SubmitSink`                            | Exists; can be adapter over `SchedulerClient`    |
| `get_tip` plumbing                  | gateway `TipSink`                               | Exists; can be adapter over `SchedulerClient`    |

The gateway's narrow traits (`gateway/src/submitter.rs:32-67`,
`gateway/src/tip_coalescer.rs:29-67`) currently wrap the raw codegen'd
XDR client directly. Once `SchedulerClient` is a shared dependency, both
become ~5-line adapters over `dyn SchedulerClient`, removing the
duplicated `Arc<RwLock<Option<Client>>>` connection-management dance
that lives in both gateway sinks today.

## Anvil DX features, mapped onto the layering

| DX feature                       | Lives in           | Notes                                              |
|----------------------------------|--------------------|----------------------------------------------------|
| `groundhog node` instant startup | dev-node + impl    | tcp = ~hogd boot (~100s of ms, once per session)   |
| `sendRawTransaction` real exec   | SchedulerClient    | `submit_unvetted` + auto-mine                      |
| Auto-mine on submit              | dev-node           | `nominate` + `commit_block` after submit           |
| `groundhog_dev_advanceBlock`     | dev-node           | Manual `nominate` + `commit_block`                 |
| `groundhog_dev_setStorageTokens` | DevControl         | Backend-specific implementation                    |
| `groundhog_dev_snapshot/revert`  | DevControl         | Requires hogd-side support; same gap as 04-24 spec |
| Pre-funded accounts              | dev-node           | Same `derive_keypair` pattern as `tx-builder`      |
| `chainTip`                       | SchedulerClient    | `get_tip`                                          |
| Real auth/nonce/replay path      | SchedulerClient    | Falls out of using real mempool — the DRY win      |

The last row is load-bearing: "real auth/nonce/replay path" is **not**
something the dev-node implements — it falls out of going through
`SchedulerClient::submit_unvetted`. Today's Level B (`native_exec`
direct) skips this entirely, which is a chunk of the test-harness
lifecycle problem the 04-24 spec names.

## Why subprocess is probably the answer (and FFI is contingent)

The 04-24 embedded-backend spec presents `libhogdev` as the durable
target. After working through the layering, that framing looks
overstated for the dev-node use case. **A TCP backend against
`hogd --no-chuckd` is probably the durable answer, not just an interim
shipping point.**

### Why subprocess holds up

- **Performance is irrelevant.** Loopback TCP is sub-ms; `groundhog node`
  starts once per dev session. Even hogd's startup time (~100s of ms)
  is invisible at session granularity.
- **Anvil itself is a separate binary.** Of the references the 04-24
  spec cites, the closest analog to "C++ daemon, want Rust DX" is
  Anvil — which chose subprocess. EDR and Soroban are Rust libraries
  because their *runtime* is Rust-native; the analogy doesn't transfer
  cleanly to hogd.
- **Already proven on-the-wire.** `sched-smoke` already connects to
  `hogd --no-chuckd` and drives blocks. The wire works today.
- **`DevControl` doesn't motivate FFI.** Snapshot/revert/setStorageTokens
  can be implemented as a dev-only XDR-RPC program added to hogd, gated
  on `--no-chuckd` or similar. Same hogd work either way; subprocess
  delivers it without a C ABI.
- **Crash isolation.** Subprocess: hogd panics, dev-node reports it
  cleanly. Embedded: hogd panic = dev-node crash.
- **No C ABI to design and version forever.** This is the big one. C
  ABIs are *expensive* — versioning, ownership rules, threading
  contract, every change is a breaking change for someone. Avoiding
  that surface is a major DRY win.

### When FFI would actually win

The honest scenarios that would change this conclusion:

1. **Per-test node startup at high frequency.** A `cargo test` suite
   that spins up a fresh dev-node per test (hundreds of times).
   Subprocess fork+exec+init becomes meaningful overhead. (Mitigation
   today: testkit's mock scheduler covers most of these without exec
   fidelity.)
2. **Embedding hogd in other Rust binaries.** Some hypothetical future
   tool that doesn't want a subprocess hanging around.
3. **Single-binary distribution as a hard requirement.** If "ship one
   file, no PATH dependencies" is non-negotiable. Anvil shows it
   usually isn't.
4. **Zero-serialization snapshot/revert.** If snapshot/revert needs
   to be sub-ms, embedded wins. For dev workflows it almost certainly
   doesn't.

None of these is a current requirement. (1) is the most likely to
materialize, but only if SDK tests start spinning up dev-nodes
per-test; for the CLI loop it doesn't.

### The deferred-FFI bet

The `SchedulerClient` aperture lets us defer the FFI question
indefinitely without painting into a corner. If a concrete trigger
ever shows up, libhogdev slots in behind the trait without rewriting
consumer code. Until then, we don't pay the C-ABI tax.

## Path forward

### Stage 1 — Trait migration + TCP backend (the path)

- Decide where `chuckd_scheduler` lives long-term (open question below).
  Short-term: `groundhog-rpc` takes a dependency on it from chuckd-rs.
- Refactor `crates/dev-node/src/backend.rs` from
  `TestEnvironmentHandle` → `Box<dyn SchedulerClient>`.
- Implement `--backend tcp` (default and likely only production
  backend): `groundhog node` spawns `hogd --no-chuckd` as a child
  process (same flag chuckd-rs's `sched-smoke` uses), points
  `TcpSchedulerClient` at it, serves JSON-RPC on top.
- Refactor gateway `SchedulerSink` and `SchedulerTipSink` into adapters
  over `dyn SchedulerClient`.

This is shippable without any new hogd work. "Single binary DX" becomes
"single binary that spawns one subprocess on demand" — same model as
Anvil/Foundry, and `sched-smoke` already proves the wire works.

### Stage 2 — `DevControl` as a hogd dev-only RPC program

- Define the dev-only mutation trait (snapshot, revert,
  set_storage_tokens, write_item, register_contract).
- Add a new XDR program (e.g. `dev_rpc.x`) implemented in hogd, gated
  on `--no-chuckd` or a `--dev` flag so it can never serve production
  traffic.
- Add hogdb support for what's needed. Snapshot/revert is the heaviest;
  `set_storage_tokens` already works via direct DAO writes today.
- TCP `DevControl` impl talks to that program; the dev-node consumes
  the trait.

### Stage 3 (optional) — Mock backend for CLI tests

- `--backend mock` using testkit's mock scheduler for CLI integration
  tests where we're testing the *CLI*, not the chain. Drop in if/when
  the test surface needs it.

### Stage 4 (contingent, may never happen) — `libhogdev` embedded backend

Only worth doing if one of the FFI-win scenarios above becomes a real
requirement. The trait aperture means it can be added later as a
drop-in `--backend embedded` without breaking Stage 1/2 consumers.

If it does happen: extract `libhogdev` from `MempoolTestInfra`'s
wiring (`infra.h`), define the C ABI to back `SchedulerClient` +
`DevControl`, build `EmbeddedSchedulerClient` as the Rust adapter.
The `~MempoolTestInfra` destructor (`infra.h:80-128`) is the worked
example of the shutdown contract that would need to be productionized
across the FFI.

## Why this combines well

- **One trait, every consumer.** Dev-node, chuckd-rs coordinator, and
  (eventually) gateway all consume `SchedulerClient`. No parallel
  reimplementations of "talk to hogd's coord surface."
- **Anvil DX shippable from existing pieces.** Stage 1 lands a working
  real-fidelity dev-node using `TcpSchedulerClient` + `hogd --no-chuckd`,
  zero new hogd work required. Stage 2 adds dev-only RPC for
  snapshot/revert/etc.
- **Honest fidelity.** Going through `submit_unvetted` exercises the real
  auth/nonce/replay path — bugs that match production show up in dev.
- **Aligned with chuckd-rs.** Same trait that drives validators in
  production drives the dev-node. Surface stays honest as hogd evolves.
- **No C ABI tax.** The dev-node ships, ships well, and never has to
  carry an FFI surface unless a future use case actually warrants it.
  The trait aperture keeps that option alive at zero cost.

## What this does NOT solve

- Historical state reads — same hogdb gap as 04-24 spec, same plan.
- Consensus / gossip / catchup — out of scope for dev-node by design;
  that's chuckd's job.
- chuckd-rs's own production-readiness — orthogonal.

## Relationship to the 04-24 embedded-backend spec

This doc partially supersedes the staging in
`specs/2026-04-24-embedded-dev-node-backend.md`. That spec presents
`libhogdev` as the durable target; this doc argues subprocess is the
durable target for the dev-node, with `libhogdev` as a contingent
option.

The 04-24 spec remains correct as a description of what `libhogdev`
*would be* if it were built — the architectural principle ("reuse
production semantics, not production topology"), the lifecycle
contract, the state model, and the execution-mode framing all transfer
to whichever backend implements the trait. What changes here is the
urgency: `libhogdev` becomes a capability hogd might develop for other
reasons (e.g. the SDK per-test scenario), not a thing the dev-node
needs to wait for.

## Open questions

1. **Where does `chuckd_scheduler` live long-term?**
   - Option A: stays in chuckd-rs; groundhog-rpc takes a path/registry
     dep. Simplest; tightest coupling to chuckd-rs's release cadence.
   - Option B: moves to a shared workspace crate (e.g.
     `groundhog-rs-common/scheduler-client`). Cleanest ownership story;
     more upfront packaging work.
   - Option C: moves to groundhog-rpc; chuckd-rs takes a dep. Sensible
     if groundhog-rpc becomes the canonical Rust-side home for
     hogd-facing protocol code.

   Short-term default: A. Revisit when the trait shape needs to change
   in lockstep with both consumers.

2. **Subprocess management.** Where does the `hogd` binary come from?
   - Option A: ship a pinned `hogd` binary alongside `groundhog`
     (single download, two files). Strongest DX, biggest distribution
     story.
   - Option B: look up `hogd` on `$PATH`. Lightest install; relies on
     user to install hogd separately.
   - Option C: `--hogd-binary` flag, no default. Most explicit,
     friction-y for users.
   
   Probably A for the CLI-distributed case, with B as a fallback for
   developers who already have hogd built locally.

3. **`DevControl` shape.** Sibling Rust trait + dev-only XDR program
   in hogd. Open: does the trait live with `SchedulerClient` (one
   crate covers "talk to hogd's RPC surfaces") or separate (production
   trait stays clean)? Lean toward separate.

4. **Snapshot/revert semantics.** Same open question as the 04-24 spec:
   do snapshots include receipts/events/traces or just DB state? Defer
   to that doc; resolve before implementing Stage 2.

5. **Conformance tests.** Where do tests that exercise expected
   outcomes against multiple `SchedulerClient` implementations live?
   `chuckd-rs testkit` already has multi-impl harness machinery;
   potentially the right home.

6. **Is FFI off the table forever, or just deferred?** This doc treats
   it as deferred. Worth being explicit about which trigger conditions
   would actually flip the decision (per-test SDK use case being the
   most likely), so we're not relitigating it every six months.

## Decision points needed before implementation

- Approve `SchedulerClient` as the dev-node's backend aperture.
- Approve **subprocess as the durable backend strategy**, with FFI
  contingent on specific future triggers rather than scheduled.
- Pick a short-term home for `chuckd_scheduler` (open Q1, Option A
  unless objections).
- Pick a subprocess management strategy (open Q2).
- Confirm gateway sink consolidation (adapters over the trait) is in
  scope alongside Stage 1.
