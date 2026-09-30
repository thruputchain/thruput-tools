use groundhog_toolchain::{Catalog, CatalogError, HostTarget};

const PUBLIC_BASE_URL: &str =
    "https://groundhog-public-release-artifacts-017820684264.s3.us-west-2.amazonaws.com";

#[test]
fn embedded_catalog_pins_the_published_default_matrix() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");

    assert_eq!(catalog.default_version(), "0.1.29-rc.4");
    assert_eq!(
        catalog.resolve_version(None).expect("bug: default exists"),
        "0.1.29-rc.4"
    );

    let expected = [
        (
            HostTarget::LinuxX86_64,
            "70c1311c473743400e0d8107433a56a4758c1609f4279a5adb9b77c635fa2106",
            9_719_031,
        ),
        (
            HostTarget::LinuxAarch64,
            "6ef94d911d67439766d229a07b918a17a09330570adb392bd40a7d111617eb08",
            8_897_454,
        ),
        (
            HostTarget::MacosAarch64,
            "b3cfe11f1e4b47346389c0005869c58e9cc1eff363f6ef9ac07ab50c920f867e",
            10_559_961,
        ),
    ];

    for (target, sha256, size) in expected {
        let artifact = catalog
            .artifact("0.1.29-rc.4", target)
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
        catalog.artifact_for_triple("0.1.29-rc.4", "x86_64-apple-darwin"),
        Err(CatalogError::UnsupportedTarget { .. })
    ));
}

#[test]
fn public_url_uses_the_catalog_object_key() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");
    let artifact = catalog
        .artifact("0.1.29-rc.4", HostTarget::LinuxX86_64)
        .expect("bug: default target should exist");

    assert_eq!(
        artifact
            .public_url(PUBLIC_BASE_URL)
            .expect("bug: base URL should be valid")
            .as_str(),
        "https://groundhog-public-release-artifacts-017820684264.s3.us-west-2.amazonaws.com/hog-devnode/0.1.29-rc.4/hog-devnode_0.1.29-rc.4_x86_64-unknown-linux-gnu.tar.gz"
    );
}

#[test]
fn catalog_validation_rejects_a_checksum_change() {
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes())
        .expect("bug: embedded catalog should be JSON");
    value["versions"]["0.1.29-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["sha256"] =
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
    value["versions"]["0.1.29-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["objectKey"] =
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
    value["versions"]["0.1.29-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["files"] =
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
    value["versions"]["0.1.29-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["files"][0]["path"] =
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
        .artifact("0.1.29-rc.4", HostTarget::LinuxX86_64)
        .expect("bug: default target should exist");

    assert!(matches!(
        artifact.public_url("file:///tmp/artifacts"),
        Err(CatalogError::UnsupportedBaseUrlScheme)
    ));
    assert!(matches!(
        artifact.public_url("https://example.test/artifacts?version=wrong"),
        Err(CatalogError::StatefulBaseUrl)
    ));
    assert!(matches!(
        artifact.public_url("https://user:secret@example.test"),
        Err(CatalogError::StatefulBaseUrl)
    ));
}

#[test]
fn catalog_rejects_malformed_runtime_minimums() {
    for minimum in ["", ".", "26.", "2..38", "2.x", "18446744073709551616"] {
        let mut value: serde_json::Value =
            serde_json::from_slice(Catalog::embedded_bytes()).unwrap();
        value["versions"]["0.1.29-rc.4"]["targets"]["x86_64-unknown-linux-gnu"]["compatibility"]
            ["minimumVersion"] = minimum.into();
        assert!(matches!(
            Catalog::parse(&serde_json::to_vec(&value).unwrap()),
            Err(CatalogError::InvalidCompatibility { .. })
        ));
    }
}

#[test]
fn artifact_enforces_its_catalog_runtime_minimum_on_the_current_host() {
    let target = HostTarget::current().unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes()).unwrap();
    value["versions"]["0.1.29-rc.4"]["targets"][target.triple()]["compatibility"]["minimumVersion"] =
        "999999.0".into();
    let catalog = Catalog::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        catalog
            .artifact(catalog.default_version(), target)
            .unwrap()
            .validate_current_host(target),
        Err(CatalogError::IncompatibleRuntime { .. })
    ));
}
