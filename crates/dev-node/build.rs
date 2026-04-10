use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let wallet_dir = PathBuf::from(&manifest_dir).join("../dev-wallet");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Rerun if wallet source or config changes.
    println!(
        "cargo:rerun-if-changed={}",
        wallet_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        wallet_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        wallet_dir.join(".cargo").display()
    );

    // Cross-compile the dev-wallet contract to WASM.
    // Run from the wallet directory so .cargo/config.toml is picked up
    // (it sets critical rustflags like -zstack-size=30000).
    // Clear CARGO_ENCODED_RUSTFLAGS to prevent the parent cargo process's
    // flags from leaking into the WASM build.
    let status = Command::new("cargo")
        .current_dir(&wallet_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .args([
            "build",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()
        .expect("failed to run cargo for dev-wallet build");

    assert!(status.success(), "dev-wallet WASM build failed");

    // Copy the WASM output to OUT_DIR so include_bytes! can find it.
    let wasm_src =
        wallet_dir.join("target/wasm32-unknown-unknown/release/groundhog_dev_wallet.wasm");
    let wasm_dst = out_dir.join("dev_wallet.wasm");

    assert!(
        wasm_src.exists(),
        "dev-wallet WASM not found at {}",
        wasm_src.display()
    );

    // Run wasm-opt if available to shrink the binary.
    let wasm_opt_result = Command::new("wasm-opt")
        .args(["-Oz", "-o"])
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

    println!(
        "cargo:warning=Dev wallet WASM: {} bytes",
        std::fs::metadata(&wasm_dst).unwrap().len()
    );
}
