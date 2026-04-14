# Bundled wasm-opt Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the external `wasm-opt` CLI dependency with the bundled `wasm-opt` Rust crate, align WASM feature flags with the Groundhog VM, and remove all "wasm-opt not found" UX surface area.

**Architecture:** Add `wasm-opt = "0.116.1"` as a workspace dependency. A shared helper in the CLI crate constructs `OptimizationOptions` with the correct Groundhog VM feature set (MVP + bulk-memory + sign-ext + mutable-globals + SIMD, `-Oz`). The dev-node build script duplicates the feature config since build scripts can't import workspace lib crates. All `which::which("wasm-opt")` checks and related error variants / UI messages are removed.

**Tech Stack:** `wasm-opt` crate (Binaryen C++ bindings via cxx), Rust edition 2024

**Spec:** `docs/cli/specs/2026-04-14-bundled-wasm-opt-design.md`

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `Cargo.toml` (workspace root) | Add `wasm-opt` workspace dependency |
| Modify | `crates/cli/Cargo.toml` | Add `wasm-opt` dependency |
| Modify | `crates/dev-node/Cargo.toml` | Add `wasm-opt` build-dependency |
| Create | `crates/cli/src/wasm_opt.rs` | Shared helper: `groundhog_wasm_opt()` returning configured `OptimizationOptions` |
| Modify | `crates/cli/src/lib.rs` | Add `pub mod wasm_opt;` |
| Modify | `crates/cli/src/build.rs` | Replace shell-out with API call |
| Modify | `crates/cli/src/error.rs` | Remove `WasmOptNotFound`, update `WasmOptFailed` to wrap `OptimizationError` |
| Modify | `crates/cli/src/main.rs` | Update `--optimize` help text |
| Modify | `crates/cli/src/checks.rs` | Remove `wasm_opt` field and check |
| Modify | `crates/cli/src/init.rs` | Remove wasm-opt messaging |
| Modify | `crates/cli/src/status.rs` | Remove wasm-opt display line |
| Modify | `crates/dev-node/build.rs` | Replace shell-out with API call |

---

### Task 1: Add workspace dependency and wire up Cargo.toml files

**Files:**
- Modify: `Cargo.toml:26-80` (workspace dependencies)
- Modify: `crates/cli/Cargo.toml:23-46` (dependencies)
- Modify: `crates/dev-node/Cargo.toml` (add build-dependencies section)

- [ ] **Step 1: Add `wasm-opt` to workspace dependencies**

In `Cargo.toml`, add to the `[workspace.dependencies]` section after the `# --- Error handling ---` block:

```toml
# --- WASM optimization ---
wasm-opt = "0.116.1"
```

- [ ] **Step 2: Add `wasm-opt` to CLI crate dependencies**

In `crates/cli/Cargo.toml`, add to `[dependencies]`:

```toml
wasm-opt = { workspace = true }
```

- [ ] **Step 3: Add `wasm-opt` as build-dependency for dev-node**

In `crates/dev-node/Cargo.toml`, add a new section at the end of the file:

```toml
[build-dependencies]
wasm-opt = { workspace = true }
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo check --workspace`
Expected: compiles successfully (first run will take ~1.5 min for the C++ build)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cli/Cargo.toml crates/dev-node/Cargo.toml
git commit -m "deps: add wasm-opt crate as workspace dependency

Adds the wasm-opt Rust crate which bundles the Binaryen C++ optimizer.
This will replace the external wasm-opt CLI dependency."
```

---

### Task 2: Create the shared wasm-opt helper in the CLI crate

**Files:**
- Create: `crates/cli/src/wasm_opt.rs`
- Modify: `crates/cli/src/lib.rs:1-23`

- [ ] **Step 1: Create `crates/cli/src/wasm_opt.rs`**

```rust
//! WASM optimization via the bundled Binaryen library.
//!
//! The feature set here must match what the Groundhog VM supports.
//! Source of truth: hogd wasmtime config and hogd/built_contracts
//! CMakeLists.txt.  Keep the dev-node build script
//! (`crates/dev-node/build.rs`) in sync when changing these flags.

use std::path::Path;

use wasm_opt::{Feature, OptimizationOptions};

use crate::CliError;

