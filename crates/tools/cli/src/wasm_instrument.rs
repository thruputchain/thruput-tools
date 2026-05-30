//! WASM instrumentation via `groundhog-wasm-meter`.
//!
//! Adds deterministic stack limiting required by the Groundhog VM
//! for contract verification.

use std::path::Path;

use groundhog_spec_tool::{FINAL_SECTION_NAME, RAW_SECTION_NAME};

use crate::CliError;

/// Maximum WASM table initial size the Groundhog VM accepts.
///
/// Mirrors `MAX_TABLE_ELEMENTS` in the node's contract verifier (hogd
/// `common`).  wasm-meter's own default is looser, so the CLI pins this
/// to the node value to reject locally what the node would reject at
/// deploy time.
const MAX_TABLE_INITIAL_SIZE: u64 = 64;

/// Build the wasm-meter config the Groundhog VM enforces.
///
/// Mirrors the node's contract verifier so a contract that passes the
/// CLI build is one the node will accept.  `reject_floats` and
/// `max_imported_functions` already match wasm-meter's defaults; the
/// table limit and the contract-spec custom sections do not, so they
/// are set explicitly.
fn meter_config() -> groundhog_wasm_meter::Config {
    groundhog_wasm_meter::Config {
        max_table_initial_size: MAX_TABLE_INITIAL_SIZE,
        allowed_custom_sections: vec![
            "name".to_string(),
            FINAL_SECTION_NAME.to_string(),
            RAW_SECTION_NAME.to_string(),
        ],
        ..groundhog_wasm_meter::Config::default()
    }
}

/// Instrument a WASM binary with stack limiting in place.
///
/// Must run after `wasm-opt` optimization (if any) and before
/// deployment.  Runs a verification pass after instrumentation as
/// a defensive check.
pub fn instrument(wasm: &Path) -> Result<(), CliError> {
    let raw = std::fs::read(wasm)?;
    let config = meter_config();
    let instrumented = groundhog_wasm_meter::instrument(&raw, &config)?;
    groundhog_wasm_meter::verify(&instrumented, &config)
        .map_err(|e| CliError::WasmVerifyFailed(e.to_string()))?;
    std::fs::write(wasm, &instrumented)?;
    Ok(())
}
