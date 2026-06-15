# PR 7: RV32 Default Finalization Plan

Status: draft plan

Owner: CLI / dev tooling

Goal: flip the Groundhog developer path to RV32 by default only after the CLI,
simulator, scaffolds, and `groundhog-examples` all prove representative RV32
behavior against the current local `hog-devnode` runtime.

## Context

Already landed:

- CLI deploy accepts artifact-neutral `.wasm` and `.rv32a` artifacts.
- `groundhog test --json` reports artifact metadata under `artifact`.
- `groundhog test --integration --target rv32` builds and deploys RV32.
- `hogsim` manifest/result/event schemas are artifact-neutral and can deploy
  `rv32` artifacts.
- `groundhog test --integration --differential` runs WASM and RV32 integration
  paths on isolated ephemeral local nodes and requires matching parsed test
  outcomes.
- `hog-devnode` from hogd `7ab700bf2073c8b47becc6bdf476aa2fb4f2aa35`
  executes RV32 gas table v3 / artifact v4 deployments with protocol
  `hogp/7874f05decb9`.

Known pre-PR7 blocker:

- `scripts/test-scaffold.sh` currently fails because fresh scaffold host tests
  still call `contract_exec`, which is no longer exposed by the current upstream
  SDK. Fix scaffold templates before using scaffold smoke as a PR7 gate.

## Non-Goals

- Do not remove WASM. Keep explicit `--target wasm`, explicit `.wasm` deploys,
  and WASM sim artifacts for differential debugging.
- Do not add compatibility shims for old CLI JSON or old `hogsim` schemas.
- Do not make host unit tests execute RV32. The default flip concerns artifact
  build/deploy/integration/sim execution first.

## Code Tasks

1. Change `groundhog build` default target from `wasm` to `rv32`.
2. Make deploy auto-resolution prefer `target/groundhog/<name>.rv32a` when
   `--artifact` and `--target` are omitted.
3. Make `groundhog test --integration` default to RV32.
4. Keep explicit fallback paths:
   - `groundhog build --target wasm`
   - `groundhog deploy --target wasm`
   - `groundhog test --integration --target wasm`
   - `hogsim` manifest artifact `{ "kind": "wasm" }`
5. Update help text, README, roadmap, scaffold docs, and example command text
   from WASM-default language to artifact/RV32-default language.
6. Fix generated scaffold tests for the current SDK API, then prove new counter
   and stub projects still build and test.
7. Add a dedicated examples RV32 scenario gate for `~/code/gh/groundhog-examples`.

## Required Gates

Run before merging PR7:

- `cargo fmt --check`
- `cargo clippy -p groundhog-cli --all-targets -- -D warnings`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- `cargo test -p groundhog-cli`
- `cargo test -p groundhog-sim`
- `scripts/test-scaffold.sh`
- `scripts/smoke-local-dx.sh`
- `scripts/smoke-sim-local.sh`
- `scripts/smoke-sim-scenarios.sh`
- `scripts/smoke-runtime-manifest-bench.sh`
- `groundhog test --integration --differential --hogd-protocol hogp/7874f05decb9`
- `hogsim` RV32 deploy smoke with schema v2 result/event artifact fields.
- `groundhog-examples` RV32 scenario gate described below.

For local live smokes use:

```bash
HOG_DEVNODE_BINARY=/Users/markmayo/code/gh/hogd/build/hogd/devnode/hog-devnode
GROUNDHOG_HOG_DEVNODE_BINARY=/Users/markmayo/code/gh/hogd/build/hogd/devnode/hog-devnode
HOGD_PROTOCOL_OVERRIDE=hogp/7874f05decb9
```

## Examples RV32 Gate

Add a dedicated runner and scenario manifest. The runner should live in this
repo, consume `GROUNDHOG_EXAMPLES_ROOT` defaulting to
`~/code/gh/groundhog-examples`, and treat every scenario as a hard failure unless
explicitly marked skipped with a reason.

Runner responsibilities:

1. Build every listed package with `groundhog build --target rv32`.
2. Deploy each needed `.rv32a` artifact to local `hog-devnode`.
3. Execute the representative usage pattern for that contract or contract set.
4. Assert observable behavior: receipt status, decoded return data or raw return
   data, event count/topics/payload where available, storage values where
   relevant, and expected abort/error behavior for negative paths.
5. Emit machine-readable JSON summarizing package, artifact path, deploy
   address, calls, expected observations, and pass/fail status.

Preferred runner shape:

- `scripts/smoke-examples-rv32.sh` for shell entrypoint and runtime setup.
- `docs/cli/specs/fixtures/rv32-examples-scenarios.toml` or JSON for the stable
  scenario manifest.
- Small Rust or Python helper only if shell JSON/XDR assertions become too
  brittle.

## Examples Scenario Matrix

Top-level example crates:

