use crate::CliError;
use serde::Serialize;
use serde_json::json;
use std::io::{self, IsTerminal, Write};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";

/// Controls CLI output. Status messages go to stderr
/// (stdout is reserved for data / cargo passthrough).
pub struct Print {
    quiet: bool,
    json: bool,
    trace_commands: bool,
}

impl Print {
    pub fn new(quiet: bool, json: bool) -> Self {
        Self {
            quiet,
            json,
            trace_commands: false,
        }
    }

    /// A `Print` that suppresses all status messages.
    pub fn quiet() -> Self {
        Self {
            quiet: true,
            json: false,
            trace_commands: false,
        }
    }

    /// Enable or disable tracing of subprocess invocations to stderr.
    #[must_use]
    pub fn with_command_trace(mut self, on: bool) -> Self {
        self.trace_commands = on;
        self
    }

    /// Returns whether structured JSON output is enabled.
    pub fn is_json(&self) -> bool {
        self.json
    }

    /// Returns whether quiet (non-interactive) mode is active.
    pub fn is_quiet(&self) -> bool {
        self.quiet
    }

    /// Returns whether styled interactive output is appropriate.
    pub fn is_human_tty(&self) -> bool {
        !self.quiet && !self.json && io::stderr().is_terminal()
    }

    /// Trace a subprocess invocation to stderr when command tracing is on.
    ///
    /// Opt-in diagnostic (see the global `--show-cargo-commands` flag). Writes
    /// to stderr so it never pollutes `--json` stdout, and is emitted even
    /// under `--quiet`/`--json` because it was explicitly requested. The
    /// rendering is for human inspection, not guaranteed shell-safe re-execution.
    pub fn trace_command(&self, command: &std::process::Command) {
        if !self.trace_commands {
            return;
        }
        eprintln!("+ {}", render_command(command));
    }

    /// Informational status message (suppressed by --quiet).
    pub fn status(&self, msg: impl std::fmt::Display) {
        if !self.quiet && !self.json {
            eprintln!("{msg}");
        }
    }

    /// Styled success message for human output.
    pub fn success(&self, msg: impl std::fmt::Display) {
        if self.is_human_tty() {
            eprintln!("{GREEN}✓{RESET} {msg}");
        } else {
            self.status(format_args!("[ok] {msg}"));
        }
    }

    /// Styled note message for human output.
    pub fn note(&self, msg: impl std::fmt::Display) {
        if self.is_human_tty() {
            eprintln!("{YELLOW}•{RESET} {msg}");
        } else {
            self.status(format_args!("- {msg}"));
        }
    }

    /// Label/value detail line for human output.
    pub fn detail(&self, label: &str, value: impl std::fmt::Display) {
        if self.is_human_tty() {
            eprintln!("  {DIM}{label}:{RESET} {value}");
        } else {
            self.status(format_args!("  {label}: {value}"));
        }
    }

    /// Start a short human task line that can be replaced by success in TTY mode.
    pub fn start_task(&self, msg: impl std::fmt::Display) -> TaskLine<'_> {
        let tty = self.is_human_tty();
        if tty {
            eprint!("{CYAN}…{RESET} {msg}");
            let _ = io::stderr().flush();
        } else {
            self.status(msg);
        }

        TaskLine {
            print: self,
            tty,
            active: tty,
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

    /// Write a command error using the active output mode.
    ///
    /// This is used by long-running commands that need to report
    /// multiple failures without returning immediately.
    pub fn command_error(&self, command: &str, error: &CliError) -> Result<(), CliError> {
        if self.json {
            return self.error(command, error);
        }

        let mut stderr = io::stderr();
        if self.is_human_tty() {
            writeln!(stderr, "{RED}✗{RESET} {error}")?;
        } else {
            writeln!(stderr, "error: {error}")?;
        }
        if let Some(hint) = error.hint() {
            writeln!(stderr)?;
            if self.is_human_tty() {
                writeln!(stderr, "  {DIM}hint:{RESET} {hint}")?;
            } else {
                writeln!(stderr, "  hint: {hint}")?;
            }
        }
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn write_json_to_stderr(&self, value: &impl Serialize) -> Result<(), CliError> {
        let mut stderr = io::stderr();
        serde_json::to_writer(&mut stderr, value).map_err(|e| CliError::Other(e.to_string()))?;
        writeln!(stderr)?;
        Ok(())
    }
}

/// A short-lived task line started by [`Print::start_task`].
pub struct TaskLine<'a> {
    print: &'a Print,
    tty: bool,
    active: bool,
}

impl TaskLine<'_> {
    /// Clear the loading line in TTY mode without printing a replacement.
    pub fn clear(mut self) {
        if self.tty {
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
        self.active = false;
    }

    /// Replace the loading line with a styled success message in TTY mode.
    pub fn success(mut self, msg: impl std::fmt::Display) {
        if self.tty {
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
        self.active = false;
        self.print.success(msg);
    }
}

impl Drop for TaskLine<'_> {
    fn drop(&mut self) {
        if self.active && self.tty {
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
    }
}

/// Render a subprocess invocation as a single inspectable line.
///
/// Includes the working directory as a `(cd <dir> && ...)` wrapper when the
/// command sets one. Quoting is not shell-safe; the output is a diagnostic
/// aid, not a re-runnable script.
fn render_command(command: &std::process::Command) -> String {
    use std::fmt::Write as _;

    let mut rendered = String::new();
    let scoped = command.get_current_dir();
    if let Some(dir) = scoped {
        let _ = write!(rendered, "(cd {} && ", dir.display());
    }
    rendered.push_str(&command.get_program().to_string_lossy());
    for arg in command.get_args() {
        rendered.push(' ');
        rendered.push_str(&arg.to_string_lossy());
    }
    if scoped.is_some() {
        rendered.push(')');
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::render_command;
    use std::process::Command;

    #[test]
    fn renders_program_and_args() {
        let mut cmd = Command::new("cargo");
        cmd.arg("build").arg("--release");
        assert_eq!(render_command(&cmd), "cargo build --release");
    }

    #[test]
    fn wraps_working_directory_when_set() {
        let mut cmd = Command::new("cargo");
        cmd.current_dir("/ws").arg("metadata");
        assert_eq!(render_command(&cmd), "(cd /ws && cargo metadata)");
    }
}
