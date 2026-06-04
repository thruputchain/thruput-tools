# Local Target Provider (Phase 2, Slice A) — Design

**Date:** 2026-05-26
**Status:** Approved design, ready for implementation planning.
**Owner of implementation:** TBD (will be driven in a separate session).

This is a handoff design doc. It is self-contained: an implementer who has
not seen the brainstorming conversation should be able to build from it.

## Summary

Add a `LocalTargetProvider` to the `hogsim` simulator (`crates/dev/sim`) that
launches a managed `groundhog node` (which supervises a `hogd-dev`
subprocess), runs a simulation against it, and tears it down on completion.
This is the first **managed** target provider; Phase 1 shipped only the
`existing` provider, which talks to an already-running node.

The simulator continues to interact with the target exclusively over public
JSON-RPC. The local provider only adds process lifecycle *around* that; it does
not change how workloads talk to the node.

## Scope

This is **Slice A** of Phase 2 ("Local Managed Target" in
`docs/sim/sim-implementation-plan.md`). Phase 2 as written bundles four
loosely-coupled subsystems; this slice is deliberately just the foundation.

### In scope

- `LocalTargetProvider` implementing the existing `TargetProvider` trait:
  launch → hand a `ResolvedTarget` to the existing run path → graceful teardown.
- Launch by **subprocess**: shell out to `groundhog node --json`, parse its
  startup JSON for `rpc_url` and `runtime_manifest.path`.
- A new working `groundhog node --dev-account-count <N>` CLI flag (replaces the
  dead, ignored `--accounts` flag).
- **Account-count consistency, enforced pre-launch:** sim derives the required
  bootstrap-account count from the scenario and requests exactly that many, so
  a run never launches a node and then fails for too few accounts.
- `target-ownership.json` records `started_by_sim: true`, `cleanup_allowed:
  true`, `provider: "local"`.
- Provider dispatch in `run.rs`: select `Existing` vs `Local` from
  `manifest.target.provider` and run the shared `acquire → run → cleanup` flow.
- Graceful teardown: SIGTERM the node → bounded grace → SIGKILL fallback.
- Startup-failure handling (binary missing, startup timeout, unparseable
  startup JSON), each failing before any workload traffic with diagnosable
  context.

### Out of scope (deferred, with rationale)

- **Log/artifact capture** — persisting node stdout/stderr or a
  `bootstrap-manifest.json` into the run's `logs/` dir. Cleanly additive later;
  changes no provider contract. (`logs/` already exists as a placeholder.)
- **Explicit `bootstrap.accounts` manifest field** — Phase 1's model is one
  bootstrap account per actor with no hot-account rotation, so an explicit count
  can only be redundant (== actors), an error (< actors), or inert
  over-provisioning (> actors). It earns its place when Phase 3 adds rotation or
  dynamic spawning. Until then, derive from the load shape.
- **`restart-target` chaos** (Phase 2 Slice B) — depends on this slice.
- **Baseline diff** (Phase 2 Slice C) — orthogonal to the provider.
- **Sweeper command** (Phase 2 Slice D) — depends on this slice's ownership
  artifacts; it will design the ownership fields (pidfile, etc.) it actually
  needs.
- **Sim-side panic/SIGKILL orphan safety** — see "Lifecycle" below; the durable
  fix belongs in `groundhog node`, not in a sim-side guard.
- **PID in `TargetOwnership`** — not added this slice. A bare PID is the wrong
  shape (the design doc warns the sweeper must never infer ownership from a
  stale PID alone), and PIDs recycle. The sweeper slice designs its own
  ownership fields.

## Background: what this builds on

- **`TargetProvider` trait** (`crates/dev/sim/src/target.rs`) — dyn-compatible via
  boxed futures, with `acquire`, `cleanup` (idempotent), `capabilities`, and
  `chaos_controller`. `ExistingTargetProvider` is the only current impl.
  `TargetCapabilities::for_provider(Local)` deliberately declares no chaos
  until Slice B wires a real `restart-target` controller, so validation cannot
  accept an unexecutable chaos event.
