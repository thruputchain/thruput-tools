# Hogd-Dev Binary Bootstrap (Hogd-Side Plan)

**Date:** 2026-04-30
**Status:** Draft for hogd-owner review. No groundhog-rpc work blocked on
this until the hogd team agrees on shape.

**Companion to:**

- [`../2026-04-25-hogd-dev-node-hybrid.md`](../2026-04-25-hogd-dev-node-hybrid.md)
  — overall hybrid dev-node design.
- [`../specs/2026-04-29-hogd-dev-process-contract.md`](../specs/2026-04-29-hogd-dev-process-contract.md)
  — supervisor↔child contract this binary must satisfy.
- [`2026-04-29-local-node-hogd-dev-supervisor.md`](2026-04-29-local-node-hogd-dev-supervisor.md)
  — Rust supervisor plan; PR 4a is the first consumer of a real hogd-dev.

## Why this doc exists

The Rust node-supervisor is shipped through PR 3 with stub backend planes.
The next planned PR (4a, `groundhog_chainTip` end-to-end) needs a real x23
endpoint to talk to. We considered building a fake `RpcSender` test fixture
in groundhog-rpc instead, but that locks in guesses about a wire contract
hogd owns. Better to build the smallest real `hogd-dev` binary in hogd and
let it be the source of truth.

This plan proposes how to do that with the **least disruption to the hogd
codebase**. It is opinionated about scope reduction so that v1 is small
enough to land alongside other hogd work without conflict.

## Existence proof we are leaning on

Per Jack (hogd), the reference for "wire the normally-separate hogd
binaries together inside one process" is:

- `hogd/hogd/mempool/test/infra.h` — the `MempoolTestInfra` class.
- `hogd/hogd/mempool/test/mempool_test.cc:42` — `MempoolTestInfra server;`
  — one C++ line stands up the whole stack.

`MempoolTestInfra` already wires the following components into one
process:

- `RpcServer<MempoolDbRpc>`, `MempoolEventRpc`, `MempoolExecutorRpc`,
  `MempoolCoordinatorRpc`, `MempoolAuthRpc`
- `HogDBExecutorServer`, `HogDBSchedServer`, `HogDBAuthServer` over
  `RAMDB`-backed `HogDBDAO`
- `Executor`, `AuthServer`
- `TestEventAggregator{Sched,Exec}Server`
- a fake `RpcServer<CoordinatorSchedRpc>` for the back-channel
- `GenesisConfigurationBuilder::build(default_cost_model)` loaded via
  `schedcoord_load_genesis_configuration`

That is the topology hogd-dev wants. The work is to lift it from a test
class into a long-running binary surface.

## Proposed v1 scope (smallest credible bootstrap)

### What v1 ships

- New module `hogd/hogd_dev/` with one library + one executable.
- The full mempool topology runs internally — `RpcServer<...>` instances
  for db/event/exec/coord/auth, the hogdb `Sched/Exec/Auth` servers, the
  auth servers, the event aggregators, and a fake coordinator-side
  back-channel server. **All bind to loopback on port 0.**
