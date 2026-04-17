# `--target-dir` Support For Artifact-Aware Groundhog CLI Commands

Date: 2026-04-17

## Summary

Add a first-class `--target-dir <path>` option to every Groundhog CLI
command that produces or auto-discovers build artifacts:

- `groundhog build`
- `groundhog test`
- `groundhog deploy`
- `groundhog call`

The flag should match Cargo's `--target-dir` semantics from the user's
perspective while also keeping Groundhog's own artifact lookup logic
aligned with Cargo's resolved target directory.

## Problem

Today the CLI resolves artifact locations from `cargo metadata` and
then constructs paths under `metadata.target_directory`. That works when
Cargo uses its default target directory or ambient Cargo configuration,
but there is no explicit Groundhog flag to override the build directory
per command.

That gap creates two problems:

1. Users cannot direct build output to a non-default location in a way
   that matches normal Cargo workflows.
2. Even if Groundhog forwarded an override only to `cargo build`,
   commands like `deploy` and `call` would still need to know where
   artifacts actually live when they auto-detect the `.wasm` file or
   local contract spec sidecars.

## Goals

- Add a user-facing `--target-dir` flag that behaves predictably across
  all artifact-aware commands.
- Keep Cargo subprocesses and Groundhog's artifact lookup logic in sync.
- Preserve current behavior when `--target-dir` is not provided.
- Prevent ambiguous command lines where the user passes competing target
  directory overrides through Cargo passthrough arguments.

## Non-Goals

- Changing the artifact layout under the target directory.
- Adding a persistent Groundhog-specific target-dir setting to
  `groundhog.toml`.
- Detecting every possible indirect Cargo config interaction beyond the
  direct conflicting forms users are likely to type in passthrough args.

## User-Facing Behavior

### Command Surface

Add `--target-dir <path>` to:

- `build`
- `test`
- `deploy`
- `call`

### Semantics

- `--target-dir` means "use this Cargo target directory for artifact
  production and lookup."
- If the flag is present, it is the authoritative override for the
  Groundhog command being run.
- If the flag is absent, current Cargo behavior remains unchanged,
  including ambient `CARGO_TARGET_DIR`, workspace config, and Cargo
  config files.

### Command-Specific Rules

#### `build`

- Use `--target-dir` for `cargo metadata`, `cargo build`, and all
  resulting artifact path resolution.
- Continue returning the resolved `.wasm` path in JSON output.

#### `test`

- Use `--target-dir` for the initial WASM build and the follow-up host
  `cargo test` invocation.
- Return the resolved `.wasm` path in JSON output as today.

#### `deploy`

- If `--wasm <path>` is provided, Groundhog uses that explicit file path
  and ignores `--target-dir` for artifact lookup.
- Otherwise, auto-detected `.wasm` resolution uses the overridden target
  directory.

#### `call`

- Use `--target-dir` anywhere local build-derived metadata is resolved,
  especially local `.wasm` and contract spec sidecar lookup.
- RPC submission and remote chain interaction remain unchanged.

## Cargo Precedence Model

Groundhog should follow Cargo's precedence behavior:

- Cargo CLI `--target-dir` overrides `CARGO_TARGET_DIR`.
- `CARGO_TARGET_DIR` overrides Cargo config values such as
  `build.target-dir`.

Groundhog will expose `--target-dir` as a top-level CLI flag, then
apply it to Cargo subprocesses via `CARGO_TARGET_DIR`. This preserves a
single Groundhog-level override without rewriting every Cargo command to
inject Cargo's own `--target-dir` flag.

Within a Groundhog command:

- explicit Groundhog `--target-dir` wins over ambient environment or
  config
- no error is raised for ambient env/config differences
- conflicts are raised only for competing user-supplied Cargo
  passthrough arguments

## Internal Design

### Shared Artifact Options

Introduce a small shared artifact options type, for example:

- `ArtifactOptions { target_dir: Option<PathBuf> }`

