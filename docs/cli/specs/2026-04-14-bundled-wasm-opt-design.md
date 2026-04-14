# Bundled wasm-opt

Replace the CLI callout to the external `wasm-opt` binary with the
`wasm-opt` Rust crate (v0.116.1), which bundles the Binaryen C++
library and exposes a Rust API.

## Motivation

The CLI currently shells out to `wasm-opt` for WASM optimization.  This
requires users to install Binaryen externally (`brew install binaryen`),
adds a class of "tool not found" errors, and means the CLI can't
guarantee consistent optimization settings across environments.

Additionally, the current CLI invocation uses bare `-Oz` without
constraining WASM features.  The hogd `built_contracts` build uses
`-mvp --enable-bulk-memory --enable-sign-ext --enable-mutable-globals`,
which is the correct conservative baseline for the Groundhog VM.  This
change aligns the CLI's settings and adds `--enable-simd` since the
wasmtime runtime supports it (`relaxed_simd_deterministic(true)` in
`hogd/rust/wasmtime/src/global_context.rs`).

## WASM Feature Set

Start from MVP and enable only features the Groundhog VM supports:

| Feature         | Enabled | Source of truth                        |
|-----------------|---------|----------------------------------------|
| bulk-memory     | yes     | hogd/built_contracts/CMakeLists.txt    |
| sign-ext        | yes     | hogd/built_contracts/CMakeLists.txt    |
| mutable-globals | yes     | hogd/built_contracts/CMakeLists.txt    |
| simd            | yes     | hogd wasmtime config (relaxed_simd_deterministic) |
| threads         | no      | not confirmed in VM                    |
| tail-call       | no      | not confirmed in VM                    |
| reference-types | no      | not confirmed in VM                    |
| multi-memory    | no      | explicitly disabled in wasmtime config |

## Callsites

### 1. CLI build command (`crates/cli/src/build.rs`)

**Current:** Lines 68-69 check `which::which("wasm-opt")`, lines
102-113 shell out to `wasm-opt -Oz`.

**New:** Replace with `wasm_opt::OptimizationOptions` API call using the
feature set above.  Remove the `which` pre-check.  Add a helper
function in the CLI crate that constructs the configured
`OptimizationOptions` so the feature set is defined in one place.

```rust
use wasm_opt::{Feature, OptimizationOptions};

/// Build wasm-opt options matching the Groundhog VM feature set.
///
/// Starts from MVP and enables only the features the VM supports.
/// Keep in sync with `crates/dev-node/build.rs`.
pub fn groundhog_wasm_opt() -> OptimizationOptions {
    let mut opts = OptimizationOptions::new_optimize_for_size_aggressively();
    opts.mvp_features_only()
        .enable_feature(Feature::BulkMemory)
        .enable_feature(Feature::SignExt)
        .enable_feature(Feature::MutableGlobals)
        .enable_feature(Feature::Simd);
    opts
}
```

### 2. Dev-node build script (`crates/dev-node/build.rs`)

**Current:** Lines 51-66 try `Command::new("wasm-opt")`, fall back to
copying the unoptimized WASM if not found.

**New:** Add `wasm-opt` as a `[build-dependency]` of `dev-node`.  Use
the same feature configuration.  Remove the fallback path since the
optimizer is always available.  Duplicate the ~5 lines of feature
configuration with a comment pointing to the canonical location in the
CLI crate (build scripts can't import workspace lib crates).

## Dependency Changes

### Workspace `Cargo.toml`

```toml
wasm-opt = "0.116.1"
```

### `crates/cli/Cargo.toml`

```toml
wasm-opt = { workspace = true }
```

Remove `which` dependency (only used for wasm-opt check; confirm no
other uses first).

### `crates/dev-node/Cargo.toml`

```toml
[build-dependencies]
wasm-opt = { workspace = true }
```

## Code Removals

### Error handling (`crates/cli/src/error.rs`)

- Remove `CliError::WasmOptNotFound` variant and its `code()` /
  `hint()` arms.
- Update `CliError::WasmOptFailed` to wrap the `wasm_opt`
  crate's `OptimizationError` instead of being a bare unit variant.

### Environment checks (`crates/cli/src/checks.rs`)

- Remove `wasm_opt: bool` field from `EnvironmentStatus`.
- Remove the `which::which("wasm-opt")` call in `check_environment()`.
- Remove the `no_wasm_opt` finding from `collect_report()`.

### Init wizard (`crates/cli/src/init.rs`)

- Remove the "wasm-opt not installed" messages from `report_ready()`
  and `handle_environment_step()`.

### Status display (`crates/cli/src/status.rs`)

- Remove the "wasm-opt: installed / not installed" line.

### `which` dependency

Keep `which` — it is also used for `cargo` detection (`lib.rs`,
`checks.rs`) and `groundhog-spec-tool` (`contract_spec.rs`).  Only
remove the `which::which("wasm-opt")` call in `build.rs` and
`checks.rs`.

## Build Time Impact

Cold build of the `wasm-opt` crate: ~1m 36s (one-time, cached
thereafter).  Incremental rebuilds: instant.  Pre-built binaries
(homebrew) are unaffected.

## Testing

- Existing `build_test.rs` integration tests exercise the optimize path.
  They should continue to pass without needing Binaryen installed.
- The `EnvironmentStatus` struct changes will require updating test
  fixtures in `checks.rs` that construct it (remove `wasm_opt` field).
- Dev-node build script change is verified by `cargo build -p
  groundhog-dev-node --features dev-node` succeeding.
