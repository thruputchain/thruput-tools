//! Envelope for all `--json` command output.
//!
//! Every command wraps its payload in [`CommandResult`] so
//! that consumers see a uniform top-level shape:
//! `{ ok, command, warnings, data }`. Commands with stable machine-readable
//! failures may also include an `error` object.

use serde::Serialize;
use serde_json::Value;

use crate::CliError;
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

    /// The envelope form of a command error: its stable code, message,
    /// hint, and structured details, as the stderr error envelope reports
    /// them.
    pub fn from_cli_error(error: &CliError) -> Self {
        let command_error = Self::new(
            error.code(),
            error.to_string(),
            error.details_json().unwrap_or(Value::Null),
        );
        match error.hint() {
            Some(hint) => command_error.with_hint(hint),
            None => command_error,
        }
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
