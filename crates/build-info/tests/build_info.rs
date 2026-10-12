use thruput_build_info::BuildVersion;

#[test]
fn display_keeps_crate_and_legacy_platform_versions_distinct() {
    let version = BuildVersion::new("0.9.6", Some("0.1.23"));

    assert_eq!(version.to_string(), "0.9.6 (platform 0.1.23)");
    assert_eq!(version.crate_version(), "0.9.6");
    assert_eq!(version.platform_version(), Some("0.1.23"));
    assert_eq!(version.source_commit(), None);
}

#[test]
fn display_omits_unavailable_legacy_platform_version() {
    assert_eq!(BuildVersion::new("0.9.6", None).to_string(), "0.9.6");
    assert_eq!(BuildVersion::new("0.9.6", Some("")).to_string(), "0.9.6");
}

#[test]
fn source_commit_is_independent_of_display_version() {
    let version = BuildVersion::new("0.9.6", None).with_source_commit(Some("abc123"));

    assert_eq!(version.source_commit(), Some("abc123"));
    assert_eq!(version.to_string(), "0.9.6");
}
