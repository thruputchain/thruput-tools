use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") | Ok("linux") => {
            println!("cargo:rustc-link-arg-bin=groundhog=-Wl,-rpath,{lib_dir}");
        }
        _ => {}
    }
}
