use std::path::PathBuf;
use thruput_cli::artifacts::ArtifactKind;

use thruput_cli::test_json::*;

#[test]
fn missing_mode_deserializes_as_rv32_default() {
    let json = serde_json::json!({
        "schemaVersion": 7,
        "projectRoot": "/project",
        "profile": {
            "name": "default",
            "rpcUrl": null,
            "defaultKey": null,
            "runtimeManifest": null,
            "contracts": {},
            "source": null
        },
        "artifact": {
            "kind": "rv32",
            "path": "/project/target/demo.rv32a"
        },
        "success": true,
        "summary": {
            "source": "empty",
            "total": "0",
            "passed": "0",
            "failed": "0",
            "ignored": "0",
            "measured": "0",
            "filteredOut": "0"
        },
        "runs": [],
        "rerun": {
            "enabled": false,
            "cachePath": "/project/target/thruput/test-failures/demo.json",
            "cachedTests": [],
            "cachedTestsAfter": [],
            "executedTests": [],
            "failedTests": []
        },
        "watch": {
            "enabled": false
        }
    });

    let report: TestReport = serde_json::from_value(json).expect("current report should parse");

    assert_eq!(report.mode, TestMode::Rv32);
    assert!(report.package.is_empty());
    assert_eq!(report.schema_version, 7);
    assert_eq!(report.artifact.kind, ArtifactKind::Rv32);
    assert_eq!(
        report.artifact.path,
        PathBuf::from("/project/target/demo.rv32a")
    );
    assert!(report.integration.is_none());
}

#[test]
fn legacy_wasm_mode_and_artifact_kind_are_rejected() {
    serde_json::from_value::<TestMode>(serde_json::json!("wasm"))
        .expect_err("bug: removed WASM mode must not deserialize");
    serde_json::from_value::<ArtifactKind>(serde_json::json!("wasm"))
        .expect_err("bug: removed WASM artifact kind must not deserialize");
}

#[test]
fn old_reports_without_artifact_fail_deserialization() {
    let json = serde_json::json!({
        "schemaVersion": 2,
        "mode": "rv32",
        "projectRoot": "/project",
        "profile": {
            "name": "default",
            "rpcUrl": null,
            "defaultKey": null,
            "runtimeManifest": null,
            "contracts": {},
            "source": null
        },
        "rv32": "/project/target/demo.rv32a",
        "success": true,
        "summary": {
            "source": "empty",
            "total": "0",
            "passed": "0",
            "failed": "0",
            "ignored": "0",
            "measured": "0",
            "filteredOut": "0"
        },
        "runs": [],
        "rerun": {
            "enabled": false,
            "cachePath": "/project/target/thruput/test-failures/demo.json",
            "cachedTests": [],
            "cachedTestsAfter": [],
            "executedTests": [],
            "failedTests": []
        },
        "watch": {
            "enabled": false
        }
    });

    let err = serde_json::from_value::<TestReport>(json).expect_err("v2 report should fail");

    assert!(
        err.to_string().contains("missing field `artifact`"),
        "unexpected error: {err}"
    );
}
