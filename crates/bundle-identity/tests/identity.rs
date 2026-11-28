use std::{fs, path::PathBuf};

use serde_json::json;
use tempfile::tempdir;

use sha2::{Digest, Sha256};
use thruput_build_info::BuildVersion;
use thruput_bundle_identity::{BundleIdentity, BundleSubject};

const COMMIT: &str = "373bec4b9dd9b99abae4f23b82f4e35df61273d1";

fn write_manifest(version: &str, source_commit: &str) -> (tempfile::TempDir, PathBuf) {
    write_manifest_with_role(version, source_commit, "cli")
}

fn write_manifest_with_role(
    version: &str,
    source_commit: &str,
    asset_role: &str,
) -> (tempfile::TempDir, PathBuf) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("bundle-manifest.json");
    let mut manifest = json!({
        "schema": "groundhog-bundle-v1",
        "bundle": {"version": "0.1.25-rc.4"},
        "components": {
            "groundhog-rpc": {
                "active": true,
                "version": version,
                "sourceCommit": source_commit,
            },
            "hogx": {
                "active": false,
                "version": "0.1.1-test9"
            }
        },
        "clients": {
            "thruput": {
                "assetRole": asset_role,
                "component": "groundhog-rpc",
                "version": version,
                "sourceCommit": source_commit,
            }
        }
    });
    manifest.sort_all_objects();
    let mut canonical = serde_json::to_vec(&manifest).unwrap();
    canonical.push(b'\n');
    manifest["bundle"]["manifestDigest"] = json!(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(&canonical))
    ));
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    (directory, path)
}

fn build() -> BuildVersion {
    BuildVersion::new("0.10.4", None).with_source_commit(Some(COMMIT))
}

fn client_subject() -> BundleSubject {
    BundleSubject::client("thruput", "groundhog-rpc", "cli")
}

#[test]
fn full_matching_manifest_reports_component_commit_and_release() {
    let (_directory, path) = write_manifest("0.10.4", COMMIT);
    let identity = BundleIdentity::from_manifest_path(client_subject(), build(), &path);

    assert_eq!(
        identity.to_string(),
        "0.10.4 (commit 373bec4b, release 0.1.25-rc.4)"
    );
    assert_eq!(identity.release_version(), Some("0.1.25-rc.4"));
    assert!(identity.bundle_manifest_digest().is_some());
    assert_eq!(identity.mismatch_reason(), None);
}

#[test]
fn matching_component_subject_reports_release_without_a_client_entry() {
    let (_directory, path) = write_manifest("0.10.4", COMMIT);
    let identity = BundleIdentity::from_manifest_path(
        BundleSubject::component("groundhog-rpc"),
        build(),
        &path,
    );

    assert_eq!(identity.release_version(), Some("0.1.25-rc.4"));
    assert!(identity.bundle_manifest_digest().is_some());
    assert_eq!(identity.mismatch_reason(), None);
}

#[test]
fn unbundled_identity_reports_component_commit() {
    let identity = BundleIdentity::unbundled(build());

    assert_eq!(identity.to_string(), "0.10.4 (commit 373bec4b)");
    assert_eq!(identity.release_version(), None);
}

#[test]
fn mismatch_diagnostic_escapes_terminal_controls() {
    let (_directory, path) = write_manifest("0.10.4", "wrong\n\u{1b}\u{202e}commit");
    let identity = BundleIdentity::from_manifest_path(client_subject(), build(), &path);

    let diagnostic = identity
        .terminal_safe_mismatch_reason()
        .expect("bug: source commit mismatch should have a diagnostic");
    assert!(diagnostic.contains(r"wrong\n\u{1b}\u{202e}commit"));
    assert!(!diagnostic.contains('\n'));
    assert!(!diagnostic.contains('\u{1b}'));
    assert!(!diagnostic.contains('\u{202e}'));
}

#[test]
fn modified_release_version_with_stale_digest_is_rejected() {
    let (_directory, path) = write_manifest("0.10.4", COMMIT);
    let contents = fs::read_to_string(&path).unwrap();
    fs::write(&path, contents.replace("0.1.25-rc.4", "9.9.9")).unwrap();

    let identity = BundleIdentity::from_manifest_path(client_subject(), build(), &path);

    assert_eq!(identity.release_version(), None);
    assert!(
        identity
            .mismatch_reason()
            .unwrap()
            .contains("digest mismatch")
    );
}

#[test]
fn missing_client_does_not_claim_the_release() {
    let (_directory, path) = write_manifest("0.10.4", COMMIT);
    let identity = BundleIdentity::from_manifest_path(
        BundleSubject::client("other-client", "groundhog-rpc", "cli"),
        build(),
        &path,
    );

    assert_eq!(identity.release_version(), None);
    assert!(
        identity
            .mismatch_reason()
            .unwrap()
            .contains("no client entry")
    );
}

#[test]
fn wrong_client_asset_role_does_not_claim_the_release() {
    let (_directory, path) = write_manifest_with_role("0.10.4", COMMIT, "other");
    let identity = BundleIdentity::from_manifest_path(client_subject(), build(), &path);

    assert_eq!(identity.release_version(), None);
    assert!(
        identity
            .mismatch_reason()
            .unwrap()
            .contains("asset role other, expected cli")
    );
}

#[test]
fn packaging_manifest_schema_is_valid_but_requires_the_thruput_client() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bundle-manifest-v1.json");
    // The stored producer fixture predates the client rename. Its schema and
    // digest remain valid; the new CLI must not select its old client entry.
    let historical = BundleIdentity::from_manifest_path(
        BundleSubject::client("groundhog", "groundhog-rpc", "cli"),
        build(),
        &path,
    );
    assert_eq!(historical.release_version(), Some("0.1.25-rc.3"));
    assert_eq!(historical.mismatch_reason(), None);

    let identity = BundleIdentity::from_manifest_path(client_subject(), build(), &path);
    assert_eq!(identity.release_version(), None);
    assert!(identity.mismatch_reason().is_some());
}
