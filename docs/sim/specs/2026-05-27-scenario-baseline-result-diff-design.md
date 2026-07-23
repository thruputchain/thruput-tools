# Scenario Baseline Result Diff (Phase 2, Slice D) - Design

**Date:** 2026-05-27
**Status:** Implemented; retained as a historical handoff design.
**Owner of implementation:** TBD.

This is a handoff design doc. It is self-contained: an implementer who has
not seen the planning conversation should be able to build from it.

The examples below describe the compare schema at implementation time. The
current schema and semantics are documented in `docs/sim/README.md`.

## Summary

Honor the simulation manifest's optional `baseline` field during `hogsim run`.
When present, the runner loads the referenced committed `result.json`, compares
it with the just-completed in-memory result, and embeds the structured
baseline-anchored diff inside the final `result.json`.

The existing `hogsim compare` logic remains the single source of truth for
diff semantics. Baseline diffing is an in-run use of that same structured diff,
not a second comparison format.

## Scope

This is **Slice D** of Phase 2. Earlier Phase 2 slices added the local managed
target, local `restart-target` chaos, and provider log capture.

### In scope

- Resolve `baseline` relative to the scenario manifest path when it is not
  absolute.
- Accept either a direct `result.json` path or a completed run directory, using
  the same loading semantics as `hogsim compare`.
- Reject unsupported baseline `result.json` schema versions with the existing
  unsupported-schema error.
- Compute the diff as `current - baseline`, matching `hogsim compare`'s
  existing `b - a` convention.
- Embed the result under an additive optional field in `result.json`; no schema
  bump is required.
- Preserve `hogsim compare` output shape and behavior.
- Add unit coverage for path resolution, embedded diff shape, no-baseline
  omission, schema rejection, and compare JSON round-tripping.
- Update simulator docs and the Phase 2 tracker.

### Out of scope

- Automatically updating baselines on merge to main.
- Failing a run because a baseline diff exceeds a threshold.
- Percentage regression thresholds.
- HTML or rich diff rendering.
- A separate baseline artifact schema.
- Streaming baseline comparison events into `result.jsonl`.

## Artifact Contract

`SimResult` gains an optional field:

```json
{
  "baselineDiff": {
    "path": "baselines/local-wallet-soak.result.json",
    "diff": {
      "schemaVersion": 1,
      "sameConfig": true,
      "outcomeA": "passed",
      "outcomeB": "passed"
    }
  }
}
```

The `path` value is the manifest-authored path string. This keeps the artifact
stable across machines and output directories. The diff is exactly the
structured compare diff that `hogsim compare --json` emits today, with the
baseline result as side A and the current run result as side B.

If `baseline` is absent, `baselineDiff` is omitted from serialized
`result.json` and defaults to `null`/`None` when older results are read.
New augmented results must still deserialize through `load_result` and remain
valid inputs to `hogsim compare`.

## Runner Semantics

1. Load and validate the simulation manifest as today.
2. Execute the run and build the normal `SimResult` in memory.
3. If `manifest.baseline` is absent, write `result.json` unchanged.
4. If present:
   - resolve the baseline path relative to the scenario file's parent
     directory unless it is absolute;
   - preserve the manifest-authored path string for `baselineDiff.path`; only
     the load path is resolved;
   - load the baseline with the existing result loader;
   - compute `ResultDiff::from_results(&baseline, &current)`;
   - attach `BaselineDiff { path, diff }` to the current result;
   - write the augmented `result.json`.

Baseline comparison does not change `outcome`. The run outcome remains driven
by workload failures, assertions, and chaos failures. A missing or invalid
configured baseline is a run error because the scenario explicitly requested
the artifact. This happens after workload execution but before final
`result.json` is written; the process exits with an error and no partial
`result.json` is emitted. Provider cleanup still runs through the existing
runner cleanup path before the error is returned.

## Review Gates

- Slice D1: write this handoff doc and tracker update.
- Review gate D1: spec review for artifact shape, path resolution, and
  deferred CI policy.
- Slice D2: wire baseline loading and embedded diff into the runner.
- Review gate D2: semantics review for outcome behavior, load errors, and
  compare reuse.
- Slice D3: add tests, examples/docs, and verification.
- Final review gate: full Slice D code review plus test/artifact review before
  commit.

Each gate uses two reviewer passes:

- behavior/lifecycle reviewer;
- test/artifact reviewer.

## Test Plan

- Unit tests:
  - no `baseline` omits `baselineDiff`;
  - relative baseline paths resolve from the scenario manifest directory;
  - direct `result.json` and run-directory baselines load through the same
    helper;
  - embedded diff uses `current - baseline`;
  - `baselineDiff.path` preserves the manifest-authored string instead of a
    canonical or absolute load path;
  - unsupported baseline result schema returns the existing schema error;
  - `SimResult` can deserialize older results without `baselineDiff`;
  - augmented `SimResult` files with `baselineDiff` still load through
    `load_result` and remain comparable with `hogsim compare`;
  - `ResultDiff` still serializes to the existing compare JSON shape.
- CLI/runner smoke:
  - create a tiny existing-target scenario fixture with a baseline result and
    assert `baselineDiff.path`, `baselineDiff.diff.schemaVersion`, and one
    known `current - baseline` delta.

## Verification

Run before final review:

- `cargo fmt`
- `cargo test -p groundhog-sim`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
