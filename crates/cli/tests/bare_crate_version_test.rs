//! `crates/thruput` sits outside the workspace, so it can't inherit the
//! workspace version or profiles, and it has its own Cargo.lock. Fail here
//! when a version bump misses it, its profiles differ, or its lockfile drifts
//! from the one CI tests and audits.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn tools_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_toml(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// `(name, version, source)` for every package in a Cargo.lock. Path packages
/// have no source.
fn locked_packages(path: &Path) -> BTreeSet<(String, String, String)> {
    let lock = read_toml(path);
    let field = |package: &toml::Value, key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    lock["package"]
        .as_array()
        .expect("Cargo.lock has a package array")
        .iter()
        .map(|package| {
            (
                field(package, "name"),
                field(package, "version"),
                field(package, "source"),
            )
        })
        .collect()
}

#[test]
fn bare_thruput_crate_tracks_cli_version() {
    let manifest = read_toml(&tools_root().join("crates/thruput/Cargo.toml"));

    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        manifest["package"]["version"].as_str(),
        Some(version),
        "crates/thruput package version"
    );
    assert_eq!(
        manifest["dependencies"]["thruput-cli"]["version"].as_str(),
        Some(format!("={version}").as_str()),
        "crates/thruput pin on thruput-cli"
    );
}

#[test]
fn bare_thruput_crate_release_profile_matches_workspace() {
    let workspace = read_toml(&tools_root().join("Cargo.toml"));
    let bare = read_toml(&tools_root().join("crates/thruput/Cargo.toml"));
    assert_eq!(
        bare.get("profile"),
        workspace.get("profile"),
        "crates/thruput [profile] tables must match the workspace's"
    );
}

#[test]
fn bare_thruput_crate_lockfile_matches_workspace() {
    let workspace = locked_packages(&tools_root().join("Cargo.lock"));
    let drifted: Vec<_> = locked_packages(&tools_root().join("crates/thruput/Cargo.lock"))
        .into_iter()
        .filter(|(name, _, _)| name != "thruput")
        .filter(|package| !workspace.contains(package))
        .collect();
    assert!(
        drifted.is_empty(),
        "crates/thruput/Cargo.lock has packages that the workspace Cargo.lock does not: \
         {drifted:?}. Copy the workspace Cargo.lock to crates/thruput/ and run \
         `cargo update --workspace` there."
    );
}
