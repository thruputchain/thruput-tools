use std::env;
use std::fs;
use std::path::Path;

const SOURCE_COMMIT_ENV: &str = "GROUNDHOG_SOURCE_COMMIT";

fn embed_source_commit(manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed={SOURCE_COMMIT_ENV}");
    if let Some(commit) = env::var_os(SOURCE_COMMIT_ENV) {
        let commit = commit.to_string_lossy();
        assert_valid_commit(&commit, SOURCE_COMMIT_ENV);
        println!("cargo:rustc-env=GIT_COMMIT={commit}");
        return;
    }

    if let Some(commit) = packaged_source_commit(manifest_dir) {
        println!("cargo:rustc-env=GIT_COMMIT={commit}");
    } else {
        // Do not let an ambient variable bypass the explicit/package provenance boundary.
        println!("cargo:rustc-env=GIT_COMMIT=");
    }
}

fn packaged_source_commit(manifest_dir: &Path) -> Option<String> {
    let path = manifest_dir.join(".cargo_vcs_info.json");
    if !path.is_file() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    let contents = fs::read(&path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&contents).ok()?;
    let git = value.get("git")?;
    if git.get("dirty").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    let commit = git.get("sha1")?.as_str()?.to_owned();
    if !is_valid_commit(&commit) {
        return None;
    }
    Some(commit)
}

fn assert_valid_commit(commit: &str, source: &str) {
    assert!(
        is_valid_commit(commit),
        "{source} must be a 40-character lowercase Git commit, got {commit:?}"
    );
}

fn is_valid_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    embed_source_commit(manifest_dir);
}
