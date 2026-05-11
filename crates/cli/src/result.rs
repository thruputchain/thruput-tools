//! Envelope for all `--json` command output.
//!
//! Every command wraps its payload in [`CommandResult`] so
//! that consumers see a uniform top-level shape:
//! `{ ok, command, warnings, data }`.

use serde::Serialize;

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
        }
    }

    /// Wrap a command's data with warnings.
    pub fn with_warnings(command: &'static str, warnings: Vec<Finding>, data: T) -> Self {
        Self {
            ok: true,
            command,
            warnings,
            data,
        }
    }

    /// Wrap a command's data in a structured failure envelope.
    pub fn failure(command: &'static str, data: T) -> Self {
        Self {
            ok: false,
            command,
            warnings: Vec::new(),
            data,
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