/// Build wasm-opt options matching the Groundhog VM feature set.
///
/// Starts from MVP and enables only the features the VM supports.
/// Optimizes aggressively for code size (`-Oz`).
pub fn groundhog_wasm_opt() -> OptimizationOptions {
    let mut opts = OptimizationOptions::new_optimize_for_size_aggressively();
    opts.mvp_features_only()
        .enable_feature(Feature::BulkMemory)
        .enable_feature(Feature::SignExt)
        .enable_feature(Feature::MutableGlobals)
        .enable_feature(Feature::Simd);
    opts
}

/// Optimize a WASM binary in place.
pub fn optimize(wasm: &Path) -> Result<(), CliError> {
    let opts = groundhog_wasm_opt();
    opts.run(wasm, wasm).map_err(CliError::WasmOptFailed)
}
```

- [ ] **Step 2: Register the module in `crates/cli/src/lib.rs`**

Add `pub mod wasm_opt;` after `pub mod tx_utils;` (line 21):

```rust
pub mod tx_utils;
pub mod wasm_opt;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p groundhog-cli`
Expected: compiles (the `CliError::WasmOptFailed` variant doesn't exist yet with the right type — this will fail until Task 3)

Actually, this step depends on the error variant update. Skip verification here — it will be verified in Task 3.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/wasm_opt.rs crates/cli/src/lib.rs
git commit -m "feat: add bundled wasm-opt helper with Groundhog VM feature set

Configures wasm-opt with MVP baseline plus bulk-memory, sign-ext,
mutable-globals, and SIMD — matching the Groundhog VM capabilities."
```

---

### Task 3: Update error variants

**Files:**
- Modify: `crates/cli/src/error.rs:1-249`

- [ ] **Step 1: Update the `WasmOptFailed` variant and remove `WasmOptNotFound`**

In `crates/cli/src/error.rs`, remove the `WasmOptNotFound` variant (line 10):

```rust
    #[error("wasm-opt not found on PATH")]
    WasmOptNotFound,
```

Replace the `WasmOptFailed` variant (line 49):

```rust
    #[error("wasm-opt optimization failed")]
    WasmOptFailed,
```

with:

```rust
    #[error("wasm-opt optimization failed: {0}")]
    WasmOptFailed(#[from] wasm_opt::OptimizationError),
```

- [ ] **Step 2: Remove `WasmOptNotFound` from `code()` match**

Remove this arm (line 96):

```rust
            Self::WasmOptNotFound => "wasm_opt_not_found",
```

- [ ] **Step 3: Remove `WasmOptNotFound` from `hint()` match**

Remove this arm (lines 138-141):

```rust
            Self::WasmOptNotFound => Some(Cow::Borrowed(
                "install Binaryen (`brew install binaryen`), or \
                 rerun without `--optimize`",
            )),
```

- [ ] **Step 4: Update `WasmOptFailed` in `hint()` match**

Change (lines 215-217):

```rust
            Self::WasmOptFailed => Some(Cow::Borrowed(
                "check the wasm-opt output above; the WASM binary \
                 may be invalid",
            )),
```

to:

```rust
            Self::WasmOptFailed(_) => Some(Cow::Borrowed(
                "the WASM binary may be invalid or use \
                 unsupported features",
            )),
```

- [ ] **Step 5: Update `WasmOptFailed` in `code()` match**

The existing arm (line 117):

```rust
            Self::WasmOptFailed => "wasm_opt_failed",
```

becomes:

```rust
            Self::WasmOptFailed(_) => "wasm_opt_failed",
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo check -p groundhog-cli`
Expected: compiles successfully

- [ ] **Step 7: Commit**

```bash
git add crates/cli/src/error.rs
git commit -m "refactor: remove WasmOptNotFound, wrap OptimizationError in WasmOptFailed

wasm-opt is now bundled so 'not found' is no longer possible.
WasmOptFailed now wraps the crate's typed error."
```

---

### Task 4: Replace the CLI build callsite

**Files:**
- Modify: `crates/cli/src/build.rs:1-212`
- Modify: `crates/cli/src/main.rs:70-84`

- [ ] **Step 1: Update imports in `build.rs`**

Remove `use std::process::Command;` from the imports (line 6) — but
wait, `Command` is still used for `cargo` on line 74. Keep `Command`.

