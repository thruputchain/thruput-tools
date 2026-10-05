use std::process::Command;

#[test]
fn validate_accepts_rv32_and_rejects_legacy_wasm_artifacts() {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/sim/local-fibonacci-view-smoke.json");
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
