use crate::CliError;
use crate::terminal::terminal_safe;
use serde::Serialize;
use serde_json::json;
use std::io::{self, IsTerminal, Write};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";

/// Widest summary card, in terminal columns, that value wrapping aims for.
const CARD_MAX_WIDTH: usize = 100;

/// Narrowest value column values are wrapped into.
const CARD_MIN_VALUE_WIDTH: usize = 18;

/// Trails a value line that continues on the next line of a summary card.
const CARD_WRAP_MARKER: char = '↵';

/// Columns a continued value line is indented past the value column.
const CARD_CONTINUATION_INDENT: usize = 2;

/// Controls CLI output. Status messages go to stderr
/// (stdout is reserved for data / cargo passthrough).
#[allow(clippy::struct_excessive_bools)]
pub struct Print {
    quiet: bool,
    json: bool,
    trace_commands: bool,
    non_interactive: bool,
}

impl Print {
    pub fn new(quiet: bool, json: bool) -> Self {
        Self {
            quiet,
            json,
            trace_commands: false,
            non_interactive: false,
        }
    }

    /// A `Print` that suppresses all status messages.
    pub fn quiet() -> Self {
        Self {
            quiet: true,
            json: false,
            trace_commands: false,
            non_interactive: false,
        }
    }

    /// Disable interactive input and terminal rendering without suppressing text.
    #[must_use]
    pub fn with_non_interactive(mut self, non_interactive: bool) -> Self {
        self.non_interactive = non_interactive;
        self
    }

