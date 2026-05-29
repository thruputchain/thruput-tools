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
    let (run_dir, _temp_guard) = existing_smoke_run_dir("chain-tip-smoke");
    eprintln!("existing chain-tip run dir {}", run_dir.display());

    let status = Command::new(env!("CARGO_BIN_EXE_hoglab"))
        .arg("run")
        .arg(&scenario)
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg("--runtime-manifest")
        .arg(runtime_manifest)
        .arg("--out-dir")
        .arg(&run_dir)
        .status()
        .expect("hoglab should launch");

    assert!(status.success(), "hoglab run failed with status {status}");
    assert_common_existing_artifacts(&run_dir);
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
    let (run_dir, _temp_guard) = existing_smoke_run_dir("chain-tip-smoke-baseline");
    eprintln!("existing baseline run dir {}", run_dir.display());

    let status = Command::new(env!("CARGO_BIN_EXE_hoglab"))
        .arg("run")
        .arg(&scenario)
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg("--runtime-manifest")
        .arg(runtime_manifest)
        .arg("--out-dir")
        .arg(&run_dir)
        .status()
        .expect("hoglab should launch");

    assert!(status.success(), "hoglab run failed with status {status}");
    assert_common_existing_artifacts(&run_dir);
    let result: serde_json::Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("result.json")).expect("result should read"),
    )
    .expect("result should parse");
    assert_eq!(result["outcome"], "passed");
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

fn existing_smoke_run_dir(name: &str) -> (PathBuf, Option<tempfile::TempDir>) {
    if let Ok(root) = std::env::var("GROUNDHOG_SIM_EXISTING_ROOT") {
        let run_dir = PathBuf::from(root).join(name);
        std::fs::create_dir_all(&run_dir).expect("existing smoke run dir should be created");
        return (run_dir, None);
    }

    let temp = tempfile::tempdir().expect("temp dir should be created");
    (temp.path().to_path_buf(), Some(temp))
}

fn assert_common_existing_artifacts(run_dir: &std::path::Path) {
    assert!(run_dir.join("result.json").exists());
    assert!(run_dir.join("result.jsonl").exists());
    assert!(run_dir.join("simulation.json").exists());
    assert!(run_dir.join("runtime-manifest.json").exists());
    assert!(run_dir.join("target-ownership.json").exists());

    let ownership: serde_json::Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("target-ownership.json"))
            .expect("target ownership should read"),
    )
    .expect("target ownership should parse");
    assert_eq!(ownership["startedBySim"], false);
    assert_eq!(ownership["cleanupAllowed"], false);
}
