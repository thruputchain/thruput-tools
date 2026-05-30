# Deferred: bundled wasm-opt

`groundhog build --optimize` currently shells out to the external
`wasm-opt` binary.  We considered replacing that subprocess with the
Rust `wasm-opt` crate, which builds Binaryen and exposes an in-process
`OptimizationOptions` API.

Decision: defer for now.  Bundling `wasm-opt` would introduce Binaryen's
C++ build into the CLI dependency graph.  That is a larger toolchain and
build-time cost than the current repo footprint, which has no
first-party C/C++ sources and no broad C++ build requirement.

Future tracking issue: <https://github.com/groundhogchain/groundhog-rpc/issues/68>

## Current State

- `groundhog build --optimize` still requires `wasm-opt` on PATH.
- The CLI still reports missing `wasm-opt` as optional setup guidance.
- The current `wasm-opt` invocation already uses the Groundhog VM feature
  set:
  - `-mvp`
  - `--enable-bulk-memory`
  - `--enable-sign-ext`
  - `--enable-mutable-globals`
  - `--enable-simd`
  - `-Oz`

## Future Option

If we decide the UX benefit outweighs the C++ build cost, revisit adding
`wasm-opt = "0.116.1"` to the workspace and replacing the shell-out in
`crates/tools/cli/src/build.rs` with `wasm_opt::OptimizationOptions`.

Acceptance criteria for that future work:

- optimized builds pass without `wasm-opt` on PATH
- `CliError::WasmOptNotFound` and related status/init messages are
  removed
- the added native/C++ build dependency is documented clearly
