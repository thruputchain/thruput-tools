use std::{fs, process::Command};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const RELEASE_VERSION: &str = "0.1.25-rc.4";

fn embedded_source_commit() -> Option<&'static str> {
    let commit = option_env!("GIT_COMMIT").filter(|commit| !commit.is_empty());
    if let Ok(required) = std::env::var("GROUNDHOG_SOURCE_COMMIT") {
        assert_eq!(
            commit,
            Some(required.as_str()),
            "the gateway test lane must embed its required source commit"
        );
    }
    commit
}

fn write_manifest(source_commit: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("bundle-manifest.json");
    let mut manifest = json!({
        "schema": "groundhog-bundle-v1",
        "bundle": {"version": RELEASE_VERSION},
        "components": {
            "groundhog-rpc": {
                "active": true,
                "version": env!("CARGO_PKG_VERSION"),
                "sourceCommit": source_commit,
            }
        }
    });
    let mut canonical = Vec::new();
    write_canonical_json(&manifest, &mut canonical);
    canonical.push(b'\n');
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
    manifest["bundle"]["manifestDigest"] = json!(digest);
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    (directory, path, digest)
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Object(object) => {
            output.push(b'{');
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key).unwrap();
                output.push(b':');
                write_canonical_json(value, output);
            }
            output.push(b'}');
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output);
            }
            output.push(b']');
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_writer(output, value).unwrap();
        }
    }
}

#[test]
fn version_flag_reports_validated_release_identity() {
    let Some(source_commit) = embedded_source_commit() else {
        return;
    };
    let (_directory, manifest_path, _digest) = write_manifest(source_commit);
    let output = Command::new(env!("CARGO_BIN_EXE_hog-gateway"))
        .arg("--version")
        .env("THRUPUT_BUNDLE_MANIFEST", manifest_path)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!(
            "hog-gateway {} (commit {}, release {RELEASE_VERSION})",
            env!("CARGO_PKG_VERSION"),
            &source_commit[..8]
        )
    );
}

#[test]
fn version_flag_reports_precise_manifest_mismatch_on_stderr() {
    let (_directory, manifest_path, _digest) = write_manifest("not-this-build");
    let output = Command::new(env!("CARGO_BIN_EXE_hog-gateway"))
        .arg("--version")
        .env("THRUPUT_BUNDLE_MANIFEST", manifest_path)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("bundle manifest mismatch"),
        "stdout should retain the stable summary"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_reason = embedded_source_commit().map_or_else(
        || "binary does not carry a source commit for groundhog-rpc".to_owned(),
        |commit| {
            format!(
                "bundle manifest expects groundhog-rpc source commit not-this-build, binary reports {commit}"
            )
        },
    );
    assert!(
        stderr.contains(&format!(
            "warning: bundle manifest mismatch: {expected_reason}"
        )),
        "missing precise mismatch diagnostic; stderr: {stderr}"
    );
}

#[test]
fn startup_logs_validated_release_identity() {
    let Some(source_commit) = embedded_source_commit() else {
        return;
    };
    let (directory, manifest_path, digest) = write_manifest(source_commit);
    let missing_config = directory.path().join("missing.toml");
    for rust_log in [None, Some("")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hog-gateway"));
        command
            .args(["--config", missing_config.to_str().unwrap()])
            .env("THRUPUT_BUNDLE_MANIFEST", &manifest_path)
            .env_remove("RUST_LOG");
        if let Some(rust_log) = rust_log {
            command.env("RUST_LOG", rust_log);
        }
        let output = command.output().unwrap();

        assert!(!output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stdout.contains("Groundhog release identity")
                || stderr.contains("Groundhog release identity"),
            "gateway output did not contain the startup identity log with RUST_LOG={rust_log:?}; stdout: {stdout}; stderr: {stderr}"
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(combined.contains(env!("CARGO_PKG_VERSION")));
        assert!(combined.contains(RELEASE_VERSION));
        assert!(combined.contains(&digest));
    }
}