No import changes needed in `build.rs`.

- [ ] **Step 2: Remove the `which` pre-check in `build_project()`**

Remove lines 68-70:

```rust
    if optimize {
        which::which("wasm-opt").map_err(|_| CliError::WasmOptNotFound)?;
    }
```

- [ ] **Step 3: Replace the `Command::new("wasm-opt")` block**

Replace lines 102-113:

```rust
    if optimize {
        print.status("Optimizing with wasm-opt...");
        let status = Command::new("wasm-opt")
            .arg("-Oz")
            .arg(&wasm)
            .arg("-o")
            .arg(&wasm)
            .status()?;
        if !status.success() {
            return Err(CliError::WasmOptFailed);
        }
    }
```

with:

```rust
    if optimize {
        print.status("Optimizing with wasm-opt...");
        crate::wasm_opt::optimize(&wasm)?;
    }
```

- [ ] **Step 4: Update the `--optimize` help text in `main.rs`**

In `crates/cli/src/main.rs`, change the Build command's `long_about` (lines 72-77):

```rust
    #[command(long_about = "Cross-compile the contract to \
                      wasm32-unknown-unknown via cargo. \
                      Must be run inside a Groundhog \
                      project.\n\n\
                      --optimize requires wasm-opt \
                      (install via `brew install binaryen`).")]
```

to:

```rust
    #[command(long_about = "Cross-compile the contract to \
                      wasm32-unknown-unknown via cargo. \
                      Must be run inside a Groundhog \
                      project.\n\n\
                      --optimize runs the bundled wasm-opt \
                      optimizer for smaller binaries.")]
```

- [ ] **Step 5: Update the `--optimize` arg doc comment in `main.rs`**

Change line 82:

```rust
        /// Run wasm-opt after compilation (requires wasm-opt)
```

to:

```rust
        /// Run wasm-opt after compilation
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo check -p groundhog-cli`
Expected: compiles successfully

- [ ] **Step 7: Commit**

```bash
git add crates/cli/src/build.rs crates/cli/src/main.rs
git commit -m "feat: use bundled wasm-opt API in build command

Replaces the shell-out to the wasm-opt binary with an in-process
call via the wasm-opt crate. Configures MVP + bulk-memory + sign-ext
+ mutable-globals + SIMD to match the Groundhog VM feature set."
```

---

### Task 5: Remove wasm-opt from environment checks and UI

**Files:**
- Modify: `crates/cli/src/checks.rs:44-49` (EnvironmentStatus), `checks.rs:128-131` (check_environment), `checks.rs:300-310` (collect_report)
- Modify: `crates/cli/src/init.rs:58-63` (report_ready), `init.rs:99-101` (handle_environment_step)
- Modify: `crates/cli/src/status.rs:116-120` (render_environment)

- [ ] **Step 1: Remove `wasm_opt` field from `EnvironmentStatus`**

In `crates/cli/src/checks.rs`, remove from the struct (line 48):

```rust
    pub wasm_opt: bool,
```

- [ ] **Step 2: Remove `which::which("wasm-opt")` from `check_environment()`**

In `check_environment()`, remove line 131:

```rust
    let wasm_opt = which::which("wasm-opt").is_ok();
```

And remove the `wasm_opt,` field from the `EnvironmentStatus` return value (around line 143).

- [ ] **Step 3: Remove the `no_wasm_opt` finding from `collect_report()`**

Remove lines 300-310:

```rust
    if !environment.wasm_opt {
        findings.push(Finding {
            level: FindingLevel::Info,
            code: "no_wasm_opt",
            message: "wasm-opt not installed (optional)".into(),
            hint: Some(Cow::Borrowed(
                "install Binaryen for \
                 `groundhog build --optimize` support",
            )),
        });
    }
```

- [ ] **Step 4: Remove wasm-opt message from `init.rs` `report_ready()`**

In `crates/cli/src/init.rs`, remove lines 58-63:

```rust
    if !report.environment.wasm_opt {
        print.status(
            "wasm-opt not installed (optional, needed \
             for `build --optimize`).",
        );
    }
```

- [ ] **Step 5: Remove wasm-opt message from `init.rs` `handle_environment_step()`**

In `crates/cli/src/init.rs`, remove lines 99-101:

