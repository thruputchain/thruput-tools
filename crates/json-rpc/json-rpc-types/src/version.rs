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
    /// Platform release this binary was packaged for, when available.
    /// Consumers must not branch on this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_version: Option<String>,
    /// Release containing this binary, after validation against its installed bundle manifest.
    /// Consumers must not branch on this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    /// Digest of the validated installed bundle manifest, when release identity is available.
    /// Consumers must not branch on this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_manifest_digest: Option<String>,
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
            platform_version: None,
            release_version: None,
            bundle_manifest_digest: None,
            commit_hash: commit_hash.map(Into::into),
        }
    }

    /// Attach the packaging-platform release when the server was built as part of one.
    #[must_use]
    pub fn with_platform_version(mut self, platform_version: Option<impl Into<String>>) -> Self {
        self.platform_version = platform_version.map(Into::into);
        self
    }

    /// Attach identity from a validated installed bundle manifest.
    #[must_use]
    pub fn with_release_identity(
        mut self,
        release_version: Option<String>,
        bundle_manifest_digest: Option<String>,
    ) -> Self {
        self.release_version = release_version;
        self.bundle_manifest_digest = bundle_manifest_digest;
        self
    }
}
