//! WASM instrumentation via `groundhog-wasm-meter`.
//!
//! Adds deterministic stack limiting required by the Groundhog VM
//! for contract verification.

use std::path::Path;

use crate::CliError;

/// Instrument a WASM binary with stack limiting in place.
///
/// Must run after `wasm-opt` optimization (if any) and before
/// deployment.  Runs a verification pass after instrumentation as
/// a defensive check.
pub fn instrument(wasm: &Path) -> Result<(), CliError> {
    let raw = std::fs::read(wasm)?;
    let config = groundhog_wasm_meter::Config::default();
    let instrumented = groundhog_wasm_meter::instrument(&raw, &config)?;
    groundhog_wasm_meter::verify(&instrumented, &config)
        .map_err(|e| CliError::WasmVerifyFailed(e.to_string()))?;
    std::fs::write(wasm, &instrumented)?;
    Ok(())
}
