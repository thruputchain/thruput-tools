# Node Supervisor x23 Client Boundary

**Date:** 2026-04-29
**Status:** Decided.

**Companion to:**

- [`../plans/2026-04-29-local-node-hogd-dev-supervisor.md`](../plans/2026-04-29-local-node-hogd-dev-supervisor.md)
  — Rust node-supervisor implementation plan; resolves open question #1.
- [`../2026-04-25-hogd-dev-node-hybrid.md`](../2026-04-25-hogd-dev-node-hybrid.md)
  — hybrid dev-node design; this decision picks among the alternatives that
  doc surfaced.

## Decision

`groundhog-node-supervisor` depends directly on the codegen x23 client
crates (`groundhog_scheduler`, future hogd-dev read/control client crates)
over `rpc_driver::FixedSerialPool`. It does **not** depend on
`chuckd_scheduler::SchedulerClient` or any other chuckd-rs crate.

This matches what `crates/gateway` does today
(`crates/gateway/src/context.rs:3-5`, `crates/gateway/src/context.rs:131-186`).

## Rationale

1. **One impl, no trait needed.** node-supervisor has exactly one scheduler
   client: TCP to hogd-dev. The trait abstraction in
   `chuckd_scheduler::SchedulerClient` exists to swap TCP/mock/embedded
   impls. node-supervisor has no alternatives in scope. The aperture that
   *does* need to swap (gateway vs. supervisor) lives one layer up in
   `rpc-server`'s `SubmitPlane`/`ChainPlane`/`ReadPlane`.

2. **No coupling to chuckd-rs release cadence.** `2026-04-25` and
   `2026-04-29` planning docs both flag this as the cost of depending on
   chuckd_scheduler.

3. **Consistency with gateway.** Same `rpc_driver::FixedSerialPool` +
   codegen-client pattern in both binaries means one mental model, one
   place to evolve, and a clear path to extracting shared transport
   helpers within this repo when the second consumer arrives.

4. **Reversible.** If chuckd-rs grows a useful test mock or shared trait
   surface, we can adopt it later. The `rpc-server` plane traits insulate
   handler code from the change; only the supervisor's adapter modules
   would move.

5. **Pre-empts nothing on the read/control side.** hogd-dev's read and
   control x23 programs are not defined yet. Whichever codegen client crate
   hogd ships will be the dependency. Picking chuckd_scheduler now would
   not cover those endpoints anyway.

## Implications for design across gateway and node-supervisor

We are choosing the same wiring style in two places. That style should be
idiomatic and shared, not copy-pasted.

- The current `FromPool` trait and `connect`/`try_connect` helpers in
  `crates/gateway/src/context.rs:131-186` are a good seed for a shared
  internal transport module.
- When PR 4a adds the first node-supervisor x23 client, hoist these
  helpers into a shared place (likely a small module in `rpc-server`, or a
  new tiny crate if `rpc-server` is the wrong home) and have gateway and
  node-supervisor both consume it.
- Keep degraded-mode/connection-loss policy per consumer. Gateway tolerates
  a missing scheduler at startup ("degraded mode"); node-supervisor should
  fail hard when its supervised hogd-dev's endpoints are unreachable, since
  it just spawned the child. The shared helper should expose both shapes.
- Pool sizing is per consumer. Gateway sizes its pool to cover the batch
  submitter, tip coalescer, and ad-hoc slack
  (`crates/gateway/src/context.rs:113-125`). node-supervisor's needs are
  smaller and can start with a fixed default.

## Alternative we considered: `chuckd_scheduler::SchedulerClient`

There is a defensible argument that `chuckd_scheduler::SchedulerClient`
should be the Rust-side aperture across dev-node, the chuckd-rs coordinator,
and a future embedded backend — one trait, three impls (TCP, mock, embedded),
swappable behind the same dependency. This decision deliberately stops short
of taking that path because:

- the unifying-aperture argument requires gateway, supervisor, and
  chuckd-rs coordinator to all consume the trait. Today only chuckd-rs
  does;
- adopting it now would block on a coordinated migration of gateway, which
  is out of scope for the current local-node work;
- node-supervisor v1 has one scheduler impl. The trait carries no value
  for v1's own consumers.

If gateway later converts to `chuckd_scheduler::SchedulerClient`,
node-supervisor should follow. That is a coordinated change at that point,
not a v1 blocker.

## Reversal triggers

Revisit this decision if any of these become true:

- gateway adopts `chuckd_scheduler::SchedulerClient`;
- a third consumer in this repo needs swappable scheduler-client impls;
- chuckd-rs ships a shared test mock that materially improves
  conformance-test coverage for node-supervisor;
- hogd-dev's wire protocol diverges enough from the production scheduler
  surface that a chuckd-rs-managed adapter layer becomes load-bearing.

## Out of scope

- Long-term home for `chuckd_scheduler` itself. That is a chuckd-rs
  question and unaffected by this decision.
- Whether the production gateway should eventually adopt
  `chuckd_scheduler`. Tracked separately in the layering doc; not on the
  local-node critical path.