```rust
    if !report.environment.wasm_opt {
        print.status("wasm-opt not installed (optional, needed for `build --optimize`).");
    }
```

- [ ] **Step 6: Remove wasm-opt line from `status.rs` `render_environment()`**

In `crates/cli/src/status.rs`, remove lines 116-120:

```rust
    if report.environment.wasm_opt {
        print.status("wasm-opt: installed");
    } else {
        print.status("wasm-opt: not installed (optional)");
    }
```

- [ ] **Step 7: Fix test fixtures in `checks.rs`**

Update every `EnvironmentStatus` construction in the test module to remove the `wasm_opt` field. There are two instances:

In `empty_report_has_no_warnings` (around line 443), remove:

```rust
                wasm_opt: true,
```

In `has_warnings_returns_true_for_warn_finding` (around line 631), remove:

```rust
                wasm_opt: true,
```

- [ ] **Step 8: Verify it compiles and tests pass**

Run: `cargo test -p groundhog-cli`
Expected: all tests pass

- [ ] **Step 9: Commit**

```bash
git add crates/cli/src/checks.rs crates/cli/src/init.rs crates/cli/src/status.rs
git commit -m "cleanup: remove wasm-opt environment checks and UI messages

wasm-opt is now bundled — no need to check for it on PATH or warn
users to install Binaryen."
```

---

### Task 6: Replace wasm-opt in the dev-node build script

**Files:**
- Modify: `crates/dev-node/build.rs:1-72`

- [ ] **Step 1: Replace the wasm-opt shell-out with the API call**

Replace lines 50-66:

```rust
    // Run wasm-opt if available to shrink the binary.
    let wasm_opt_result = Command::new("wasm-opt")
        .args(["-Oz", "-o"])
        .arg(&wasm_dst)
        .arg(&wasm_src)
        .status();

    match wasm_opt_result {
        Ok(s) if s.success() => {
            println!("cargo:warning=wasm-opt applied successfully");
        }
        _ => {
            // wasm-opt not available — use unoptimized WASM.
            std::fs::copy(&wasm_src, &wasm_dst).expect("failed to copy dev-wallet WASM");
            println!("cargo:warning=wasm-opt not found, using unoptimized WASM");
        }
    }
```

with:

```rust
    // Optimize the WASM binary with bundled wasm-opt.
    // Feature flags must match the Groundhog VM feature set.
    // Canonical source: crates/cli/src/wasm_opt.rs — keep in sync.
    let mut opts = wasm_opt::OptimizationOptions::new_optimize_for_size_aggressively();
    opts.mvp_features_only()
        .enable_feature(wasm_opt::Feature::BulkMemory)
        .enable_feature(wasm_opt::Feature::SignExt)
        .enable_feature(wasm_opt::Feature::MutableGlobals)
        .enable_feature(wasm_opt::Feature::Simd);
    opts.run(&wasm_src, &wasm_dst)
        .expect("wasm-opt optimization of dev-wallet failed");
    println!("cargo:warning=wasm-opt applied successfully");
```

- [ ] **Step 2: Verify the dev-node crate builds**

Run: `cargo check -p groundhog-dev-node`
Expected: compiles successfully (the build script runs and optimizes the dev-wallet WASM)

- [ ] **Step 3: Commit**

```bash
git add crates/dev-node/build.rs
git commit -m "feat: use bundled wasm-opt in dev-node build script

Replaces the optional shell-out to wasm-opt with the bundled API.
Optimization is now guaranteed — no fallback to unoptimized WASM."
```

---

### Task 7: Final verification

- [ ] **Step 1: Run the full test suite**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Run format check**

Run: `cargo fmt --check`
Expected: no formatting issues

- [ ] **Step 4: Verify no stale wasm-opt references remain**

Run: `grep -r "which.*wasm.opt\|WasmOptNotFound\|brew install binaryen" crates/`
Expected: no matches

- [ ] **Step 5: Spot-check that the README/roadmap references are acceptable**

`crates/cli/README.md` and `docs/cli/roadmap.md` mention wasm-opt.
These references are informational and don't need updating as part of
this change (the roadmap item about "missing wasm-opt" errors is
effectively resolved; the README may mention optimization workflow).
Flag any that look wrong but don't block on them.
