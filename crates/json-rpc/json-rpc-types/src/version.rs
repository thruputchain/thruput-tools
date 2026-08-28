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

#[cfg(test)]
mod tests {
    use super::VersionInfo;
    use serde_json::json;

    #[test]
    fn version_info_serializes_component_and_release_identity_separately() {
        let version = VersionInfo::new("3.1.0", "0.9.6", Some("abc123")).with_release_identity(
            Some("0.1.25-rc.4".to_owned()),
            Some("sha256:test-manifest".to_owned()),
        );

        assert_eq!(
            serde_json::to_value(version).unwrap(),
            json!({
                "wireFormat": "3.1.0",
                "implementation": "0.9.6",
                "releaseVersion": "0.1.25-rc.4",
                "bundleManifestDigest": "sha256:test-manifest",
                "commitHash": "abc123",
            })
        );
    }

    #[test]
    fn version_info_omits_unavailable_platform_version() {
        let version = VersionInfo::new("3.1.0", "0.9.6", None::<String>);
        let value = serde_json::to_value(version).unwrap();

        assert_eq!(value["implementation"], "0.9.6");
        assert!(value.get("platformVersion").is_none());
    }

    #[test]
    fn version_info_deserializes_the_legacy_shape() {
        let version: VersionInfo = serde_json::from_value(json!({
            "wireFormat": "3.0.0",
            "implementation": "0.9.5",
            "platformVersion": "0.1.23",
            "commitHash": null,
        }))
        .unwrap();

        assert_eq!(version.implementation, "0.9.5");
        assert_eq!(version.platform_version.as_deref(), Some("0.1.23"));
        assert_eq!(version.release_version, None);
        assert_eq!(version.bundle_manifest_digest, None);
    }
}
