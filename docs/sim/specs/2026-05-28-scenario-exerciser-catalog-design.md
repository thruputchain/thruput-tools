# Scenario Exerciser Catalog (Phase 2, Slice F) - Design

**Date:** 2026-05-28
**Status:** Implemented.

## Summary

Build a small scenario catalog and smoke runner that exercise the simulator with
the workloads available today: `chain-tip`, `wallet-submit-smoke`, and
`receipt-poll`. Phase 2 has enough target lifecycle, restart chaos, logs,
sweeping, baseline diffing, and result artifacts to start using `hogsim` as an
operator tool. This slice deliberately puts the current surface through real
local runs before adding Phase 3 contract-state workloads.

## Scope

This is **Slice F** of Phase 2. It follows the local target provider,
restart-target chaos, baseline diffing, and local sweeper slices. It does not
add new workload kinds. The output is a curated scenario ladder, a gated script
that runs that ladder against a real local target, and documentation that tells
operators which scenario to run first.

### In scope

- Add runnable scenarios under `examples/sim/` using only current workloads.
- Cover existing-provider and local-provider entrypoints where useful.
- Cover read-only, write-only, mixed read/write, receipt polling, restart chaos,
  baseline-backed, and longer opt-in soak use cases.
- Add a scenario catalog smoke script that builds the local `groundhog` binary,
  starts managed local targets through `hogsim run`, and validates result
  artifacts.
- Extend ignored smoke coverage so the catalog script can run one named test
  instead of duplicating all result parsing in shell.
- Update simulator docs and tracker.

### Out of scope

- New workloads such as `storage-read`, `events-query`, `view-call`, or
  contract deployment.
- Remote SSH scenarios.
- Baseline threshold enforcement. Baseline diffs remain diagnostic only.
- Automatically refreshing committed baselines.
- Claiming restart chaos is multi-node resilience testing. Local restart
  scenarios remain whole-target restart checks for a single local node.

## Scenario ladder

The catalog is intentionally small and ordered by operator confidence:

1. `local-chain-tip-smoke.json` - existing fast read-only local target sanity
   check: 1 user, 3s, 1.0 Hz, `chain-tip` weight 1, `maxErrorRate: 0.001`,
   `maxChainStall: "10s"`, expected `passed`.
2. `local-wallet-submit-smoke.json` - fast write-only local target sanity
   check: 1 user, 3s, 1.0 Hz, `wallet-submit-smoke` weight 1,
   `maxErrorRate: 0.001`, `p99SubmitLatencyMs: 500`, expected `passed`.
3. `local-receipt-poll-smoke.json` - submit plus receipt polling check:
   1 user, 5s, 2.0 Hz, `wallet-submit-smoke` weight 1 and `receipt-poll`
   weight 4, `maxErrorRate: 0.001`, `maxReceiptTimeoutRate: 1.0`,
   `p99SubmitLatencyMs: 750`, expected `passed`. Artifact checks must require
   a `receiptPoll` summary with `present > 0`.
4. `local-mixed-wallet-receipt-smoke.json` - short mixed read/write/poll run:
   2 users, 5s, 1.5 Hz, `chain-tip` weight 2, `wallet-submit-smoke` weight 2,
   `receipt-poll` weight 6, `maxErrorRate: 0.001`,
   `maxReceiptTimeoutRate: 1.0`, `maxChainStall: "10s"`,
   `p99SubmitLatencyMs: 750`, expected `passed`.
5. `local-restart-chain-tip-smoke.json` - existing read-only restart smoke:
   1 user, 3s, 1.0 Hz, one restart at 1s, expected `passed` with completed and
   recovered chaos transitions.
6. `local-restart-mixed-artifact.json` - restart artifact exercise for mixed
   write/receipt workloads: 1 user, 5s, 1.0 Hz, `chain-tip` weight 1,
   `wallet-submit-smoke` weight 4, `receipt-poll` weight 4, one restart at 2s.
   The fixed seed selects wallet submits before the restart and receipt polls
   after recovery, so the scenario covers restart-loss artifact paths. It is
   expected to be allowed to fail because current restart-loss semantics can
   record `target-restarted` workload failures before assertion tolerance
   matters. The catalog script treats it as an artifact check, not a pass/fail
   smoke.
