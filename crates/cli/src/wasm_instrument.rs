//! WASM instrumentation via `groundhog-wasm-meter`.
//!
//! Adds deterministic stack limiting required by the Groundhog VM
//! for contract verification.

use std::path::Path;

use crate::CliError;

/// Instrument a WASM binary with stack limiting in place.
///
/// Must run after `wasm-opt` optimization (if any) and before
/// deployment.
pub fn instrument(wasm: &Path) -> Result<(), CliError> {
    let raw = std::fs::read(wasm)?;
    let instrumented =
        groundhog_wasm_meter::instrument(&raw, &groundhog_wasm_meter::Config::default())?;
    std::fs::write(wasm, &instrumented)?;
    Ok(())
}
