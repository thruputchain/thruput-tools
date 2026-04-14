use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

// Recursively copy a directory, skipping `target/` subtrees.
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("failed to create dir");
    for entry in std::fs::read_dir(src).expect("failed to read dir") {
        let entry = entry.expect("failed to read entry");
        let name = entry.file_name();
        if name == "target" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).expect("failed to copy file");
        }
    }
}

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let wallet_src = PathBuf::from(&manifest_dir).join("dev-wallet");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Rerun if wallet source or config changes.
    println!(
        "cargo:rerun-if-changed={}",
        wallet_src.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        wallet_src.join("Cargo.toml.bundled").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        wallet_src.join(".cargo").display()
    );

    // Copy the dev-wallet source to OUT_DIR so the cross-compile runs
    // outside the parent workspace tree (avoids workspace detection
    // issues).  Also handles published crates where the manifest is
    // shipped as Cargo.toml.bundled.
    let wallet_dir = out_dir.join("dev-wallet");
    copy_dir(&wallet_src, &wallet_dir);

    // Restore Cargo.toml from the bundled copy.  The bundled manifest
    // includes [workspace] to prevent cargo from attaching to any
    // parent workspace discovered via OUT_DIR.
    let manifest = wallet_dir.join("Cargo.toml");
    let bundled = wallet_dir.join("Cargo.toml.bundled");
    std::fs::copy(&bundled, &manifest).expect("failed to restore dev-wallet Cargo.toml");

    // Cross-compile the dev-wallet contract to WASM.
    // Run from the wallet directory so .cargo/config.toml is picked up
    // (it sets critical rustflags like -zstack-size=30000).
    // Clear CARGO_ENCODED_RUSTFLAGS to prevent the parent cargo process's
    // flags from leaking into the WASM build.
    let status = Command::new("cargo")
        .current_dir(&wallet_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .status()
        .expect("failed to run cargo for dev-wallet build");

    assert!(status.success(), "dev-wallet WASM build failed");

    // Copy the WASM output to OUT_DIR so include_bytes! can find it.
    let wasm_src =
        wallet_dir.join("target/wasm32-unknown-unknown/release/groundhog_dev_wallet.wasm");
    let wasm_dst = out_dir.join("dev_wallet.wasm");

    // Run wasm-opt if available to shrink the binary.
    // Feature flags must match the Groundhog VM feature set.
    // Keep in sync with crates/cli/src/build.rs.
    let wasm_opt_result = Command::new("wasm-opt")
        .arg("-mvp")
        .arg("--enable-bulk-memory")
        .arg("--enable-sign-ext")
        .arg("--enable-mutable-globals")
        .arg("--enable-simd")
        .arg("-Oz")
        .arg("-o")
        .arg(&wasm_dst)
        .arg(&wasm_src)
        .status();

    match wasm_opt_result {
        Ok(s) if s.success() => {
            println!("cargo:warning=wasm-opt applied successfully");
        }
        _ => {
            // wasm-opt not available — use unoptimized WASM.
            std::fs::copy(&wasm_src, &wasm_dst).expect("failed to copy dev-wallet WASM");
            println!("cargo:warning=wasm-opt not found, using unoptimized WASM");
        }
    }

    // Run the wasm-meter instrumentation pass to add stack limiting.
    // Required by groundhog-native-exec for contract verification.
    let raw = std::fs::read(&wasm_dst).expect("failed to read dev-wallet WASM for instrumentation");
    let instrumented =
        groundhog_wasm_meter::instrument(&raw, &groundhog_wasm_meter::Config::default())
            .expect("wasm-meter instrumentation failed on dev-wallet WASM");
    std::fs::write(&wasm_dst, &instrumented).expect("failed to write instrumented dev-wallet WASM");

    println!(
        "cargo:warning=Dev wallet WASM: {} bytes (instrumented)",
        instrumented.len()
    );
}