7. `chain-tip-smoke-baseline.json` - existing-provider baseline-diff example:
   existing 1 user, 3s, 1.0 Hz, expected `passed`, with a positive
   `baselineDiff.diff.workloadCountDeltas.chain-tip.completed` delta.
8. `local-mixed-wallet-receipt-soak.json` - longer gated local soak: 5 users,
   30s, 0.5 Hz, `chain-tip` weight 10, `wallet-submit-smoke` weight 20,
   `receipt-poll` weight 20, expected `passed`.

Existing-provider examples continue to use manifest placeholder values for
`rpcUrl` and `runtimeManifest`; scripts override them with the actual temporary
target. Local-provider examples omit target outputs and rely on CLI binary
flags.

## Script behavior

Add `scripts/smoke-sim-scenarios.sh`.

- Accept the same binary environment conventions as `scripts/smoke-sim-local.sh`:
  `GROUNDHOG_DEVNODE_BINARY`, optional
  `GROUNDHOG_BINARY`, optional `HOGD_PROTOCOL_OVERRIDE=hogp/<hash>`, and
  `KEEP_WORKDIR=1`.
- Build `groundhog`.
- Run a new ignored Rust smoke test that executes the fast local catalog with
  managed local targets and validates result artifacts.
- Run the existing-provider baseline scenario by starting one temporary
  `groundhog node`, overriding RPC URL and runtime manifest, and checking
  `baselineDiff`.
- Skip the longer soak by default. Run it only when
  `GROUNDHOG_SIM_RUN_SOAKS=1`.
- Print each run directory. Fail immediately if any scenario exits non-zero or
  expected artifacts are missing, except `local-restart-mixed-artifact.json`,
  where a non-zero `hogsim run` exit is acceptable only if `result.json` exists
  and contains restart chaos transitions plus restart-related failures.
- Keep the default fast catalog around 60-90 seconds after the `groundhog`
  build. Only `GROUNDHOG_SIM_RUN_SOAKS=1` may exceed that budget.

The existing `scripts/smoke-sim-local.sh` remains the focused lifecycle smoke.
It should call the scenario catalog script only if doing so does not turn the
default local smoke into a long soak; otherwise it documents the catalog script
as the next rung.

## Result checks

The Rust smoke should validate, per scenario:

- `result.json`, `result.jsonl`, `simulation.json`, `runtime-manifest.json`,
  and `target-ownership.json` exist.
- Local-provider scenarios also require `logs/local-node-*` stdout/stderr logs.
  Existing-provider scenarios do not require provider logs in the run
  directory.
- `result.json.outcome` is `passed` for non-restart fast scenarios.
- Restart scenario result contains `scheduled`, `started`, `completed`, and
  `recovered` chaos states and `result.jsonl` contains matching
  `chaos-transition` events.
- Non-restart receipt scenarios record `receiptPoll` metrics for the
  `receipt-poll` workload with `present > 0`.
- Baseline scenario result contains `baselineDiff.path` and
  `baselineDiff.diff.schemaVersion`, plus a positive
  `workloadCountDeltas.chain-tip.completed` delta.
- Docs update includes a README “scenario ladder” section and the Phase 2
  tracker records Slice F and its review gates.

## Review gates

- Review gate F1: scenario/catalog design review for scope, scenario ladder,
  and artifact expectations.
- Review gate F2: scenario semantics review for durations, rates, assertions,
  and current workload limitations.
- Review gate F3: script/process review for binary env conventions, cleanup,
  runtime cost, and result parsing.
- Review gate F4: artifact/test review for result shape, baseline checks,
  chaos checks, and backward compatibility.
- Final review gate: full slice review before commit.

Each gate uses two reviewers: behavior/lifecycle and test/artifact.

## Verification

- `cargo fmt`
- `cargo test -p groundhog-sim`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- Gated:
  - `GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-sim-local.sh`
  - `GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-sim-scenarios.sh`
  - Optional longer soak:
    `GROUNDHOG_SIM_RUN_SOAKS=1 GROUNDHOG_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-sim-scenarios.sh`
