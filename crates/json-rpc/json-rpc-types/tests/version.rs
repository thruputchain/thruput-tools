use serde_json::json;
use thruput_json_rpc_types::version::VersionInfo;

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
