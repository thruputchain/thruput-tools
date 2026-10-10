use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

fn find_groundhog_sys_lib_dir(out_dir: &Path) -> Option<PathBuf> {
    let build_dir = out_dir.ancestors().nth(2)?;
    let mut candidates = Vec::new();

    for entry in fs::read_dir(build_dir).ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path().join("out");
        let Ok(children) = fs::read_dir(&path) else {
            continue;
        };
        for child in children {
            let Ok(child) = child else {
                continue;
            };
            let child_path = child.path();
            let Some(name) = child_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("hogd_lib_") {
                continue;
            }
            let dylib = child_path.join("libtest_ffi.dylib");
            let so = child_path.join("libtest_ffi.so");
            if !dylib.exists() && !so.exists() {
                continue;
            }
            let Ok(modified) = child_path.metadata().and_then(|meta| meta.modified()) else {
                continue;
            };
            candidates.push((modified, child_path));
        }
    }

    candidates.sort_by_key(|(modified, _)| *modified);
    candidates.pop().map(|(_, path)| path)
}

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    embed_source_commit(manifest_dir);

    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_DEV_NODE");
    println!("cargo:rerun-if-env-changed=DEP_GROUNDHOG_TEST_FFI_LIB_DIR");

    if env::var_os("CARGO_FEATURE_DEV_NODE").is_none() {
        return;
    }

    let lib_dir = env::var_os("DEP_GROUNDHOG_TEST_FFI_LIB_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("OUT_DIR")
                .map(PathBuf::from)
                .and_then(|out_dir| find_groundhog_sys_lib_dir(&out_dir))
        });

    let Some(lib_dir) = lib_dir else {
        return;
    };

    let Some(lib_dir) = lib_dir.to_str() else {
        return;
    };

    if let Ok("macos" | "linux") = env::var("CARGO_CFG_TARGET_OS").as_deref() {
        println!("cargo:rustc-link-arg-bin=thruput=-Wl,-rpath,{lib_dir}");
    }
}
