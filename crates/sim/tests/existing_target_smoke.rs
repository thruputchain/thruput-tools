use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "requires GROUNDHOG_SIM_RPC_URL and GROUNDHOG_SIM_RUNTIME_MANIFEST"]
fn existing_target_chain_tip_smoke() {
    let rpc_url = std::env::var("GROUNDHOG_SIM_RPC_URL")
        .expect("set GROUNDHOG_SIM_RPC_URL to a running Groundhog JSON-RPC endpoint");
    let runtime_manifest = std::env::var("GROUNDHOG_SIM_RUNTIME_MANIFEST")
        .expect("set GROUNDHOG_SIM_RUNTIME_MANIFEST to runtime-manifest.json");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("sim crate should be two levels below repo root")
        .to_path_buf();
    let scenario = repo_root.join("examples/sim/chain-tip-smoke.json");
    let temp = tempfile::tempdir().expect("temp dir should be created");

    let status = Command::new(env!("CARGO_BIN_EXE_hoglab"))
        .arg("run")
        .arg(&scenario)
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg("--runtime-manifest")
        .arg(runtime_manifest)
        .arg("--out-dir")
        .arg(temp.path())
        .status()
        .expect("hoglab should launch");

    assert!(status.success(), "hoglab run failed with status {status}");
    assert!(temp.path().join("result.json").exists());
    assert!(temp.path().join("result.jsonl").exists());
}

#[test]
#[ignore = "requires GROUNDHOG_SIM_RPC_URL and GROUNDHOG_SIM_RUNTIME_MANIFEST"]
fn existing_target_chain_tip_baseline_smoke() {
    let rpc_url = std::env::var("GROUNDHOG_SIM_RPC_URL")
        .expect("set GROUNDHOG_SIM_RPC_URL to a running Groundhog JSON-RPC endpoint");
    let runtime_manifest = std::env::var("GROUNDHOG_SIM_RUNTIME_MANIFEST")
        .expect("set GROUNDHOG_SIM_RUNTIME_MANIFEST to runtime-manifest.json");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("sim crate should be two levels below repo root")
        .to_path_buf();
    let scenario = repo_root.join("examples/sim/chain-tip-smoke-baseline.json");
    let temp = tempfile::tempdir().expect("temp dir should be created");

    let status = Command::new(env!("CARGO_BIN_EXE_hoglab"))
        .arg("run")
        .arg(&scenario)
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg("--runtime-manifest")
        .arg(runtime_manifest)
        .arg("--out-dir")
        .arg(temp.path())
        .status()
        .expect("hoglab should launch");

    assert!(status.success(), "hoglab run failed with status {status}");
    let result: serde_json::Value = serde_json::from_slice(
        &std::fs::read(temp.path().join("result.json")).expect("result should read"),
    )
    .expect("result should parse");
    assert_eq!(
        result["baselineDiff"]["path"],
        "baselines/chain-tip-smoke-result.json"
    );
    assert_eq!(result["baselineDiff"]["diff"]["schemaVersion"], 1);
    assert!(
        result["baselineDiff"]["diff"]["workloadCountDeltas"]["chain-tip"]["completed"]
            .as_i64()
            .expect("completed delta should be numeric")
            > 0
    );
}
