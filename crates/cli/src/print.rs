use crate::CliError;
use serde::Serialize;
use serde_json::json;
use std::io::{self, Write};

/// Controls CLI output. Status messages go to stderr
/// (stdout is reserved for data / cargo passthrough).
pub struct Print {
    quiet: bool,
    json: bool,
}

impl Print {
    pub fn new(quiet: bool, json: bool) -> Self {
        Self { quiet, json }
    }

    /// A `Print` that suppresses all status messages.
    pub fn quiet() -> Self {
        Self {
            quiet: true,
            json: false,
        }
    }

    /// Returns whether structured JSON output is enabled.
    pub fn is_json(&self) -> bool {
        self.json
    }

    /// Returns whether quiet (non-interactive) mode is active.
    pub fn is_quiet(&self) -> bool {
        self.quiet
    }

    /// Informational status message (suppressed by --quiet).
    pub fn status(&self, msg: impl std::fmt::Display) {
        if !self.quiet && !self.json {
            eprintln!("{msg}");
        }
    }

    /// Warning — always printed in human mode. No-op in
    /// JSON mode (commands must collect warnings into the
    /// command result envelope instead).
    pub fn warn(&self, msg: impl std::fmt::Display) {
        if !self.json {
            eprintln!("warning: {msg}");
        }
    }

    /// Prompt the user for input — always printed
    /// regardless of --quiet.
    pub fn prompt(&self, msg: impl std::fmt::Display) -> Result<(), CliError> {
        let mut stderr = io::stderr();
        write!(stderr, "{msg}")?;
        stderr.flush()?;
        Ok(())
    }

    /// Write a structured success payload to stdout when
    /// `--json` is active.
    pub fn json(&self, value: &impl Serialize) -> Result<(), CliError> {
        if !self.json {
            return Ok(());
        }

        let mut stdout = io::stdout();
        serde_json::to_writer(&mut stdout, value).map_err(|e| CliError::Other(e.to_string()))?;
        writeln!(stdout)?;
        Ok(())
    }

    /// Write a structured error payload to stderr when
    /// `--json` is active.
    pub fn error(&self, command: &str, error: &CliError) -> Result<(), CliError> {
        if !self.json {
            return Ok(());
        }

        self.write_json_to_stderr(&json!({
            "ok": false,
            "command": command,
            "error": {
                "code": error.code(),
                "message": error.to_string(),
                "hint": error.hint(),
                "details": error.details_json(),
            }
        }))
    }

    #[allow(clippy::unused_self)]
    fn write_json_to_stderr(&self, value: &impl Serialize) -> Result<(), CliError> {
        let mut stderr = io::stderr();
        serde_json::to_writer(&mut stderr, value).map_err(|e| CliError::Other(e.to_string()))?;
        writeln!(stderr)?;
        Ok(())
    }
}
