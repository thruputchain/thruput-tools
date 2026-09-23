use crate::CliError;
use crate::terminal::terminal_safe;
use serde::Serialize;
use serde_json::json;
use std::io::{self, IsTerminal, Write};
mod diagnostic;
mod log;
mod policy;
pub(crate) mod progress;
mod render;
mod report;
mod theme;
pub use theme::Outcome;

const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";

/// Controls CLI output. Status messages go to stderr
/// (stdout is reserved for data / cargo passthrough).
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent CLI switches retain their existing combinations and semantics"
)]
pub struct Print {
    quiet: bool,
    json: bool,
    trace_commands: bool,
    no_ui: bool,
    task: Option<std::sync::Arc<progress::TaskOutput>>,
    verbose_build: bool,
    compact_build: bool,
    diagnostics: std::sync::Arc<std::sync::Mutex<Vec<diagnostic::Diagnostic>>>,
}

impl Print {
    pub fn new(quiet: bool, json: bool) -> Self {
        Self {
            quiet,
            json,
            trace_commands: false,
            no_ui: false,
            task: None,
            verbose_build: false,
            compact_build: false,
            diagnostics: std::sync::Arc::default(),
        }
    }

    /// A `Print` that suppresses all status messages.
    pub fn quiet() -> Self {
        Self {
            quiet: true,
            json: false,
            trace_commands: false,
            no_ui: false,
            task: None,
            verbose_build: false,
            compact_build: false,
            diagnostics: std::sync::Arc::default(),
        }
    }

    /// Enable or disable tracing of subprocess invocations to stderr.
    #[must_use]
    pub fn with_command_trace(mut self, on: bool) -> Self {
        self.trace_commands = on;
        self
    }

    /// Select plain append-only human output without changing confirmation rules.
    #[must_use]
    pub fn with_no_ui(mut self, no_ui: bool) -> Self {
        self.no_ui = no_ui;
        self
    }

    pub(crate) fn is_no_ui(&self) -> bool {
        self.no_ui
    }

    fn output_policy(&self) -> policy::Policy {
        let mut policy = policy::Policy::stderr(self.no_ui || self.quiet || self.json);
        // The live viewport inserts safe plain diagnostic text; final reports are
        // rendered with the parent's full theme after the worker has finished.
        if self.is_live() {
            policy.color = false;
        }
        policy
    }

    fn write_report(&self, output: report::Report) {
        for line in output.into_lines() {
            self.emit_human(&line);
        }
    }

