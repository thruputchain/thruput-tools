use groundhog_toolchain::{Catalog, CatalogError, HostTarget};

const PUBLIC_BASE_URL: &str =
    "https://groundhog-public-release-artifacts-017820684264.s3.us-west-2.amazonaws.com";

#[test]
fn embedded_catalog_pins_the_published_default_matrix() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");

    assert_eq!(catalog.default_version(), "0.1.27-rc.4");
    assert_eq!(
        catalog.resolve_version(None).expect("bug: default exists"),
        "0.1.27-rc.4"
    );

    let expected = [
        (
            HostTarget::LinuxX86_64,
            "2abce546ab2b48eae7b49ecf4ae3dfc76be3c781d21a95fc6682e7d93b7e953a",
            9_525_453,
        ),
        (
            HostTarget::LinuxAarch64,
            "37adc18ab8fe31ceb8222c45b2006ae9974ca3584da49e9b6d71513fc4d0f7e1",
            8_719_976,
        ),
        (
            HostTarget::MacosAarch64,
            "6b62b8233591e3f54fefb4383b4aa31c1b8172636d56e9c2c214fecb6744f146",
            10_362_465,
        ),
    ];

    for (target, sha256, size) in expected {
        let artifact = catalog
            .artifact("0.1.27-rc.4", target)
            .expect("bug: default target should exist");
        assert_eq!(artifact.sha256(), sha256);
        assert_eq!(artifact.size(), size);
        assert!(artifact.has_required_bundle_files());
    }
}

#[test]
fn target_detection_accepts_only_the_managed_v1_matrix() {
    assert_eq!(
        HostTarget::from_os_arch("linux", "x86_64").expect("bug: supported target"),
        HostTarget::LinuxX86_64
    );
    assert_eq!(
        HostTarget::from_os_arch("linux", "aarch64").expect("bug: supported target"),
        HostTarget::LinuxAarch64
    );
    assert_eq!(
        HostTarget::from_os_arch("macos", "aarch64").expect("bug: supported target"),
        HostTarget::MacosAarch64
    );
    assert!(matches!(
        HostTarget::from_os_arch("macos", "x86_64"),
        Err(CatalogError::UnsupportedHost { .. })
    ));
}

#[test]
fn catalog_rejects_unknown_versions_and_targets() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");

    assert!(matches!(
        catalog.resolve_version(Some("0.1.12")),
        Err(CatalogError::UnknownVersion { .. })
    ));
    assert!(matches!(
        catalog.artifact_for_triple("0.1.27-rc.4", "x86_64-apple-darwin"),
        Err(CatalogError::UnsupportedTarget { .. })
    ));
}

#[test]
fn public_url_uses_the_catalog_object_key() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");
    let artifact = catalog
        .artifact("0.1.27-rc.4", HostTarget::LinuxX86_64)
        .expect("bug: default target should exist");

    assert_eq!(
        artifact
            .public_url(PUBLIC_BASE_URL)
            .expect("bug: base URL should be valid")
            .as_str(),
        "https://groundhog-public-release-artifacts-017820684264.s3.us-west-2.amazonaws.com/hog-devnode/0.1.27-rc.4/hog-devnode_0.1.27-rc.4_x86_64-unknown-linux-gnu.tar.gz"
    );
}

#[test]
fn catalog_validation_rejects_a_checksum_change() {
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes())
        .expect("bug: embedded catalog should be JSON");
    value["versions"]["0.1.27-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["sha256"] =
        serde_json::Value::String("not-a-sha256".to_string());
    let bytes = serde_json::to_vec(&value).expect("bug: test JSON should serialize");

    assert!(matches!(
        Catalog::parse(&bytes),
        Err(CatalogError::InvalidSha256 { .. })
    ));
}

#[test]
fn catalog_validation_rejects_an_object_key_change() {
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes())
        .expect("bug: embedded catalog should be JSON");
    value["versions"]["0.1.27-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["objectKey"] =
        serde_json::Value::String("somewhere-else/archive.tar.gz".to_string());
    let bytes = serde_json::to_vec(&value).expect("bug: test JSON should serialize");

    assert!(matches!(
        Catalog::parse(&bytes),
        Err(CatalogError::ObjectKeyMismatch { .. })
    ));
}

#[test]
fn catalog_validation_rejects_a_missing_required_file() {
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes())
        .expect("bug: embedded catalog should be JSON");
    value["versions"]["0.1.27-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["files"] =
        serde_json::json!([{"kind": "executable", "path": "hog-devnode"}]);
    let bytes = serde_json::to_vec(&value).expect("bug: test JSON should serialize");

    assert!(matches!(
        Catalog::parse(&bytes),
        Err(CatalogError::InvalidFileSet { .. })
    ));
}

#[test]
fn catalog_validation_rejects_non_normalized_paths() {
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes())
        .expect("bug: embedded catalog should be JSON");
    value["versions"]["0.1.27-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["files"][0]["path"] =
        serde_json::Value::String("contracts//dev_wallet.rv32a".to_string());
    let bytes = serde_json::to_vec(&value).expect("bug: test JSON should serialize");

    assert!(matches!(
        Catalog::parse(&bytes),
        Err(CatalogError::InvalidFileSet { .. })
    ));
}

#[test]
fn public_url_rejects_stateful_or_non_http_sources() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");
    let artifact = catalog
        .artifact("0.1.27-rc.4", HostTarget::LinuxX86_64)
        .expect("bug: default target should exist");

    assert!(matches!(
        artifact.public_url("file:///tmp/artifacts"),
        Err(CatalogError::UnsupportedBaseUrlScheme { .. })
    ));
    assert!(matches!(
        artifact.public_url("https://example.test/artifacts?version=wrong"),
        Err(CatalogError::StatefulBaseUrl { .. })
    ));
    assert!(matches!(
        artifact.public_url("https://user:secret@example.test"),
        Err(CatalogError::StatefulBaseUrl { .. })
    ));
}