Each artifact-aware subcommand accepts and forwards this shared option
instead of handling raw target-dir strings independently.

### Shared Cargo Application

Add a helper that applies artifact options to Cargo subprocesses:

- if `target_dir` is present, set `CARGO_TARGET_DIR` on the child
  process
- otherwise, leave the process environment unchanged

This helper must be used for:

- `cargo metadata`
- `cargo build`
- host `cargo test`

### Shared Metadata Resolution

Change `cargo_metadata()` to accept artifact options and apply the same
target-dir override as the actual Cargo subprocesses. This ensures that:

- metadata reports the same `target_directory` Groundhog will build into
- Groundhog can continue using Cargo metadata as the source of truth for
  the resolved artifact root

### Shared Artifact Path Helper

Add a single helper for deriving the expected WASM output path from:

- resolved target directory
- package name

It should centralize the current repeated logic that joins:

- `wasm32-unknown-unknown/release/<snake_name>.wasm`

Use this helper in:

- `build`
- `deploy` auto-detection
- `call` local artifact/spec resolution
- any tests that assert artifact paths

## Conflict Detection

Conflict detection applies only when Groundhog's top-level
`--target-dir` is set and the same command also accepts Cargo
passthrough args, namely `build` and `test`.

Groundhog should reject these direct conflicting forms in passthrough
args:

- `--target-dir <dir>`
- `--target-dir=<dir>`
- `--config build.target-dir=...`
- `--config=build.target-dir=...`
- `--config <path>`
- `--config=<path>`

For `--config <path>` and `--config=<path>`, Groundhog should inspect
the referenced TOML file and treat it as conflicting when it sets
`[build].target-dir`.

Error behavior:

- return a dedicated CLI error such as `TargetDirConflict`
- explain that `--target-dir` was provided both to Groundhog and to
  Cargo passthrough args
- instruct the user to use only the top-level Groundhog flag

Groundhog does not need to detect every possible transitive config
source. The robust requirement is to catch the common direct forms that
would create an ambiguous command line.

## Implementation Plan Shape

Expected implementation steps:

1. Add `--target-dir` to the clap definitions for the four affected
   commands.
2. Introduce shared artifact options and child-process application
   helpers.
3. Update `cargo_metadata()` to accept artifact options.
4. Add a shared artifact-path helper and switch existing callers to it.
5. Thread artifact options through `build`, `test`, `deploy`, and
   `call`.
6. Add passthrough conflict detection for `build` and `test`.
7. Update help text and JSON tests where needed.

## Testing

Add or extend tests in the CLI crate to cover:

- clap parsing for `--target-dir` on `build`, `test`, `deploy`, and
  `call`
- shared artifact-path helper behavior
- metadata resolution with explicit target-dir override
- passthrough conflict detection for:
  - `--target-dir <dir>`
  - `--target-dir=<dir>`
  - `--config build.target-dir=...`
  - `--config=build.target-dir=...`
  - `--config <path>` with `[build].target-dir`
  - `--config=<path>` with `[build].target-dir`
- `deploy` helper behavior when explicit `--wasm` is present versus
  auto-detect mode
- `call` local spec lookup behavior when the resolved target directory
  changes

Full end-to-end integration coverage is not required for all build/test
paths because the existing repo constraints around registry-backed
builds still apply.

## Backward Compatibility

This change is backward-compatible:

- existing users without `--target-dir` keep current behavior
- users relying on ambient `CARGO_TARGET_DIR` or Cargo config continue
  to work
- users gain a clearer explicit override that applies consistently to
  both Cargo subprocesses and Groundhog artifact lookup

## Risks

- Missing one artifact lookup call site would produce confusing
  "artifact not found" behavior after a successful build into a custom
  target directory.
- Overly narrow conflict detection would allow ambiguous command lines.
- Overly broad conflict detection would reject harmless Cargo args.

The mitigation is to centralize artifact path computation and keep
conflict detection scoped to the direct override forms described above.