- **`ResolvedTarget`** — a pure-data value type (`Clone + PartialEq + Eq`),
  threaded into events and `result.json`. It holds `provider`, `capabilities`,
  `ownership`, `rpc_url`, `runtime_manifest`. It intentionally does **not** hold
  a live process handle.
- **`run.rs`** — already runs `acquire → run → cleanup` via a `finish_target_run`
  helper that gives run-error-wins-over-cleanup-error precedence.
- **`groundhog node`** (`crates/tools/cli/src/node.rs`, feature `dev-node`) — a
  supervisor that launches and reaps `hogd-dev`, serves public JSON-RPC, and in
  `--json` mode prints a startup payload with `data.rpc_url` and
  `data.runtime_manifest.path` / `.hash`. Its `LocalNodeOptions.dev_account_count`
  is already plumbed to `HogdDevConfig::with_dev_account_count` →
  `to_bootstrap_manifest()` → hogd-dev `--bootstrap-manifest` →
  `validate_dev_account_count` against readiness. Only the *CLI surface* is
  missing a working flag; the legacy `--accounts` flag is marked deprecated and
  ignored.

## Architecture

Four units, each with one job.

### 1. `LocalTargetProvider` (`crates/dev/sim/src/target.rs`)

Implements `TargetProvider`.

- `kind()` → `TargetProviderKind::Local`.
- `acquire(manifest, overrides)`:
  1. Resolve the `groundhog` binary (see Data Flow).
  2. Compute `required_accounts` = max `users` across the manifest's load phases.
  3. Spawn `groundhog node --dev-account-count <required_accounts> --json`,
     plus optional `--hogd-dev-binary <path>` and `--port <p>` when supplied.
  4. Read the child's stdout until the startup JSON line appears, bounded by a
     startup timeout.
  5. Parse `data.rpc_url` and `data.runtime_manifest.path`.
  6. Store the live child handle in provider-owned interior-mutable state.
  7. Return a `ResolvedTarget` with `ownership.started_by_sim = true`,
     `cleanup_allowed = true`.
- `cleanup(target)`: take the stored child handle and terminate it gracefully
  (SIGTERM → bounded grace → SIGKILL fallback). Idempotent.
- `chaos_controller()` → `None` for this slice (Slice B adds the controller).

### 2. Child-handle ownership

The live child cannot live on `ResolvedTarget` (that type is `Clone + Eq +
Serialize` and is threaded into events/`result.json`). So `LocalTargetProvider`
holds it in interior-mutable provider state (e.g. a `tokio::sync::Mutex<Option<
Child>>`). `acquire` stores the child; `cleanup` takes and terminates it.

This resolves the deferred plan question "do local-provider process handles
live on `ResolvedTarget` or in provider-owned state": **provider-owned state.**
`ResolvedTarget`'s shape does not change.

### 3. `groundhog node --dev-account-count` flag (`crates/tools/cli`)

Replace the dead `--accounts` flag (`crates/tools/cli/src/main.rs`, currently
`#[arg(... hide = true)]` "Deprecated: ignored by the hogd-dev backend") with a
working `--dev-account-count <N>` wired to the already-plumbed
`LocalNodeOptions.dev_account_count`. This is the one cross-crate change. It is
independently useful: there is currently no way for a human to control the
launched account count from the CLI.

### 4. Provider dispatch (`crates/dev/sim/src/run.rs`)

Generalize the current `run_existing_target` into a provider-agnostic path that
selects the provider from `manifest.target.provider` and runs the shared
`acquire → run → cleanup` flow through the existing `finish_target_run` helper.
The `Existing` path is unchanged in behavior.

### Boundary property

Sim still only ever talks **public JSON-RPC over a socket** to the target.
The local provider adds process lifecycle; it does not change workload
interaction with the node.