    /// Whether the caller explicitly disabled interactive terminal features.
    pub fn is_non_interactive(&self) -> bool {
        self.non_interactive
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

    /// Returns whether ordinary status output is suppressed.
    pub fn is_quiet(&self) -> bool {
        self.quiet
    }

    /// Returns whether this invocation may safely read an interactive answer.
    pub fn can_prompt(&self) -> bool {
        self.allows_interaction()
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
        let answer = self.read_line(format_args!("{message} [y/N] "))?;
        Ok(confirmation_answer(&answer))
    }

    /// Ask an optional wizard question when interactive input is available.
    pub fn ask(
        &self,
        message: impl std::fmt::Display,
        default_yes: bool,
    ) -> Result<bool, CliError> {
        if !self.can_prompt() {
            return Ok(false);
        }

        let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
        let answer = self.read_line(format_args!("{message} {suffix} "))?;
        Ok(wizard_answer(&answer, default_yes, !answer.is_empty()))
    }

    fn allows_interaction(&self) -> bool {
        !self.quiet && !self.json && !self.non_interactive
    }

    /// Returns whether styled interactive output is appropriate.
    pub fn is_human_tty(&self) -> bool {
        self.allows_interaction() && supports_ansi_stderr()
    }

    /// Returns whether styled human data on stdout is appropriate.
    pub fn is_human_stdout_tty(&self) -> bool {
        self.allows_interaction()
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
        eprintln!("+ {}", render_command(command));
    }

    /// Informational status message (suppressed by --quiet).
    pub fn status(&self, msg: impl std::fmt::Display) {
        if !self.quiet && !self.json {
            eprintln!("{msg}");
        }
    }

    /// Write one human-readable data row to stdout.
    ///
    /// Data remains visible under `--quiet`; JSON commands should use [`Self::json`].
    pub fn data_line(&self, value: impl std::fmt::Display) -> Result<(), CliError> {
        if self.json {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{value}")?;
        Ok(())
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
        let label = terminal_safe(label);
        let value = terminal_safe(&value.to_string());
        if self.is_human_tty() {
            eprintln!("  {DIM}{label}:{RESET} {value}");
        } else {
            self.status(format_args!("  {label}: {value}"));
        }
    }

    /// Render a compact label/value summary for human output.
    pub fn summary_card(&self, title: &str, items: &[(String, String)]) {
        if self.quiet || self.json {
            return;
        }

        let title = terminal_safe(title);
        let items = items
            .iter()
            .map(|(label, value)| (terminal_safe(label), terminal_safe(value)))
            .collect::<Vec<_>>();

        if self.is_human_tty() {
            render_summary_card_tty(&title, &items);
        } else {
            self.status(&title);
            self.status("");
            for (label, value) in items {
                self.status(format_args!("{label}: {value}"));
            }
        }
    }

    /// Render a conventional `Next:` block for human output.
    pub fn next_steps(&self, steps: &[String]) {
        if self.quiet || self.json {
            return;
        }

        self.status("");
        self.status("Next:");
        for step in steps {
            self.status(format_args!("  {step}"));
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

    /// Write a prompt only when interactive input is available.
    pub fn prompt(&self, msg: impl std::fmt::Display) -> Result<(), CliError> {
        self.require_interactive_input()?;
        let mut stderr = io::stderr();
        write!(stderr, "{msg}")?;
        stderr.flush()?;
        Ok(())
    }

    fn require_interactive_input(&self) -> Result<(), CliError> {
        if self.can_prompt() {
            Ok(())
        } else {
            Err(CliError::InteractiveInputRequired)
        }
    }

    /// Ask for a line of terminal input under the shared interaction policy.
    pub fn read_line(&self, message: impl std::fmt::Display) -> Result<String, CliError> {
        self.prompt(message)?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        Ok(answer)
    }

    /// Read hidden terminal input under the same policy as ordinary prompts.
    pub fn read_secret(&self, message: &str) -> Result<zeroize::Zeroizing<String>, CliError> {
        self.require_interactive_input()?;
        Ok(zeroize::Zeroizing::new(rpassword::prompt_password(
            message,
        )?))
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
        let message = terminal_safe(&error.to_string());
        if self.is_human_tty() {
            writeln!(stderr, "{RED}✗{RESET} {message}")?;
        } else {
            writeln!(stderr, "error: {message}")?;
        }
        if let Some(hint) = error.hint() {
            let hint = terminal_safe(&hint);
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

fn supports_ansi_stderr() -> bool {
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

fn render_summary_card_tty(title: &str, items: &[(String, String)]) {
    for line in summary_card_lines(title, items, crate::terminal::stderr_width()) {
        eprintln!("{line}");
    }
}

/// Build the styled lines of a summary card.
///
/// Values wider than the remaining column budget are wrapped so the card stays
/// within [`CARD_MAX_WIDTH`] columns, with each continued line marked by
/// [`CARD_WRAP_MARKER`]. The title and labels are never wrapped, so a card whose
/// label column alone exceeds the budget may be wider. If the resulting box
/// cannot fit the terminal (or its width is unknown), emit unboxed values.
fn summary_card_lines(
    title: &str,
    items: &[(String, String)],
    terminal_width: Option<usize>,
) -> Vec<String> {
    let title_width = display_width(title);
    let label_width = items
        .iter()
        .map(|(label, _)| display_width(label))
        .max()
        .unwrap_or(0)
        .max(10)
        .max(title_width);
    // Borders, padding, and the label's colon take 6 columns of every row.
    let value_budget = CARD_MAX_WIDTH
        .saturating_sub(label_width + 6)
        .max(CARD_MIN_VALUE_WIDTH);
    let value_width = items
        .iter()
        .map(|(_, value)| display_width(value))
        .max()
        .unwrap_or(0)
        .max(CARD_MIN_VALUE_WIDTH)
        .min(value_budget);
    let continued_width = value_width.saturating_sub(CARD_CONTINUATION_INDENT);
    let inner_width = (label_width + value_width + 4).max(title_width + 2);
    if terminal_width.is_none_or(|width| inner_width + 2 > width) {
        let mut lines = vec![title.to_string(), String::new()];
        lines.extend(
            items
                .iter()
                .map(|(label, value)| format!("{label}: {value}")),
        );
        return lines;
    }
    let top_fill = inner_width.saturating_sub(title_width + 2);
    let continuation_label = line_fill(' ', label_width + 1);

    let mut lines = vec![format!(
        "{GREEN}╭─ {BOLD}{title}{RESET}{GREEN}{}╮{RESET}",
        line_fill('─', top_fill)
    )];
    for (label, value) in items {
        let label_padding = line_fill(' ', label_width.saturating_sub(display_width(label)));
        let segments = wrap_value(value, value_width, continued_width);
        let last = segments.len().saturating_sub(1);
        for (index, segment) in segments.iter().enumerate() {
            let cell = value_cell(segment, value_width, index > 0, index < last);
            if index == 0 {
                lines.push(format!(
                    "{GREEN}│{RESET} {DIM}{label}:{RESET}{label_padding} {cell} {GREEN}│{RESET}"
                ));
            } else {
                lines.push(format!(
                    "{GREEN}│{RESET} {continuation_label} {cell} {GREEN}│{RESET}"
                ));
            }
        }
    }
    lines.push(format!("{GREEN}╰{}╯{RESET}", line_fill('─', inner_width)));
    lines
}

/// Columns a string occupies in a terminal.
///
/// Wide characters such as CJK ideographs count as two columns and combining
/// marks as none, so card rows line up on the rendered glyphs rather than on
/// the number of `char`s behind them.
fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

/// Render one value segment padded to the full width of the value column.
///
/// A segment that resumes an earlier line is indented; one that more of the
/// value follows is marked as wrapped.
fn value_cell(segment: &str, width: usize, resumed: bool, more_follows: bool) -> String {
    let indent = if resumed { CARD_CONTINUATION_INDENT } else { 0 };
    let marker = if more_follows {
        format!("{GREEN}{CARD_WRAP_MARKER}{RESET}")
    } else {
        String::new()
    };
    let used = indent + display_width(segment) + usize::from(more_follows);
    format!(
        "{}{segment}{marker}{}",
        line_fill(' ', indent),
        line_fill(' ', width.saturating_sub(used))
    )
}

/// Split a value into segments that fit the value column.
///
/// The first segment fits `first_width` columns and later ones `continued_width`,
/// each leaving room for the wrap marker when more of the value follows. Breaks
/// fall wherever the width runs out; values such as filesystem paths have no
/// reliable break points.
fn wrap_value(value: &str, first_width: usize, continued_width: usize) -> Vec<String> {
    let mut segments = Vec::new();
    let mut rest = value;
    let mut budget = first_width;
    loop {
        if display_width(rest) <= budget {
            segments.push(rest.to_string());
            return segments;
        }
        let (head, tail) = rest.split_at(prefix_within(rest, budget.saturating_sub(1).max(1)));
        segments.push(head.to_string());
        rest = tail;
        budget = continued_width;
    }
}

/// Byte length of the longest prefix of `text` that fits `budget` columns.
///
/// Zero-width characters travel with the character they combine with, so a
/// break never separates a base character from its accents. A leading character
/// wider than the whole budget is still taken, so wrapping always advances.
fn prefix_within(text: &str, budget: usize) -> usize {
    let mut width = 0;
    let mut end = 0;
    for (index, character) in text.char_indices() {
        let character_width = character.width().unwrap_or(0);
        if end > 0 && width + character_width > budget {
            break;
        }
        width += character_width;
        end = index + character.len_utf8();
    }
    end
}

fn line_fill(ch: char, count: usize) -> String {
    std::iter::repeat_n(ch, count).collect()
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
        CARD_CONTINUATION_INDENT, CARD_MAX_WIDTH, CARD_WRAP_MARKER, GREEN, Print, RESET,
        confirmation_answer, display_width, render_command, summary_card_lines,
        terminal_allows_ansi, terminals_allow_prompt, wizard_answer, wrap_value,
    };
    use std::ffi::OsStr;
    use std::process::Command;

    /// A styled line with its ANSI codes removed, as the terminal renders it.
    fn plain(line: &str) -> String {
        let mut characters = line.chars();
        let mut visible = String::new();
        while let Some(character) = characters.next() {
            if character == '\u{1b}' {
                for escape in characters.by_ref() {
                    if escape == 'm' {
                        break;
                    }
                }
            } else {
                visible.push(character);
            }
        }
        visible
    }

    /// Terminal column `needle` starts at on a styled line.
    fn column_of(line: &str, needle: char) -> Option<usize> {
        let line = plain(line);
        let index = line.find(needle)?;
        Some(display_width(&line[..index]))
    }

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
    fn summary_card_falls_back_without_losing_values() {
        let value = "長い/path/".repeat(20);
        let items = vec![("Path".to_string(), value.clone())];
        for width in [None, Some(0), Some(40), Some(80)] {
            assert_eq!(
                summary_card_lines("Status", &items, width),
                vec![
                    "Status".to_string(),
                    String::new(),
                    format!("Path: {value}")
                ]
            );
        }
        assert!(summary_card_lines("Status", &items, Some(100))[0].contains('╭'));
        let items = vec![("Label".repeat(30), "value".to_string())];
        assert!(!summary_card_lines("Status", &items, Some(100))[0].contains('╭'));
    }

    #[test]
    fn summary_card_keeps_boxes_that_fit_exactly() {
        let items = vec![("Key".to_string(), "value".to_string())];
        let lines = summary_card_lines("Status", &items, Some(80));
        let width = display_width(&plain(&lines[0]));
        assert!(lines[0].contains('╭'));
        assert_eq!(summary_card_lines("Status", &items, Some(width)), lines);
        assert!(!summary_card_lines("Status", &items, Some(width - 1))[0].contains('╭'));
    }

    #[test]
    fn summary_card_stays_within_the_maximum_width() {
        let items = vec![
            (
                "Toolchain Cache".to_string(),
                "/home/example/.cache/groundhog/toolchains/hog-devnode/0.17.1/bin/hog-devnode"
                    .to_string(),
            ),
            ("Network".to_string(), "local".to_string()),
        ];

        let widths = summary_card_lines("Groundhog Status", &items, Some(CARD_MAX_WIDTH))
            .iter()
            .map(|line| display_width(&plain(line)))
            .collect::<Vec<_>>();

        assert!(widths.iter().all(|width| *width <= CARD_MAX_WIDTH));
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{widths:?}"
        );
    }

    #[test]
    fn summary_card_indents_wrapped_values_under_a_blank_label() {
        let items = vec![("Binary".to_string(), "x".repeat(200))];

        let lines = summary_card_lines("Node", &items, Some(CARD_MAX_WIDTH));

        let rows = &lines[1..lines.len() - 1];
        assert!(rows.len() > 2);
        let value_column = column_of(&rows[0], 'x');
        assert!(rows[1..].iter().all(|row| !plain(row).contains(':')));
        for row in &rows[1..] {
            assert_eq!(
                column_of(row, 'x'),
                value_column.map(|column| column + CARD_CONTINUATION_INDENT)
            );
        }
        assert!(
            rows[..rows.len() - 1]
                .iter()
                .all(|row| row.contains(CARD_WRAP_MARKER))
        );
        assert!(!rows[rows.len() - 1].contains(CARD_WRAP_MARKER));
    }

    #[test]
    fn summary_card_colors_the_wrap_marker() {
        let items = vec![("Binary".to_string(), "x".repeat(120))];

        let lines = summary_card_lines("Node", &items, Some(CARD_MAX_WIDTH));

        assert!(lines[1].contains(&format!("{GREEN}{CARD_WRAP_MARKER}{RESET}")));
    }

    #[test]
    fn summary_card_aligns_wide_and_combining_characters_by_column() {
        let items = vec![
            (
                "プロジェクト".to_string(),
                "/home/例/ワークスペース/".repeat(6),
            ),
            ("Note".to_string(), "e\u{301}".repeat(90)),
            ("Plain".to_string(), "local".to_string()),
        ];

        let lines = summary_card_lines("状態", &items, Some(CARD_MAX_WIDTH));

        let widths = lines
            .iter()
            .map(|line| display_width(&plain(line)))
            .collect::<Vec<_>>();
        assert!(widths.iter().all(|width| *width <= CARD_MAX_WIDTH));
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{widths:?}"
        );
    }

    #[test]
    fn wrap_value_measures_segments_in_columns_not_characters() {
        // Each ideograph is two columns wide, so only four fit in nine columns
        // once the wrap marker reserves one.
        let segments = wrap_value(&"漢".repeat(8), 9, 9);

        assert_eq!(
            segments,
            vec!["漢漢漢漢".to_string(), "漢漢漢漢".to_string()]
        );
        assert!(segments.iter().all(|segment| display_width(segment) <= 8));
    }

    #[test]
    fn wrap_value_keeps_combining_marks_with_their_base_character() {
        // Accents add no columns, so ten accented letters fit a ten-column line.
        let accented = "e\u{301}".repeat(10);

        assert_eq!(wrap_value(&accented, 10, 8), vec![accented.clone()]);

        let segments = wrap_value(&"a\u{308}".repeat(12), 10, 8);

        assert!(
            segments
                .iter()
                .all(|segment| !segment.starts_with('\u{308}'))
        );
        assert_eq!(segments.concat(), "a\u{308}".repeat(12));
    }

    #[test]
    fn wrap_value_reserves_room_for_the_marker_on_continued_segments() {
        assert_eq!(wrap_value("short", 10, 8), vec!["short".to_string()]);
        assert_eq!(
            wrap_value("exactly-10", 10, 8),
            vec!["exactly-10".to_string()]
        );
        assert_eq!(
            wrap_value("abcdefghijklmn", 5, 4),
            vec![
                "abcd".to_string(),
                "efg".to_string(),
                "hij".to_string(),
                "klmn".to_string(),
            ]
        );
        assert!(
            wrap_value("\u{e9}".repeat(40).as_str(), 12, 10)
                .iter()
                .all(|segment| segment.chars().count() <= 11)
        );
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
        assert!(Print::new(false, false).allows_interaction());
        assert!(!Print::quiet().allows_interaction());
        assert!(!Print::new(false, true).allows_interaction());
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
    fn non_interactive_policy_never_prompts_or_grants_consent() {
        let print = Print::new(false, false).with_non_interactive(true);
        assert!(!print.is_quiet());
        assert!(!print.is_json());
        assert!(!print.can_prompt());
        assert!(!print.is_human_tty());
        assert!(!print.is_human_stdout_tty());
        assert!(!print.ask("Create key?", true).unwrap());
        assert!(matches!(
            print.confirm(false, "test", "Confirm?"),
            Err(crate::CliError::ConfirmationRequired { .. })
        ));
        assert!(print.confirm(true, "test", "Confirm?").unwrap());
        assert!(matches!(
            print.read_line("Input: "),
            Err(crate::CliError::InteractiveInputRequired)
        ));
        assert!(matches!(
            print.read_secret("Secret: "),
            Err(crate::CliError::InteractiveInputRequired)
        ));
    }

    #[test]
    fn quiet_wizard_questions_do_not_authorize_actions() {
        let print = Print::quiet();

        assert!(!print.ask("Create key?", true).unwrap());
        assert!(!print.ask("Create project?", false).unwrap());
    }
}
