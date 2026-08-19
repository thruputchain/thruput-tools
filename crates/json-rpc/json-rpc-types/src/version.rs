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
            commit_hash: commit_hash.map(Into::into),
        }
    }

    /// Attach the packaging-platform release when the server was built as part of one.
    #[must_use]
    pub fn with_platform_version(mut self, platform_version: Option<impl Into<String>>) -> Self {
        self.platform_version = platform_version.map(Into::into);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::VersionInfo;
    use serde_json::json;

    #[test]
    fn version_info_serializes_crate_and_platform_versions_separately() {
        let version = VersionInfo::new("3.1.0", "0.9.6", Some("abc123"))
            .with_platform_version(Some("0.1.23"));

        assert_eq!(
            serde_json::to_value(version).unwrap(),
            json!({
                "wireFormat": "3.1.0",
                "implementation": "0.9.6",
                "platformVersion": "0.1.23",
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
            "commitHash": null,
        }))
        .unwrap();

        assert_eq!(version.implementation, "0.9.5");
        assert_eq!(version.platform_version, None);
    }
}
