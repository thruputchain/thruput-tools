use std::process::Command;

#[test]
fn validate_accepts_rv32_and_rejects_legacy_wasm_artifacts() {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../examples/sim/local-fibonacci-view-smoke.json");
    let accepted = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("validate")
        .arg(&scenario)
        .output()
        .expect("hogsim validate should run");
    assert!(
        accepted.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let temp = tempfile::tempdir().expect("tempdir should create");
    let legacy_scenario = temp.path().join("legacy-wasm.json");
    let contents = std::fs::read_to_string(&scenario).expect("scenario should read");
    std::fs::write(
        &legacy_scenario,
        contents
            .replacen(r#""kind": "rv32""#, r#""kind": "wasm""#, 1)
            .replacen(".rv32a", ".wasm", 1),
    )
    .expect("legacy scenario should write");

    let rejected = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("validate")
        .arg(&legacy_scenario)
        .output()
        .expect("hogsim validate should run");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("unknown variant `wasm`"));
}

#[test]
fn read_only_existing_run_does_not_require_confirmation() {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../examples/sim/chain-tip-smoke.json");

    let output = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("run")
        .arg(scenario)
        .output()
        .expect("hogsim run should launch");

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("confirmation"));
}

#[test]
fn mutating_existing_run_fails_closed_without_force_when_non_interactive() {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../examples/sim/local-wallet-soak.json");

    let output = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("run")
        .arg(scenario)
        .output()
        .expect("hogsim run should launch");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("rerun with --force"));
}

#[test]
fn mutating_existing_json_run_emits_structured_confirmation_requirement() {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../examples/sim/local-wallet-soak.json");

    let output = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("run")
        .arg(scenario)
        .arg("--json")
        .output()
        .expect("hogsim run should launch");

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("confirmation should parse");
    assert_eq!(
        report,
        serde_json::json!({
            "ok": false,
            "command": "run",
            "warnings": [],
            "data": {
                "schemaVersion": 1,
                "status": "confirmation_required",
                "action": "mutate an existing simulation target",
                "requiredFlag": "--force"
            }
        })
    );
}

#[test]
fn local_run_does_not_require_confirmation_when_non_interactive() {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../examples/sim/local-chain-tip-smoke.json");

    let output = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("run")
        .arg(scenario)
        .arg("--groundhog-binary")
        .arg("/definitely/missing/groundhog")
        .output()
        .expect("hogsim run should launch");

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("confirmation"));
}

#[test]
fn empty_json_sweep_does_not_require_force() {
    let root = tempfile::tempdir().expect("tempdir should create");

    let output = Command::new(env!("CARGO_BIN_EXE_hogsim"))
        .arg("sweep")
        .arg(root.path())
        .arg("--json")
        .output()
        .expect("hogsim sweep should launch");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("sweep report should parse");
    assert_eq!(report["entries"], serde_json::json!([]));
}