- One **externally advertised** endpoint in the readiness JSON:
  `scheduler` (the `MempoolCoordinatorRpc`'s `SchedulerCoord1` bind).
  All other internal services are loopback-only and not part of the
  supervisor contract.
- `RAMDB`-backed ephemeral state. Uses production-safe
  `Directory::open_temporary()` from `hogd/base/io.cc:361-371`
  (`mkdtemp`-based) — **not** the test-only `TmpDir` from
  `hogd/base/test.h`. The `HogdDevTmpDir` RAII wrapper handles cleanup
  on shutdown; see "Temp directory lifecycle" below.
- Readiness JSON line on stdout matching the v1-reduced supervisor
  contract (scheduler endpoint only).
- Stderr logs via existing `hogd::info/warn/error` (free-form text in v1
  — the contract spec already tolerates non-JSON during bring-up).
- SIGTERM → handler sets a `sig_atomic_t` flag; main thread observes
  via `sigsuspend(2)` (race-free vs. the older `pause()` pattern) and
  returns normally from `hogd_dev_init`. RAII destructors then run —
  including `HogdDevNode`'s teardown sequence and the `HogdDevTmpDir`
  cleanup — so the temp dir and RAMDB contents are removed. The
  supervisor's 5s SIGKILL escalation remains the safety net for any
  destructor hang or exception, not the primary mechanism.

Genesis is **not** loaded inside hogd-dev. The supervisor (Rust side)
loads genesis after the readiness handshake — see "Genesis ownership"
below.

### What v1 deliberately does NOT ship

- **No read/control endpoints.** No new `.x` interface. The supervisor
  process contract has a "v1 Reduced Contract" section that makes
  `read` and `control` *optional* in the readiness JSON until those
  interfaces are designed. PR 4a needs only `chainTip`, which is
  already on the scheduler endpoint as `schedcoord_get_tip`.
- **No persistence.** Ephemeral only.
- **No real coordinator.** Copy the trivial fake `CoordinatorSchedRpc`
  (~30 lines, 4 no-op methods) from `hogd/mempool/test/coordinator.h`
  into `hogd/hogd_dev/fake_coordinator.h`. **Do not depend on the
  test-only header** from a shipped binary, and don't re-home shared
  test code as part of this PR.
- **No test-util event aggregator dependency.** `MempoolTestInfra`
  pulls `TestEventAggregator{Sched,Exec}Server` from
  `hogd/hogexec/runtime/test_util/test_event_aggregator.h`. Shipped
  binaries should not depend on `*/test_util/` headers, so v1 hogd-dev
  ships its own minimal event back-channel in
  `hogd/hogd_dev/dev_event_aggregator.h` modeled on the same shape:
  - one struct implementing `EventExecutor1`
    (`eventexecutor_open_block`, `eventexecutor_close_block`,
    `eventexecutor_emit_event`) — all return `RPCError::kRPCNone` /
    success; no event retention; minimal block-id refcount tracking
    only if executor correctness requires it.
  - one struct implementing `EventSched1` (`eventsched_open_block`,
    `eventsched_close_block`, `eventsched_abandon_block`) — same
    no-op pattern, returning `EventBlockResult` success.
  - matching `static_assert(is_EventExecutor1<...*, x23::MaybeAsync>)`
    and `static_assert(is_EventSched1<...*, x23::MaybeAsync>)`.
  - matching `class FooServer : public RpcServer<FooRpc>` wrapper
    classes if the binding pattern needs them.

  Total expected size ~50-80 lines. If hogd grows a stable
  `event-server` library that's appropriate for non-test use, hogd-dev
  can switch to it later without supervisor-contract changes.
- **No clean-shutdown promise to the supervisor.** v1 hogd-dev *does*
  attempt clean teardown on SIGTERM (the `HogdDevNode` destructor runs,
  inheriting `MempoolTestInfra`'s ordering), but the **supervisor
  contract does not depend on that succeeding**. MempoolTestInfra's
  destructor is documented as fragile (jhumphri: "particularly prone to
  exceptions"). The destructor's `try/catch + CHECK(false)` makes any
  failure loud and immediate (process aborts on uncaught exception);
  the supervisor's 5s SIGKILL escalation handles hangs. Net: the happy
  path cleans up after itself; the unhappy path is loudly killed
  within the grace window.
- **No NDJSON log formatter.** The supervisor contract already says it
  tolerates non-JSON during bring-up. NDJSON can come later as a polish
  PR alongside whatever logging-formatter work makes sense for hogd as a
  whole.
- **No bundled binary distribution work.** Source-tree builds find the
  binary via `--hogd-dev-binary` or `PATH`. Release packaging is a later
  concern.

This carves the problem so the v1 hogd PR is mostly *additive code in a
new module* with no edits to `mempool/`, `hogdb/`, `hogexec/`, `auth/`,
or any shared infra.

## Module layout

```
hogd/hogd/hogd_dev/
  CMakeLists.txt           # one library + one executable
  hogd_dev.cc              # main(): args + hogd_init + constructs
                           # HogdDevNode + readiness print + waits on
                           # SIGTERM flag
  hogd_dev_node.h          # HogdDevNode class — non-test analogue of
  hogd_dev_node.cc         # MempoolTestInfra
  fake_coordinator.h       # tiny CoordinatorSchedRpc (copied from
                           # hogd/mempool/test/coordinator.h; ~30 lines,
                           # 4 no-op methods)
  dev_event_aggregator.h   # minimal hogd-dev-local event back-channel
                           # (~50-80 lines; no-op success returns) —
                           # replaces the test-util TestEventAggregator
                           # dependency from MempoolTestInfra
  hogd_dev_tmp_dir.h       # tiny RAII temp-dir wrapper; cleans up on
  hogd_dev_tmp_dir.cc      # both normal exit and SIGTERM (since SIGTERM
                           # returns from main and runs destructors).
                           # See "Temp directory lifecycle"
```

`hogd/hogd/CMakeLists.txt` gains exactly one line:
`add_subdirectory(hogd_dev)`.

`HogdDevNode` reuses **the same component wiring and registration
order** as `MempoolTestInfra` — `RpcServer<...>` declarations,
`register_db_with_mempool()` / `register_event_with_mempool()` /
`register_auth_with_mempool()` / `register_executor_with_mempool()` /
`register_coordinator_with_mempool()` calls, port-0 binds, etc. It does
**not** inherit:

- `init_contracts()` — loads `fibonacci.wasm`, test-only.
- `grant_initial_tokens_for_account` / `set_hogs` — test-only seeding.
- The `if (!skip_genesis_block) { ... schedcoord_load_genesis_
  configuration(...) }` block at the end of the constructor — genesis
  is **supervisor-side** in v1 (see "Genesis ownership" below).
- `set_address_auth_data` / `set_dbtip` / `set_hogs` accessors.
- The `mempool_coord_rpc()` / `coord_sched_rpc()` /
  `event_sched_rpc()` gtest getters.

Strip those and what remains is the production-shaped wiring
hogd-dev needs. Cleanup ordering documented in the
`MempoolTestInfra` destructor is **a useful reference** for what a
*future* clean-teardown path could look like, but is not promoted into
the v1 supervisor contract — see shutdown semantics below.

## Temp directory lifecycle

`Directory::open_temporary()` (`hogd/base/io.cc:361-371`) creates a
`/tmp/ramdb_XXXXXX` directory via `mkdtemp` but **does not remove it on
destruction**. v1 ships a tiny `HogdDevTmpDir` RAII wrapper in
`hogd/hogd_dev/hogd_dev_tmp_dir.{h,cc}` that owns the path and calls
`std::filesystem::remove_all` from its destructor (with the
`error_code` overload, so a failed cleanup logs a warning rather than
throws).

Because the SIGTERM path returns normally from `hogd_dev_init` (rather
than calling `_Exit`), the `HogdDevTmpDir` destructor runs on every
exit path the supervisor exercises in practice. Net: **no temp-dir
leak under normal operation**.

The remaining leak windows are:

- A destructor hang during shutdown that triggers the supervisor's
  SIGKILL escalation. SIGKILL skips the destructor, so the temp dir
  is left behind. The same applies to crash-by-uncaught-exception
  paths after the destructor's `CHECK(false)` aborts.
- A `kill -9` from the developer or OOM killer.

In all of those, `/tmp` reaping at boot bounds the leak. If
accumulation in `/tmp` becomes a problem on systems where `/tmp` isn't
tmpfs, follow-up options include: (a) a startup sweep that removes
`/tmp/ramdb_*` directories older than N hours, or (b) `O_TMPFILE`-style
auto-unlink-on-close patterns. Neither is in v1 scope.

## Code style discipline

The hogd codebase is **bespoke, handcrafted, performance-critical
C++23**. The hogd-dev PR must read like one of the lead hogd C++
developers wrote it. Mechanical conformance to `clang-format` /
`clang-tidy` is the floor, not the ceiling — patterns and idioms have
to match too.

### Primary rule: mimic surrounding code

The single most important rule is **read the surrounding files in
the directory you're touching, and match what they do.** Style
references below are useful to *understand* the conventions; the code
itself is the source of truth. When the references and the
surrounding code disagree, the surrounding code wins. (Per hogd's
`CLAUDE.md`: "Don't reformat or 'modernize' code that already follows
the established pattern. Match the surrounding file.")

For this PR specifically that means:

- `HogdDevNode` mirrors `MempoolTestInfra` — same idioms, member
  ordering style, registration-helper shape, comment voice.
- `hogd_dev.cc::main` and `hogd_dev_init` mirror `mempool.cc::main` /
  `mempool_init` and `hogdb.cc::main` / `hogdb_init` — same arg
  parsing shape, same `hogd_init` invocation, same blocking pattern,
  same `usage()` style.
- `fake_coordinator.h` mirrors `mempool/test/coordinator.h` byte-for-byte
  in the parts we copy.

### Style references

`x23/AGENTS.md` is written for the x23 subdirectory specifically, but
spot-checking hogd code outside x23 confirms the conventions transfer
broadly. K&R function definitions, `x23::Async<T>` / `Detached` /
`DestroyInhibit` patterns, `(this auto)` on coroutine lambdas with
captures, naming conventions, "explain why not what" comment voice,
RAII discipline, `std::expected` for fallible ops — all present in
hogd code outside x23 (e.g. `hogd/hogdb/auth_change_subscription_
manager.{h,cc}` is a faithful representative).

The narrow exception observed: the `// -*-C++-*-` modeline at the top
of x23 headers is **not** carried into hogd headers, which open with
`#pragma once` directly. Match the convention in the directory you
are creating files in (`hogd/hogd_dev/` lives next to the rest of
hogd, so no modeline).

### Required reading before writing code

1. **`hogd/CLAUDE.md`** — repo-level conventions and the explicit
   "match the surrounding file" instruction.
2. **`hogd/x23/AGENTS.md`** — the canonical style/idiom reference.
   The "Code style" section (formatting, naming, patterns, CTAD,
   `(this auto)` on coroutine lambdas, RAII discipline, `std::expected`
   vs. exceptions, concepts + `static_assert` for RPC server impls)
   is the most detailed style writeup in the repo and applies broadly
   to hogd, not just to the x23 subdirectory.
3. **`hogd/hogd/mempool/test/infra.{h,cc}`** — our direct template
   for `HogdDevNode`. Whatever style decisions show up here we mirror.
4. **`hogd/hogd/mempool/mempool.cc`** + **`hogd/hogd/hogdb/hogdb.cc`**
   — the existing daemon-binary patterns (`*_init` function, arg
   parsing, `hogd_init`, blocking on
   `std::promise<void>().get_future().wait()`). hogd-dev should look
   like a sibling of these.
5. **`hogd/hogd/hogdb/auth_change_subscription_manager.{h,cc}`** —
   a representative example of the prevailing hogd style for
   non-binary, non-test code (`x23::Async`/`Detached`,
   `DestroyInhibit`, `using` aliases, K&R defs).

### Style points specifically relevant to this PR

These are not exhaustive — the references above are. Calling them out
here because they show up in the v1 surface area:

- **K&R function definitions** with return type on its own line:
  ```cpp
  int
  hogd_dev_init(...)
  {
  ```
  Both `mempool.cc::mempool_init` and `hogdb.cc::hogdb_init` follow
  this. So does every function in `MempoolTestInfra`.
- **Naming.** Types `PascalCase` (`HogdDevNode`), functions
  `snake_case` (`run_hogd_dev`, `start_servers`), namespace-scope
  constants `kCamelCase` (`kReadinessTimeoutSeconds`), member variables
  trailing-underscore (`scheduler_server_`, `db_executor_server_`).
- **`x23::` qualified at use site.** hogd `.cc` files outside the x23
  subdirectory do **not** put `using namespace x23;` at the top — they
  fully qualify (`x23::Async<void>`, `x23::CoroPoll`, etc.). Match
  that. (`x23/AGENTS.md`'s "`using namespace x23;` in .cc files" rule
  applies to the x23 subdirectory only; hogd code outside x23 is
  qualified.)
- **RAII.** No raw `new`/`delete`. Use `std::unique_ptr`, `UniqueFd`,
  `Defer`, etc. `MempoolTestInfra` keeps its `RpcServer<...>` members
  as direct (non-pointer) fields where possible, ordered for correct
  destruction. Mirror that.
- **RPC server impl pattern.** Our `fake_coordinator.h` copy must
  preserve the established shape:
  ```cpp
  class CoordinatorSchedRpc {
  public:
    using rpc_service = CoordScheduler1;
    static x23::Async<void> coordscheduler_null() { co_return; }
    // ...
  };
  static_assert(is_CoordScheduler1<CoordinatorSchedRpc *, x23::Async>);
  ```
  Don't change names, don't add a base class, don't replace the
  static_assert with anything.
- **Coroutine-lambda captures.** Any unnamed coroutine lambda with
  captures used for `Detached{}` / fire-and-forget needs `(this auto)`
  per x23/AGENTS.md. v1 hogd-dev probably doesn't introduce any new
  ones, but if SIGTERM handling or readiness emission grows a
  detached coroutine, this rule applies.
- **`std::expected<T, Error>`** for fallible operations.
  `Errno{}.raise(...)` for unrecoverable errors. Don't introduce
  exception-based error returns where `std::expected` is the
  established pattern.
- **`#if NDEBUG`** (not `#ifdef`) for debug-only blocks. RPC tracing
  toggles via `rpc_stream` vs. `rpc_stream_logged`, controlled by the
  same kind of macro the rest of hogd uses.
- **Comments.** Explain *why*, not *what*. No docstring boilerplate.
  jhumphri-style comments — short, direct, name the constraint or
  invariant — match the surrounding code.
- **Includes.** Standard library alphabetical, blank line, POSIX,
  blank line, project includes (`"hogd/..."` then `"x23/..."`).
  `MempoolTestInfra` headers are the local reference.

### SIGTERM handler async-signal safety

Per the shutdown-semantics section above, the SIGTERM handler is
restricted to async-signal-safe operations:

- `volatile sig_atomic_t shutdown_requested` flag write — fine.
- **No** logging, allocation, or coroutine work inside the handler.
  Logging happens on the main thread after observing the flag.

The main-thread observer uses `sigprocmask(SIG_BLOCK, ...)` +
`sigsuspend(2)` rather than `pause(2)`. The `pause()` form has a
TOCTOU window between checking the flag and entering the kernel-side
sleep — if SIGTERM arrives in that window, `pause()` blocks
indefinitely and only the supervisor's SIGKILL escalation breaks the
wait. The `sigsuspend` pattern atomically swaps the signal mask and
sleeps, so flag-check and sleep are race-free with respect to
SIGTERM. This is the canonically-correct shape and what v1 ships.

### Linting

```bash
# From hogd repo root, with a configured build/ dir:
./scripts/format.sh -c               # format check (clang-format + cargo fmt + shfmt)
./scripts/tidy.sh -d build           # clang-tidy + clippy + flake8
./scripts/check.sh -p -b             # full local CI parity
```

`tidy.sh` treats all warnings as errors. The hogd-dev PR must be clean
on the standard gcc/Debug, clang/Debug, and gcc/Release builds before
review.

## CLI shape

```
hogd-dev \
  --listen-host 127.0.0.1 \
  --scheduler-port 0 \
  --readiness stdout
```

`--read-port` and `--control-port` are NOT accepted in v1. When they are
defined, they get added without breaking compatibility (additive flags +
additive readiness fields). No `--genesis-*` flag in v1: genesis is
loaded by the supervisor over the scheduler endpoint after readiness.

## Readiness JSON, v1 reduced shape

```json
{
  "status": "ready",
  "network_id": "groundhog-local",
  "endpoints": {
    "scheduler": "127.0.0.1:12345"
  }
}
```

When read/control endpoints exist, they appear as additional keys under
`endpoints`. The supervisor side reads `endpoints[name]` defensively and
treats missing keys as "this plane is not yet provided by hogd-dev"
rather than as a startup failure. The corresponding rpc-server backend
plane stays stub/NotImplemented in the supervisor until both sides
support the plane.

## Shutdown semantics

v1 hogd-dev attempts a clean teardown on SIGTERM, but the **supervisor
contract treats clean teardown as best-effort, not load-bearing**.
Specifically:

- A SIGTERM handler sets a `volatile sig_atomic_t shutdown_requested`
  flag. The handler does no work beyond setting the flag — async-signal
  safety only.
- The main thread observes the flag via the `sigprocmask` +
  `sigsuspend` pattern (race-free vs. the older `pause()` form, which
  has a TOCTOU window between the flag check and the kernel-side
  sleep). When `sigsuspend` returns and the flag is set,
  `hogd_dev_init` **returns normally** from main.
- C++ runtime then runs RAII destructors: `HogdDevNode` performs the
  same scheduler-reset / scheduler-shutdown / executor-shutdown /
  reverse-declaration teardown sequence as `MempoolTestInfra`, and
  `HogdDevTmpDir` removes the RAMDB temp directory.
- The destructor body is wrapped in `try { ... } catch (const
  std::exception &e) { error(...); CHECK(false); }`. Any exception
  during teardown is logged loudly and aborts the process —
  exception-during-shutdown becomes a visible failure rather than a
  silent corruption.
- The supervisor sends SIGKILL after the 5s grace if the child is
  still running. This is the safety net for destructor hangs (which
  the `try/catch` cannot catch).

Why this is the right tradeoff: MempoolTestInfra's destructor is
documented as fragile (jhumphri: "particularly prone to exceptions").
The original v1 plan responded by skipping the destructor entirely on
SIGTERM via `_Exit`, accepting a temp-dir leak per session as the
cost. Running the destructor in practice avoids that user-facing leak,
and the existing `try/catch + CHECK(false)` already turns
destructor-fragility into a loud, supervisor-killable failure mode
rather than silent corruption. The supervisor's 5s SIGKILL handles
the remaining hang case. When hogd grows a real component-shutdown
story (which it will need for production reasons, not just hogd-dev),
hogd-dev's destructor can be replaced with that proper teardown
without changing the supervisor contract — the supervisor still just
sends SIGTERM and waits up to 5s.

## Genesis ownership

Genesis loading is **supervisor-side**, not hogd-dev-side. There is no
chuckd coordinator in the dev loop, so the supervisor takes on the
genesis-load responsibility that chuckd carries in production.

Why this split:

- Production hogd never bakes genesis into a binary; chuckd loads it via
  `schedcoord_load_genesis_configuration` on startup. Mirroring that
  pattern in dev keeps hogd-dev close to "headless production hogd."
- The supervisor already owns dev account generation (per the
  hybrid-design doc). Funded-accounts genesis (v1.1+) belongs in the
  same place as the keys.
- hogd-dev becomes simpler: no `--genesis-*` flag, no startup branch
  that calls into `GenesisConfigurationBuilder`, no decision about
  default cost model.

### v1 startup flow

```text
supervisor                          hogd-dev
─────────────────────────────────────────────
spawn child                  →     reads args, hogd_init
                                   binds scheduler port
readiness handshake          ←     emits readiness JSON
connect SchedulerCoord1
schedcoord_get_tip           →
                                   returns NoGenesisBlock
schedcoord_load_genesis_     →
  configuration(dev_genesis)
                                   loads, returns OK
schedcoord_get_tip           →
                                   returns tip(height=0)
start JSON-RPC server
```

`dev_genesis` is a `GenesisConfiguration` with **empty `initial_config`
and the hogd-default `StorageCostModelXdr`** (constants mirrored from
`hogd/storage_cost/storage_cost_model.h:83-96` — see the cost-model
section below). The "no-initial-deployments + non-zero default cost
model" combination keeps PR 4a's chainTip cheap to bring up while
already running against a representative cost configuration so
PR 4b/4c don't have to re-light genesis.

The `get_tip → NoGenesisBlock → load → re-tip` retry loop is **lifted
from chuckd-rs** (see provenance below). The chuckd-rs
`empty_genesis_config()` we lift constructs an *all-zero* cost model;
in our copy we replace the cost-model fields with the hogd defaults.
No new `chuckd-rs` Cargo dep — we copy the ~25-line builder and the
~30-line load dance with comments pointing back to the source.

### Provenance (chuckd-rs lift)

- Genesis builder: `chuckd-rs/crates/scheduler/src/client.rs:324-346`
  (`empty_genesis_config()`). Pure XDR construction over
  `groundhog_xdr::storage_cost::StorageCostModelXdr` and
  `groundhog_scheduler::GenesisConfiguration`. Both crates are already
  on our supervisor's dep tree via the registry.
- Load dance: `chuckd-rs/crates/coordinator/src/system.rs:524-557`
  (the "Genesis check" block in `System::start`). Calls `get_tip`, on
  `NoGenesisBlock` calls `load_genesis_configuration`, then re-checks
  tip. The loadgen-bench path in `chuckd/src/bench.rs:457-475` uses the
  same dance with a richer override; we don't need that override for v1.
- Funded-dev-accounts genesis (v1.1+): chuckd-rs does **not** have a
  reusable Rust builder for this. Its loadgen path delegates to a
  separate `generate_experiment` RPC and XDR-decodes the resulting
  blob. When we want this, we will write our own builder using
  `GenesisConfiguration` + `PresetDeploymentConfig`, with the C++
  `GenesisConfigurationBuilder::add_initial_config` (in
  `hogd/genesis/genesis_configuration.h`) as the reference shape.

### Cost model

`empty_genesis_config()` from chuckd-rs constructs `StorageCostModelXdr`
with **all-zero** fields. For chainTip (read-only) this works. For
`sendRawTransaction` and contract-deploy flows in PR 4b/4c, a zero-cost
model probably misbehaves — hogd's production code paths assume
non-zero costs in places, and the divergence from production semantics
gets worse the more we test.

**Decision: mirror hogd's `default_cost_model()` from PR 4a onward.**

Constants at `hogd/storage_cost/storage_cost_model.h:83-96`:

```text
base_data_item_cost                = 12000
bytestring_cost_per_byte           = 10
base_contract_deployment_cost      = 12000
contract_deployment_cost_per_byte  = 10
auth_per_key_cost                  = 1111
rcq_cost_per_entry                 = 8
replay_cache_cost_per_entry        = 7
balance_cost                       = 1234
```

Adds ~10 lines to PR 4a (replace the all-zero `StorageCostModelXdr`
construction inside the lifted `empty_genesis_config()` with these
values), removes the cost-model question from PR 4b, and starts PR 4a's
chainTip already running against a representative cost configuration.
A comment in the Rust source points at the C++ constants so any future
hogd update is easy to mirror.

If the hogd team prefers a dedicated `dev_cost_model()` distinct from
production's default, we adopt that instead and drop the mirror.

### What this changes for hogd-dev

Removed from v1:

- `--genesis-default` / `--genesis-stdin` flags.
- Embedded `GenesisConfigurationBuilder` call on startup.
- Any decision about default cost model inside hogd-dev.

`MempoolTestInfra`'s `if (!skip_genesis_block) { ... build(...) ...
schedcoord_load_genesis_configuration(...) }` block is **not** inherited
into `HogdDevNode`. The corresponding work happens supervisor-side.

## Open questions for hogd owners

1. **Fake coordinator location.** v1 plan: **copy** the
   `CoordinatorSchedRpc` from `hogd/mempool/test/coordinator.h` into
   `hogd/hogd_dev/fake_coordinator.h` (~30 lines, 4 no-op methods). No
   re-homing of shared test code in this PR. Confirm OK?

2. **`hogd_init` reuse.** `hogd-dev` plans to call `hogd::hogd_init(
   argc, argv, ...)` like the other daemons. Does that init path do
   anything that's wrong for a single-process composite (e.g.,
   per-binary-specific resource registration)? If yes, what's the
   right minimum subset to call?

3. **Cost model.** Supervisor side plans to mirror hogd's
   `StorageCostModel::default_cost_model()` (constants at
   `hogd/storage_cost/storage_cost_model.h:83-96`) in Rust from PR 4a
   onward. Is that the right model for dev, or is there a dedicated
   `dev_cost_model()` we should mirror instead? (If neither, we'd start
   with chuckd-rs's all-zero `empty_genesis_config()` for PR 4a only,
   then switch in PR 4b.)

4. **WASM contract loading.** MempoolTestInfra preloads `fibonacci.wasm`
   into hogdb. v1 hogd-dev will skip that. Confirm hogd-dev does not
   need any baked-in contracts for `chainTip` to work — the supervisor
   provides contracts via `groundhog_sendRawTransaction` deploy paths
   later.

5. **Long-term ownership.** Is the hogd-dev module owned by the same
   team that owns `mempool/`, or should it land under a different
   subdirectory (e.g., `hogd/dev/`) to signal a distinct lifecycle?

6. **Build target packaging.** Is there a hogd CMake convention for
   "executables that ship to end users" vs. "tools/utilities"? hogd-dev
   should land in the former bucket.

## Acceptance criteria for hogd-dev v1

The hogd PR is "done" for v1 when all of these pass:

1. `hogd-dev --listen-host 127.0.0.1 --scheduler-port 0 --readiness
   stdout` exits with status 0 on SIGTERM and emits exactly **one**
   readiness JSON line on stdout (machine protocol; no other stdout
   output).
2. Stderr logs (free-form text via `hogd::info/warn/error`) do **not**
   appear on stdout. Stdout is reserved for the readiness line.
3. The readiness JSON's `endpoints.scheduler` field reports the
   actually-assigned port (not 0). Internal services bind on loopback
   port 0 and are not advertised.
4. An external `SchedulerCoord1` x23 client connecting to the advertised
   scheduler port can call `schedcoord_get_tip` and receive a response.
   (No external `schedulercoord_intro_coord` call is needed — `HogdDevNode`
   performs the in-process fake-coordinator registration during startup;
   the supervisor only ever calls `schedcoord_get_tip` and
   `schedcoord_load_genesis_configuration` over the wire.)
5. Before the supervisor loads genesis, `schedcoord_get_tip` returns the
   `SchedulerGetTipNoGenesisBlock` outcome.
6. After `schedcoord_load_genesis_configuration(dev_genesis)` returns OK
   (where `dev_genesis` carries empty `initial_config` and the
   hogd-default cost model), `schedcoord_get_tip` returns a tip with
   height 0.
7. SIGTERM causes the process to exit within 5 seconds (well under the
   supervisor's grace window).
8. The binary builds in the standard hogd CMake configurations
   (gcc/Debug, clang/Debug, gcc/Release) and is reachable via
   `make -C build hogd-dev` (or whatever the agreed CMake target name
   is).
9. `clang-tidy` and `clang-format` pass on the new files (per hogd's
   standard `./scripts/check.sh -p -b` flow).
10. **Style review** — a hogd C++ owner reads the diff and confirms
    the code reads like a sibling of `mempool.cc` / `hogdb.cc` /
    `MempoolTestInfra`, per the "Code style discipline" section
    above. Mechanical lint passing is necessary but not sufficient.

These can be exercised either by an integration test inside hogd
(spawning the binary as a subprocess) or by a parallel test on the
groundhog-rpc side once a build is available. The hogd PR should at
minimum include checks 1, 5, 6, 7 as gtest cases against the binary
target.

## What the groundhog-rpc side commits to

Independent of hogd-dev, the supervisor side will:

- ~~Amend the process contract spec to make read/control endpoints
  optional in the v1 readiness JSON.~~ Done as part of this batch —
  the spec now has a "v1 Reduced Contract" section that overrides the
  long-form contract for v1.
- Plumb a `--hogd-dev-binary` flag (already in place) and treat absent
  read/control endpoints as "stub plane stays NotImplemented" rather
  than as a startup failure.
- Add the supervisor-side `empty_genesis_config()` builder and
  `get_tip → NoGenesisBlock → load_genesis → re-tip` startup dance,
  lifted from chuckd-rs with provenance comments. No new `chuckd-rs`
  dep.
- Once hogd-dev v1 is buildable locally, write PR 4a as: a real
  `ChainPlane` adapter wrapping `Arc<dyn RpcSender>`, calling
  `schedcoord_get_tip`, plus the supervisor-side genesis bootstrap and
  an end-to-end CLI integration test that spawns the real hogd-dev
  binary and asserts `groundhog_chainTip` round-trips.

## Questions for hogd owners, summarized

> Is a new `hogd/hogd_dev/` module — built directly on the
> MempoolTestInfra wiring, with the carved scope above — an acceptable
> minimum first PR? If yes, whose calendar is it on, and what's the
> realistic cycle time so the groundhog-rpc supervisor team can plan PR
> 4a around it?

If the answer is "yes but not soon," the supervisor side falls back to
building a fake-x23 `RpcSender` in groundhog-rpc to unblock PR 4a, with
the explicit caveat that the fake's wire contract is a placeholder until
hogd-dev defines it.
