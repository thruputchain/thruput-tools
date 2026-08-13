//! Envelope for all `--json` command output.
//!
//! Every command wraps its payload in [`CommandResult`] so
//! that consumers see a uniform top-level shape:
//! `{ ok, command, warnings, data }`. Commands with stable machine-readable
//! failures may also include an `error` object.

use serde::Serialize;
use serde_json::Value;

use crate::checks::{Finding, FindingLevel};

/// Uniform JSON envelope for command output.
///
/// Most commands emit success records with `ok = true`.
/// Commands with stable machine-readable failure output may
/// also emit `ok = false` records directly.
#[derive(Debug, Serialize)]
pub struct CommandResult<T: Serialize> {
    ok: bool,
    command: &'static str,
    warnings: Vec<Finding>,
    data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CommandError>,
}

/// Stable machine-readable error attached to a command-owned failure result.
#[derive(Debug, Serialize)]
pub struct CommandError {
    code: &'static str,
    message: String,
    hint: Option<String>,
    details: Value,
}

impl CommandError {
    pub fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details,
        }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl<T: Serialize> CommandResult<T> {
    /// Wrap a command's data in the envelope with no
    /// warnings.
    pub fn new(command: &'static str, data: T) -> Self {
        Self {
            ok: true,
            command,
            warnings: Vec::new(),
            data,
            error: None,
        }
    }

    /// Wrap a command's data with warnings.
    pub fn with_warnings(command: &'static str, warnings: Vec<Finding>, data: T) -> Self {
        Self {
            ok: true,
            command,
            warnings,
            data,
            error: None,
        }
    }

    /// Wrap a command's data in a structured failure envelope.
    pub fn failure(command: &'static str, data: T) -> Self {
        Self {
            ok: false,
            command,
            warnings: Vec::new(),
            data,
            error: None,
        }
    }

    /// Wrap a command's data in a structured failure envelope
    /// with warnings.
    pub fn failure_with_warnings(command: &'static str, warnings: Vec<Finding>, data: T) -> Self {
        Self {
            ok: false,
            command,
            warnings,
            data,
            error: None,
        }
    }

    /// Wrap a command's data and stable machine-readable error in a structured
    /// failure envelope.
    pub fn failure_with_error(command: &'static str, data: T, error: CommandError) -> Self {
        Self {
            ok: false,
            command,
            warnings: Vec::new(),
            data,
            error: Some(error),
        }
    }

    /// True when any warn-level finding exists.
    pub fn has_warnings(&self) -> bool {
        self.warnings.iter().any(|w| w.level == FindingLevel::Warn)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

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
}
