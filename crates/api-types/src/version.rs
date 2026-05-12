use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Runtime version information for a JSON-RPC server implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// Wire-format `SemVer` of the `OpenRPC` spec this node implements.
    pub wire_format: String,
    /// Diagnostic-only implementation version of the binary serving this RPC endpoint.
    /// Consumers must not branch on this field.
    pub implementation: String,
    /// Diagnostic-only Git commit hash this server was built from, when available.
    /// Consumers must not branch on this field.
    pub commit_hash: Option<String>,
}

impl VersionInfo {
    /// Create runtime version information for a JSON-RPC server.
    pub fn new(
        wire_format: impl Into<String>,
        implementation: impl Into<String>,
        commit_hash: Option<impl Into<String>>,
    ) -> Self {
        Self {
            wire_format: wire_format.into(),
            implementation: implementation.into(),
            commit_hash: commit_hash.map(Into::into),
        }
    }
}
