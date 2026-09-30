//! Machine-readable readiness report for the local documentation server.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Schema version for the `groundhog docs --json` readiness payload.
pub const DOCS_JSON_SCHEMA_VERSION: u32 = 1;

/// Documentation server readiness and the identity of the served snapshot.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocsReport {
    /// Version of this readiness payload.
    pub schema_version: u32,
    /// Loopback URL of the listening server.
    pub url: String,
    /// Canonical directory containing the served files.
    pub path: PathBuf,
    /// Source identity of the served documentation.
    pub snapshot: Snapshot,
    /// Version of the running CLI.
    pub cli_version: String,
    /// Whether the included agent guide targets this CLI version.
    pub guide_matches_cli: bool,
}

/// Identity of the site source and agent guide, not a qualified SDK release set.
#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Version of the developer-site snapshot manifest.
    pub schema_version: u32,
    /// Git commit of the developer-site source.
    pub source_revision: String,
    /// Whether the source checkout had uncommitted changes.
    pub source_dirty: bool,
    /// CLI version targeted by the included agent guide.
    pub agent_guide_cli_version: String,
}
