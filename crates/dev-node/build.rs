use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let wallet_dir = PathBuf::from(&manifest_dir).join("../dev-wallet");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Rerun if wallet source changes.
    println!("cargo:rerun-if-changed={}", wallet_dir.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        wallet_dir.join("Cargo.toml").display()
    );

    // Cross-compile the dev-wallet contract to WASM.
    let status = Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--manifest-path",
        ])
        .arg(wallet_dir.join("Cargo.toml"))
        .status()
        .expect("failed to run cargo for dev-wallet build");

    if !status.success() {
        panic!("dev-wallet WASM build failed");
    }

    // Copy the WASM output to OUT_DIR so include_bytes! can find it.
    let wasm_src = wallet_dir.join("target/wasm32-unknown-unknown/release/groundhog_dev_wallet.wasm");
    let wasm_dst = out_dir.join("dev_wallet.wasm");

    if !wasm_src.exists() {
        panic!(
            "dev-wallet WASM not found at {}",
            wasm_src.display()
        );
    }

    std::fs::copy(&wasm_src, &wasm_dst).expect("failed to copy dev-wallet WASM");

    println!(
        "cargo:warning=Dev wallet WASM: {} bytes",
        std::fs::metadata(&wasm_dst).unwrap().len()
    );
}
