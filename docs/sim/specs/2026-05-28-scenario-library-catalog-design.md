# Scenario Library Catalog (Phase 3, Slice H) - Design

**Date:** 2026-05-28
**Status:** Implemented.

## Summary

Turn the growing `examples/sim/` directory into a discoverable scenario library.
The executable scenario manifests remain the source of run behavior, but a
checked-in catalog file records scenario purpose, tags, runner tier, provider,
and contract artifact requirements. `hogsim catalog` lists the catalog and can
filter by tag.

This slice is intentionally metadata-only: it does not change scheduling,
workload execution, target lifecycle, or result semantics.

## Scope

### In scope

- Add `examples/sim/catalog.json`.
- Add typed catalog loading/validation in `groundhog-sim`.
- Add `hogsim catalog [--catalog <path>] [--tag <tag>] [--json]`.
- Document the scenario library workflow and the source-first contract artifact
  convention.
- Add unit tests for catalog parsing, duplicate IDs, missing paths, and tag
  filtering.

### Out of scope

- Moving existing scenario manifests into subdirectories.
- Automatically running scenarios from catalog metadata.
- Keeping committed `.rv32a` fixtures for normal smoke/soak scenarios.
- Contract build orchestration beyond the existing smoke script.
- Baseline refresh or result promotion workflows.

## Catalog Shape

```json
{
  "schemaVersion": 1,
  "kind": "groundhog-sim-scenario-catalog",
  "scenarios": [
    {
      "id": "local-fibonacci-view-smoke",
      "path": "local-fibonacci-view-smoke.json",
      "title": "Fibonacci Deploy + View Smoke",
      "summary": "Deploys fibonacci.rv32a then calls compute(10) through groundhog_view.",
      "provider": "local",
      "tier": "smoke",
      "tags": ["local", "contract", "view-call", "smoke"],
      "requiresContracts": true,
      "contracts": [
        {
          "name": "fibonacci",
          "source": "groundhog-examples/fibonacci",
          "artifact": "fibonacci.rv32a"
        }
      ],
      "runner": "scripts/smoke-sim-scenarios.sh"
    }
  ]
}
```

Relative scenario paths resolve from the catalog file's parent directory.
`hogsim catalog --json` emits a filtered array of display entries with the
catalog-relative `path` preserved and an added `resolvedPath` string for the
runnable manifest path. The human output also prints the resolved path.
`contracts[].source` is descriptive metadata, not a path the simulator builds.
Contract entries are still validated for non-empty, unpadded `name`, `source`,
and `artifact` fields so stale metadata fails close to the catalog edit.

## Contract Artifact Policy

Default to source-first contract scenarios:

- Keep reusable contracts in `groundhog-examples` or a future contract fixtures
  workspace.
- Build RV32 artifacts into a temporary target directory in scripts.
- Pass the build output with `hogsim run --contract-artifact-root`.
- Commit `.rv32a` only for a fixture where exact byte identity is the test
  subject.

## Verification

- `cargo fmt`
- `cargo test -p groundhog-sim`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- `cargo run -p groundhog-sim --bin hogsim -- catalog --tag contract`