## Data flow

### Binary resolution (inputs)

- **`groundhog` binary:** resolved from a new `hogsim run --groundhog-binary
  <path>` override, else looked up as `groundhog` on `PATH` (per the CLAUDE.md
  `which`-first convention). Absent → clear error naming the override.
- **`hogd-dev` binary:** sim forwards `--hogd-dev-binary` to the node *only if*
  `manifest.target.hogd_dev_binary` (or a CLI override) is set; otherwise it
  lets `groundhog node` resolve it from the existing `GROUNDHOG_HOGD_DEV_BINARY`
  env. Sim does not re-own hogd-dev resolution.

### Account-count consistency

1. Pre-launch, sim computes `required_accounts` = max `users` across load
   phases (one bootstrap account per actor, per Phase 1's model).
2. It passes `--dev-account-count <required_accounts>`.
3. The existing Phase 1 post-launch validation (actor count ≤ runtime-manifest
   bootstrap accounts) runs unchanged as a backstop invariant — it now cannot
   fail in practice because sim requested exactly enough.

The requested count is honored pre-launch; the "launch then discover you're
short" failure never happens.

### acquire → run → cleanup

```
acquire(manifest, overrides):
  resolve groundhog binary
  required_accounts = max users across load phases
  spawn: groundhog node --dev-account-count <required_accounts> --json
         [--hogd-dev-binary X] [--port P]
  read child stdout until startup JSON (bounded by startup timeout)
  parse data.rpc_url, data.runtime_manifest.path
  store child handle in provider state
  return ResolvedTarget {
    provider: Local, capabilities, rpc_url, runtime_manifest,
    ownership: { provider: Local, started_by_sim: true, cleanup_allowed: true }
  }

run (existing path, unchanged):
  copy runtime-manifest.json into run dir
  write target-ownership.json
  run workloads over rpc_url
  aggregate result.json / result.jsonl

cleanup(target):
  take child handle from provider state
  SIGTERM; wait up to grace; SIGKILL if still alive
```

### Ports

Default to the node's own `9545`, with an optional `hogsim run --port`
passthrough. Sim always reads the actual `rpc_url` back from startup JSON
rather than assuming the port.

## Lifecycle & error handling

### Teardown (correctness, not gold-plating)

`cleanup()` SIGTERMs the node rather than SIGKILLing it. The node is a
supervisor; a catchable signal lets it reap its `hogd-dev` child. SIGKILL-first
would orphan hogd-dev. So: **SIGTERM → bounded grace → SIGKILL only as a last
resort.** This is the difference between clean teardown and a leaked hogd-dev on
every run.

### Startup failures (fail before any traffic)

- `groundhog` binary not found → error naming the missing binary and the
  `--groundhog-binary` override.
- Child exits before emitting startup JSON, or the startup timeout elapses →
  capture the child's stderr, kill it, return an error with that context (so a
  bad `--hogd-dev-binary` or protocol skew is diagnosable). Startup timeout has
  a sane default, overridable via `hogsim run --startup-timeout-secs`.
- Startup JSON present but unparseable / missing `data.rpc_url` → error citing
  the field, kill the child.

New `SimError` variants (thiserror, no anyhow), each with enough context to
diagnose without re-running — e.g. `GroundhogBinaryNotFound`, `LocalNodeSpawn`,
`LocalNodeStartupTimeout`, `LocalNodeStartupExit`.

### Explicitly out of scope: panic / SIGKILL-of-hogsim orphan safety

The common stop path is terminal Ctrl-C, which delivers SIGINT to the whole
process group — `groundhog node` already cleans up hogd-dev on a catchable
signal, for free. The only genuinely-leaky case is hogsim dying *without* the
node receiving a signal (a panic, or SIGKILL of just hogsim). The durable fix
for that belongs in `groundhog node` (cooperative shutdown: parent-death
detection or an explicit shutdown channel), which benefits every programmatic
caller — not a sim-side `Drop` guard or signal handler. Tracked as a follow-up
in `docs/sim/sim-implementation-plan.md`. The local provider is deliberately the
least-hardened provider: a deployed devnet / EC2 target may make it the
non-strategic path.

