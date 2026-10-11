//! Setup results for interactive and automated `thruput init` invocations.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of the `init --json` data contract.
pub const INIT_JSON_SCHEMA_VERSION: u32 = 1;

/// Setup state after initializing profiles and any authorized signing key.
// These are independent observed facts in the public JSON report.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitReport {
    /// Version of this setup result.
    pub schema_version: u32,
    /// Whether every required setup step is complete; scaffolding is optional.
    pub complete: bool,
    /// Whether cargo is available on PATH.
    pub cargo_available: bool,
    /// Whether the active profile resolves after global profile initialization.
    pub profiles_configured: bool,
    /// Effective key selection, including a selection that needs repair.
    pub selected_key: Option<String>,
    /// Whether this invocation created a signing key.
    pub key_generated: bool,
    /// Required steps that need user action.
    pub missing: Vec<InitRequirement>,
}

/// Required setup steps that remain incomplete.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InitRequirement {
    /// Cargo is not available on PATH.
    Cargo,
    /// The active profile cannot be resolved.
    Profile,
    /// The effective selection does not identify a stored signing key.
    SigningKey,
}