| Package | Scenario | Required observations |
| --- | --- | --- |
| `fibonacci` | Deploy, call/view `compute(10)`, then exercise too-large input. | `compute(10)` returns `55`; too-large input produces expected `InputTooLarge` abort/error; `compute(10)` emits `FibComputed(10, 55)` if event querying is available. |
| `events` | Deploy, call `multi_emit(test_addr)`. | Receipt succeeds; exactly three events are emitted: `ValueStored`, `Transfer`, `Checkpoint`; topic and payload checks match host test expectations. |
| `erc20` | Run lifecycle equivalent to current RPC integration test. | `initialize(admin)`, `mint(admin, 1000)`, `transfer(recipient, 400)` succeed; `total_supply` is `1000`; admin balance is `600`; recipient balance is `400`. |
| `error-handling-callee` | Deploy and exercise direct success plus failure paths. | `succeed(42)` returns `42`; `throw_error()` produces the expected contract error; `fault()` produces a deterministic fault/abort; `spin` completes for a small iteration count. |
| `error-handling-caller` + `error-handling-callee` | Deploy both and call through caller. | `call_succeed(callee, 42)` returns `42`; caught callee error/fault paths return `true`; low gas path returns `true`; nonexistent address and wrong-contract paths return `true`. |
| `cross-call` + `fibonacci` | Deploy both and call Fibonacci through cross-call. | `call_fib(fib, 10)` returns `55`; caught `InputTooLarge` path returns `true`; event path emits `FibComputed` then `CrossCallCompleted`. |
| `multi-impl` | Deploy and exercise one method from each exported impl/interface. | Representative add/multiply or equivalent calls return host-test expected values; method dispatch is interface-stable under RV32. |
| `multi-impl-traits` | Deploy and exercise trait-backed impls. | Representative trait methods return host-test expected values and expected trait-specific errors surface correctly. |
| `multi-throws` | Deploy and exercise each declared throw family. | Happy path succeeds; each representative throw maps to the expected typed error/abort data. |
| `swap` | Deploy pair plus supporting ERC20 contracts and run direct pair swap. | Liquidity priming succeeds; Carol exact-in direct swap changes balances by expected amounts; sync/buy/redeem representative paths match host tests where practical. |
| `swap_driver` | Deploy driver, pair, and tokens; run queued request flow. | Request swap succeeds; `process_requests()` reports expected processed/failed counts; balances/pledges match host expectations for one success and one failure case. |
| `swap_router` | Deploy router, pair, and tokens; run router exact-in/exact-out paths. | Exact-in swap returns expected output and updates balances/allowances; exact-out path respects max input; limit violations produce expected router errors. |

Simulator fixture crates:

| Package | Scenario | Required observations |
| --- | --- | --- |
| `sim-fixtures/caller-counter` | Deploy and run caller/counter path used by hogsim local scenarios. | Contract-call event scenario succeeds; emitted event count matches current sim smoke expectation. |
| `sim-fixtures/calldata` | Deploy and exercise representative large calldata path. | Call succeeds with expected return/status; no calldata encoding regression under RV32. |
| `sim-fixtures/nnint-stress` | Deploy, call `churn`, then view `total` and `caller_total`. | `churn(amount, refund)` returns expected value; totals reflect NNInt updates. |

## Implementation Notes For Examples Gate

- Start with single-contract examples using `groundhog deploy/call/view` and
  embedded spec metadata.
- For multi-contract examples, prefer a small integration harness over ad-hoc
  shell once calls require signing custom dev-wallet transactions or decoding
  complex event payloads.
- Reuse current host tests as the source of truth for expected values, but do not
  simply run host tests. The gate must execute deployed RV32 artifacts through
  JSON-RPC.
- If a scenario cannot be expressed with current CLI commands, document the
  missing CLI/API capability and add the smallest helper needed for PR7.

## PR7 Acceptance

- Fresh scaffold `groundhog build` produces `.rv32a` by default.
- Fresh scaffold deploy/integration uses RV32 by default.
- Explicit WASM build/deploy/integration still works.
- `groundhog test --integration --differential` passes.
- `hogsim` RV32 smoke passes and records `artifactKind: "rv32"` in result and
  event output.
- Every `groundhog-examples` scenario in the matrix builds, deploys, and passes
  representative RV32 behavior assertions, or has a documented explicit skip.
- Docs clearly state RV32 is the default and WASM is the explicit fallback for
  differential debugging.

## Current Smoke Baseline Before PR7

Observed passing on 2026-06-14 against `groundhog-rpc` main `191725b`:

- `cargo build -p groundhog-cli -p groundhog-sim`
- Explicit RV32 fixture build/deploy/call/view smoke returned `get = 15` after
  `initialize(10)` and `increment(5)`.
- `groundhog test --integration --target rv32` passed on the RV32 fixture.
- `groundhog test --integration --differential` passed on the RV32 fixture.
- `hogsim` RV32 deploy smoke passed with result schema v2 and `artifactKind` set
  to `rv32`.
- `scripts/smoke-local-dx.sh` passed.
- `scripts/smoke-sim-local.sh` passed.
- `scripts/smoke-sim-scenarios.sh` passed.
- `scripts/smoke-runtime-manifest-bench.sh` passed.

Observed blocker:

- `scripts/test-scaffold.sh` failed at generated `cargo test` because scaffolded
  tests still use removed SDK API `contract_exec`.