## Interim nature

Dev-account bootstrapping here is explicitly interim. A faucet mechanism for a
"nightly devnet" deployment is anticipated soon; once it exists, the simulator
will fund actors the normal way (request from a faucet, distribute via ordinary
fund-transfer transactions) rather than relying on genesis-bootstrapped dev
accounts. This matters for remote/devnet targets where sim cannot dictate
genesis. Do not harden or generalize the dev-account path beyond this slice;
keep a seam for faucet-based self-funding.

## Testing

The provider spawns a real `groundhog node`, so: unit-test the pure logic, gate
the full launch behind a real-binary smoke test. No process mocking.

### Unit tests (no node required)

- **Account-count derivation** — max `users` across load phases → required
  count.
- **Startup-JSON parsing** — extract `rpc_url` + `runtime_manifest.path` from a
  sample payload; plus error cases (missing field, malformed JSON,
  node-exited-before-JSON).
- **Argv construction** — assert the `groundhog node` command line:
  `--dev-account-count <N> --json`, optional `--hogd-dev-binary`, `--port`.
- **Local-provider validation** — rejects `--rpc-url` / `--runtime-manifest`
  overrides (those are outputs for a local target, not inputs); clear error when
  the `groundhog` binary can't be resolved.
- **Provider dispatch** — `manifest.target.provider` selects `Existing` vs
  `Local`.
- **New CLI flag** — `groundhog node --dev-account-count N` maps to
  `LocalNodeOptions.dev_account_count` (mirror the existing `node.rs` test that
  exercises `dev_account_count: Some(3)`).

### Gated smoke (ignored unless a hogd-dev binary is provided)

Gate on e.g. `GROUNDHOG_SIM_HOGD_DEV_BINARY`. Launch via the local provider →
run `chain-tip-smoke` against the sim-launched node → tear down → assert the
node exited and the port is released. Sibling to the existing
`crates/dev/sim/tests/existing_target_smoke.rs`.

### Deliberately not tested

SIGTERM-vs-SIGKILL timing, grace-window fallback, panic orphan behavior — these
are integration-level, covered by the gated smoke, and not worth process-mock
scaffolding for a non-strategic provider.

## Anticipated file touch-list (handoff aid, not prescriptive)

- `crates/dev/sim/src/target.rs` — `LocalTargetProvider`, child-handle state.
- `crates/dev/sim/src/run.rs` — provider dispatch; reuse `finish_target_run`.
- `crates/dev/sim/src/error.rs` — new `SimError` variants.
- `crates/dev/sim/src/validation.rs` — allow `local`; reject output-only overrides
  for local; keep the post-launch account-count backstop.
- `crates/dev/sim/src/main.rs` — `hogsim run` flags: `--groundhog-binary`,
  `--port`, `--startup-timeout-secs`.
- `crates/tools/cli/src/main.rs` + `crates/tools/cli/src/node.rs` — replace `--accounts`
  with working `--dev-account-count`.
- `crates/dev/sim/tests/` — gated local-provider smoke.
- `docs/sim/sim-implementation-plan.md` — check off Slice A items; keep
  remaining Phase 2 items open.
- `docs/sim/README.md` — document the `local` provider and its quickstart.

## Follow-ups this slice creates or touches

- `groundhog node` cooperative shutdown for programmatic parents (tracked).
- Log/artifact capture into `logs/` (deferred).
- Sweeper command + the ownership fields it needs (Slice D).
- `restart-target` chaos (Slice B) — `chaos_controller()` returns `None` here.
- Faucet-based self-funding seam (interim dev-account path noted above).
