# Idempotent Instrumentation and Groundhog-Owned Artifacts

## Problem

`groundhog build` currently treats Cargo's raw WASM output as the working file for all
post-processing. After Cargo writes:

- `<target_dir>/wasm32-unknown-unknown/release/<snake>.wasm`

Groundhog then mutates that same file in place by:

1. running `wasm-opt` when `--optimize` is enabled
2. rewriting the WASM to canonicalize the embedded spec
3. instrumenting the WASM with `groundhog-wasm-meter`

This creates a bad steady state:

- the raw Cargo artifact is no longer raw
- repeated builds can end up reprocessing an already instrumented WASM
- deploy/call resolution is tied to a path that Cargo also owns

The result is non-idempotent behavior and a blurred ownership boundary between Cargo outputs and
Groundhog outputs.

## Goals

- Leave Cargo's raw WASM output untouched after `cargo build`
- Make Groundhog's processed artifact path explicit and stable
- Ensure repeated `groundhog build` invocations always start from fresh raw Cargo output
- Keep `--target-dir` behavior consistent for both raw and processed artifacts
- Make `deploy` and `call` resolve the Groundhog-owned processed artifact by default

## Non-Goals

- Detecting and recovering from already instrumented raw artifacts in place
- Preserving backward compatibility with the old in-place mutated path
- Changing the order of post-processing steps beyond moving them onto a copied artifact

## Chosen Layout

For a package whose artifact stem is `<snake>`:

- `<snake>` is the existing snake-case package artifact stem, e.g. `demo-contract` ->
  `demo_contract`

- Raw Cargo output remains:
  - `<target_dir>/wasm32-unknown-unknown/release/<snake>.wasm`
- Groundhog-owned processed outputs move to:
  - `<target_dir>/groundhog/<snake>.wasm`
  - `<target_dir>/groundhog/<snake>.spec`
  - `<target_dir>/groundhog/<snake>.spec.json`

`<target_dir>` is either Cargo's default target directory or the explicit directory selected by
Groundhog's `--target-dir`.

## Behavioral Design

### Build

`groundhog build` should:

1. Run Cargo as it does today, producing the raw WASM under Cargo's normal target tree.
2. Resolve the raw Cargo artifact path.
3. Create `<target_dir>/groundhog/` if it does not already exist.
4. Copy the raw Cargo artifact to `<target_dir>/groundhog/<snake>.wasm`, overwriting any previous
   processed artifact.
5. Run all Groundhog post-processing against the copied file only:
   - `wasm-opt` when `--optimize` is enabled
   - spec finalization and sidecar generation
   - wasm-meter instrumentation
6. Return the processed Groundhog artifact path in command output and JSON.

The processing order stays the same as today; only the input/output location changes.

### Idempotency

The idempotency rule is simple:

- Every build starts from Cargo's fresh raw artifact.
- No Groundhog post-processing step ever mutates the raw Cargo artifact.

That means a second `groundhog build` is always equivalent to deleting the processed artifact tree
and rebuilding it from scratch.

### Deploy

When `deploy` is resolving a WASM automatically from the current project, it should use:

- `<target_dir>/groundhog/<snake>.wasm`

It should no longer default to the raw Cargo output path.

Explicit `--wasm <path>` continues to bypass lookup entirely.

### Call

When `call` is resolving local full spec or interface sidecars from project artifacts, it should
use the Groundhog-owned processed artifact path:

- `<target_dir>/groundhog/<snake>.wasm`

That also means sidecar resolution comes from:

- `<target_dir>/groundhog/<snake>.spec.json`

This keeps `call` aligned with the actual deployable artifact rather than the raw Cargo output.

### Target Dir Semantics

`--target-dir` continues to select the base target directory once, normalized from the original
invocation cwd as already designed.

Examples:

- default target dir:
  - raw: `target/wasm32-unknown-unknown/release/demo_contract.wasm`
  - processed: `target/groundhog/demo_contract.wasm`
- explicit target dir `/tmp/out`:
  - raw: `/tmp/out/wasm32-unknown-unknown/release/demo_contract.wasm`
  - processed: `/tmp/out/groundhog/demo_contract.wasm`

## Code Shape

The artifact helpers should distinguish between two path families:

- raw Cargo artifact path
- Groundhog processed artifact path

The existing single `wasm_output_path(...)` helper is no longer expressive enough for both roles.
The implementation should introduce explicit helpers so call sites cannot accidentally choose the
wrong ownership boundary.

Expected helper split:

- helper for raw Cargo WASM path under `wasm32-unknown-unknown/release`
- helper for Groundhog processed WASM path under `groundhog/`
- existing sidecar helpers continue to derive from the selected processed WASM path

## Failure Model

- If the raw Cargo artifact is missing, `build` fails as it does today.
- If copying into `target/groundhog/` fails, `build` fails before any post-processing starts.
- If post-processing fails, the raw Cargo artifact remains untouched.
- `deploy` and `call` should not silently fall back to the raw Cargo artifact, because that would
  reintroduce ambiguity about whether the resolved file is instrumented and ready for use.

## Test Coverage

Add coverage for:

- processed artifacts are written under `<target_dir>/groundhog/`
- raw Cargo WASM remains unchanged after build post-processing
- repeated builds succeed when a processed artifact already exists
- `deploy` resolves the Groundhog processed path, not the raw Cargo path
- `call` spec lookup resolves from the Groundhog processed path
- `--target-dir` changes both raw input base and processed output base consistently

## User-Facing Outcome

After this change, Groundhog owns a single canonical processed artifact set under `target/groundhog/`,
and Cargo continues to own the raw compilation output under the normal target tree.

That separates responsibilities cleanly:

- Cargo compiles
- Groundhog post-processes
- repeated builds are idempotent