    fn emit_human(&self, line: &str) {
        if !self
            .task
            .as_ref()
            .is_some_and(|output| output.line(progress::Stream::Stderr, line))
        {
            eprintln!("{line}");
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        self.task.as_ref().is_some_and(|output| output.live())
    }

    pub(crate) fn in_task(&self) -> bool {
        self.task.is_some()
    }

    pub(crate) fn relay_output(&self, stream: progress::Stream, bytes: &[u8]) -> io::Result<()> {
        if let Some(output) = &self.task {
            output.data(stream, bytes)
        } else {
            match stream {
                progress::Stream::Stdout => io::stdout().lock().write_all(bytes),
                progress::Stream::Stderr => io::stderr().lock().write_all(bytes),
            }
        }
    }

    /// Update a task's phase, or print a normal status line in append-only modes.
    pub fn phase(&self, message: impl std::fmt::Display) {
        if self.quiet || self.json {
            return;
        }
        let message = message.to_string();
        if !self
            .task
            .as_ref()
            .is_some_and(|output| output.phase(&message))
        {
            self.status(message);
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

    /// Returns whether this invocation may safely read an interactive answer.
    pub fn can_prompt(&self) -> bool {
        self.output_allows_prompt()
            && terminals_allow_prompt(io::stdin().is_terminal(), io::stderr().is_terminal())
    }

    /// Reject a protected action before preflight when no confirmation can be collected.
    pub(crate) fn require_confirmation_available(
        &self,
        force: bool,
        action: &'static str,
    ) -> Result<(), CliError> {
        if force || self.can_prompt() {
            Ok(())
        } else {
            Err(CliError::ConfirmationRequired { action })
        }
    }

    /// Confirm a protected action, or require `--force` when prompting is unavailable.
    pub fn confirm(
        &self,
        force: bool,
        action: &'static str,
        message: impl std::fmt::Display,
    ) -> Result<bool, CliError> {
        self.require_confirmation_available(force, action)?;
        if force {
            return Ok(true);
        }

        let message = terminal_safe(&message.to_string());
        self.prompt(format_args!("{message} [y/N] "))?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        Ok(confirmation_answer(&answer))
    }

    /// Ask an optional wizard question, using its default in quiet mode.
    pub fn ask(
        &self,
        message: impl std::fmt::Display,
        default_yes: bool,
    ) -> Result<bool, CliError> {
        if self.quiet {
            return Ok(default_yes);
        }
        if !self.can_prompt() {
            return Ok(false);
        }

        let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
        self.prompt(format_args!("{message} {suffix} "))?;
        let mut answer = String::new();
        let bytes_read = io::stdin().read_line(&mut answer)?;
        Ok(wizard_answer(&answer, default_yes, bytes_read != 0))
    }

    fn output_allows_prompt(&self) -> bool {
        !self.quiet && !self.json
    }

    /// Returns whether styled interactive output is appropriate.
    pub fn is_human_tty(&self) -> bool {
        self.output_policy().color
    }

    /// Returns whether styled human data on stdout is appropriate.
    pub fn is_human_stdout_tty(&self) -> bool {
        !self.quiet
            && !self.json
            && !self.no_ui
            && io::stdout().is_terminal()
            && terminal_allows_ansi(
                std::env::var_os("TERM").as_deref(),
                std::env::var_os("NO_COLOR").as_deref(),
            )
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
        self.emit_human(&format!("+ {}", render_command(command)));
    }

    /// Informational status message (suppressed by --quiet).
    pub fn status(&self, msg: impl std::fmt::Display) {
        if !self.quiet && !self.json && !self.compact_build {
            self.emit_human(&msg.to_string());
        }
    }

    /// Write one human-readable data row to stdout.
    ///
    /// Data remains visible under `--quiet`; JSON commands should use [`Self::json`].
    pub fn data_line(&self, value: impl std::fmt::Display) -> Result<(), CliError> {
        if self.json {
            return Ok(());
        }
        if self.is_live() {
            self.relay_output(progress::Stream::Stdout, format!("{value}\n").as_bytes())?;
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{value}")?;
        Ok(())
    }

    /// Styled success message for human output.
    pub fn success(&self, msg: impl std::fmt::Display) {
        self.outcome(Outcome::Success, msg);
    }

    /// Styled note message for human output.
    pub fn note(&self, msg: impl std::fmt::Display) {
        self.outcome(Outcome::Information, msg);
    }

    /// Render an explicit human outcome without inferring operation success.
    pub fn outcome(&self, outcome: Outcome, msg: impl std::fmt::Display) {
        if self.quiet || self.json {
            return;
        }
        let mut report = report::Report::new(self.output_policy());
        report.message(outcome, &msg.to_string(), None, None);
        self.write_report(report);
    }

    /// Label/value detail line for human output.
    pub fn detail(&self, label: &str, value: impl std::fmt::Display) {
        if self.quiet || self.json {
            return;
        }
        let mut output = report::Report::new(self.output_policy());
        output.detail(label, &value.to_string());
        self.write_report(output);
    }

    /// Render a responsive green summary card on stderr.
    ///
    /// Accepts a title and ordered label/value rows. Handles terminal width,
    /// Unicode wrapping, narrow layouts, color, and plain redirected output.
    /// Suppressed by quiet and JSON modes.
    pub fn summary_card(&self, title: &str, items: &[(String, String)]) {
        self.summary_report(title, items, std::iter::empty());
    }

    /// Render a summary card followed by width-aware findings and hints.
    pub(crate) fn summary_report<'a>(
        &self,
        title: &str,
        items: &[(String, String)],
        findings: impl Iterator<Item = &'a crate::checks::Finding>,
    ) {
        if self.quiet || self.json {
            return;
        }
        let mut output = report::Report::new(self.output_policy());
        output.card(title, items);
        output.findings(findings);
        self.write_report(output);
    }

    /// Render records as a responsive table, or stacked cards on narrow terminals.
    /// Missing cells are empty; extra cells receive numbered column labels.
    pub fn summary_table(&self, title: &str, headers: &[String], rows: &[Vec<String>]) {
        if self.quiet || self.json {
            return;
        }
        let mut output = report::Report::new(self.output_policy());
        output.table(title, headers, rows);
        self.write_report(output);
    }

    /// Render a human data table on stdout, retaining plain rows for pipes and quiet mode.
    pub fn data_table(
        &self,
        title: &str,
        headers: &[String],
        rows: &[Vec<String>],
        empty_message: &str,
        plain_rows: impl Iterator<Item = String>,
    ) -> Result<(), CliError> {
        if self.json {
            return Ok(());
        }
        let policy = policy::Policy::stdout(self.no_ui || self.quiet);
        let mut stdout = io::stdout().lock();
        if policy.width.is_none() {
            if rows.is_empty() {
                writeln!(stdout, "{empty_message}")?;
            } else {
                for line in plain_rows {
                    writeln!(stdout, "{line}")?;
                }
            }
        } else {
            let mut report = report::Report::new(policy);
            if rows.is_empty() {
                report.outcome_card(
                    Outcome::Empty,
                    title,
                    &[("Result".into(), empty_message.into())],
                );
            } else {
                report.table(title, headers, rows);
            }
            for line in report.into_lines() {
                writeln!(stdout, "{line}")?;
            }
        }
        Ok(())
    }

    /// Render a result card with an explicit outcome label in its title.
    pub fn outcome_card(&self, outcome: Outcome, title: &str, items: &[(String, String)]) {
        self.outcome_report(outcome, title, items, &[]);
    }

    /// Render a final outcome and warnings using the card's reading width.
    /// Quiet mode retains warnings; JSON commands own their structured warning payload.
    pub fn outcome_report(
        &self,
        outcome: Outcome,
        title: &str,
        items: &[(String, String)],
        warnings: &[String],
    ) {
        if self.json {
            return;
        }
        if self.quiet {
            for warning in warnings {
                self.warn(warning);
            }
            return;
        }
        let mut output = report::Report::new(self.output_policy());
        output.outcome_card(outcome, title, items);
        output.warnings(warnings);
        self.append_diagnostics(&mut output);
        self.write_report(output);
    }

    /// Render a conventional `Next:` block for human output.
    pub fn next_steps(&self, steps: &[String]) {
        if self.quiet || self.json {
            return;
        }

        let mut output = report::Report::new(self.output_policy());
        output.next_steps(steps);
        self.write_report(output);
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
            if let Some(output) = &self.task {
                output.warn(msg.to_string());
                return;
            }
            let mut output = report::Report::new(self.output_policy());
            output.message(Outcome::Warning, &msg.to_string(), None, None);
            self.write_report(output);
        }
    }

    /// Prompt the user for input — always printed
    /// regardless of --quiet.
    pub fn prompt(&self, msg: impl std::fmt::Display) -> Result<(), CliError> {
        let mut report = report::Report::new(self.output_policy());
        report.prompt(&msg.to_string());
        let lines = report.into_lines();
        let mut stderr = io::stderr();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(stderr)?;
            }
            write!(stderr, "{line}")?;
        }
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

        let mut output = report::Report::new(self.output_policy());
        output.message(
            Outcome::Error,
            &error.to_string(),
            Some(error.code()),
            error.hint().as_deref(),
        );
        self.append_diagnostics(&mut output);
        if self.is_live() {
            for line in output.into_lines() {
                self.relay_output(progress::Stream::Stderr, format!("{line}\n").as_bytes())?;
            }
        } else {
            let mut stderr = io::stderr().lock();
            for line in output.into_lines() {
                writeln!(stderr, "{line}")?;
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

pub(crate) fn supports_ansi_stderr() -> bool {
    io::stderr().is_terminal()
        && terminal_allows_ansi(
            std::env::var_os("TERM").as_deref(),
            std::env::var_os("NO_COLOR").as_deref(),
        )
}

fn terminal_allows_ansi(
    term: Option<&std::ffi::OsStr>,
    no_color: Option<&std::ffi::OsStr>,
) -> bool {
    if no_color.is_some() {
        return false;
    }
    !matches!(term.and_then(std::ffi::OsStr::to_str), Some("dumb"))
}

fn terminals_allow_prompt(stdin_tty: bool, stderr_tty: bool) -> bool {
    stdin_tty && stderr_tty
}

fn confirmation_answer(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn wizard_answer(answer: &str, default_yes: bool, received_input: bool) -> bool {
    if !received_input {
        return false;
    }
    let answer = answer.trim();
    if answer.is_empty() {
        return default_yes;
    }
    matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")
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
    use super::{
        Print, confirmation_answer, render_command, terminal_allows_ansi, terminals_allow_prompt,
        wizard_answer,
    };
    use std::ffi::OsStr;
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

    #[test]
    fn terminal_allows_ansi_respects_no_color_and_dumb_term() {
        assert!(terminal_allows_ansi(
            Some(OsStr::new("xterm-256color")),
            None
        ));
        assert!(!terminal_allows_ansi(
            Some(OsStr::new("xterm-256color")),
            Some(OsStr::new("1"))
        ));
        assert!(!terminal_allows_ansi(Some(OsStr::new("dumb")), None));
    }

    #[test]
    fn prompts_require_human_mode_and_terminal_input_and_output() {
        assert!(Print::new(false, false).output_allows_prompt());
        assert!(!Print::quiet().output_allows_prompt());
        assert!(!Print::new(false, true).output_allows_prompt());
        assert!(terminals_allow_prompt(true, true));
        assert!(!terminals_allow_prompt(false, true));
        assert!(!terminals_allow_prompt(true, false));
    }

    #[test]
    fn confirmation_accepts_only_y_or_yes() {
        assert!(confirmation_answer("y\n"));
        assert!(confirmation_answer(" YES "));
        assert!(!confirmation_answer(""));
        assert!(!confirmation_answer("no"));
    }

    #[test]
    fn wizard_defaults_apply_only_after_interactive_input() {
        assert!(wizard_answer("\n", true, true));
        assert!(!wizard_answer("", true, false));
        assert!(!wizard_answer("n\n", true, true));
        assert!(wizard_answer("yes\n", false, true));
    }

    #[test]
    fn quiet_wizard_questions_use_their_default() {
        let print = Print::quiet();

        assert!(print.ask("Create key?", true).unwrap());
        assert!(!print.ask("Create project?", false).unwrap());
    }
}
