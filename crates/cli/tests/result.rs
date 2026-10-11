use thruput_cli::checks::Finding;
use thruput_cli::checks::FindingLevel;

use std::borrow::Cow;

use thruput_cli::result::*;

#[test]
fn new_result_serializes_envelope() {
    let result = CommandResult::new("test.cmd", serde_json::json!({"key": "value"}));
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "test.cmd");
    assert!(json["warnings"].as_array().unwrap().is_empty());
    assert_eq!(json["data"]["key"], "value");
}

#[test]
fn with_warnings_includes_findings() {
    let warnings = vec![Finding {
        level: FindingLevel::Warn,
        code: "test_warn",
        message: "a warning".into(),
        hint: Some(Cow::Borrowed("fix it")),
    }];
    let result = CommandResult::with_warnings("status", warnings, serde_json::json!({}));
    assert!(result.has_warnings());
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["warnings"][0]["code"], "test_warn");
}

#[test]
fn failure_result_serializes_false_ok() {
    let result = CommandResult::failure("test", serde_json::json!({"status": "failed"}));
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["command"], "test");
    assert_eq!(json["data"]["status"], "failed");
}

#[test]
fn failure_with_warnings_includes_findings() {
    let warnings = vec![Finding {
        level: FindingLevel::Warn,
        code: "test_warn",
        message: "a warning".into(),
        hint: None,
    }];
    let result = CommandResult::failure_with_warnings(
        "test",
        warnings,
        serde_json::json!({"status": "failed"}),
    );
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["warnings"][0]["code"], "test_warn");
}

#[test]
fn failure_with_error_includes_stable_error() {
    let result = CommandResult::failure_with_error(
        "deploy",
        serde_json::json!({"status": "failed"}),
        CommandError::new(
            "transaction_reverted",
            "transaction reverted",
            serde_json::json!({"result": "reverted"}),
        ),
    );
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "transaction_reverted");
    assert_eq!(json["error"]["message"], "transaction reverted");
    assert!(json["error"]["hint"].is_null());
    assert_eq!(json["error"]["details"]["result"], "reverted");
}

#[test]
fn info_only_warnings_do_not_trigger_has_warnings() {
    let warnings = vec![Finding {
        level: FindingLevel::Info,
        code: "info_only",
        message: "just info".into(),
        hint: None,
    }];
    let result = CommandResult::with_warnings("status", warnings, serde_json::json!({}));
    assert!(!result.has_warnings());
}
