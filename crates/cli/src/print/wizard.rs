//! Refreshing views for guided input. Ordinary command results remain append-only.

use super::{Outcome, Print, report::Report};
use crate::{CliError, terminal::terminal_safe};
use ratatui::{
    crossterm::{
        cursor::MoveTo,
        queue,
        terminal::{Clear, ClearType},
    },
    text::{Line, Span, Text},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Display state for one step in a guided command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum StepState {
    #[default]
    Pending,
    Current,
    Done,
    Skipped,
}

impl StepState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Current => "current",
            Self::Done => "done",
            Self::Skipped => "skipped",
        }
    }

    fn marker(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::Pending, true) => "○",
            (Self::Pending, false) => "o",
            (Self::Current, true) => "●",
            (Self::Current, false) => ">",
            (Self::Done, true) => "✓",
            (Self::Done, false) => "+",
            (Self::Skipped, _) => "-",
        }
    }
}

impl Print {
    /// Guided input may refresh only an exclusive, supported foreground terminal.
    pub(crate) fn can_refresh_screen(&self) -> bool {
        self.can_prompt() && super::progress::eligible(self, self.output_policy())
    }

    /// Show all steps, then let the caller use the normal canonical line prompts.
    /// Short terminals get a compact heading or an append-only fallback.
    pub(crate) fn wizard_view(
        &self,
        title: &str,
        subtitle: &str,
        steps: &[(&str, StepState)],
    ) -> Result<bool, CliError> {
        if !self.can_refresh_screen() {
            return Ok(false);
        }
        let Some(height) = crate::terminal::stderr_height() else {
            return Ok(false);
        };
        let policy = self.output_policy();
        for compact in [false, true] {
            let mut report = Report::new(policy);
            report.wizard(title, subtitle, steps, compact);
            let lines = report.into_lines();
            // Leave room for the current explanation and an editable question.
            if lines.len() + 4 <= height {
                write_screen(&lines)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// End an active guided view before writing its final result and warnings.
    pub(crate) fn clear_guided_view(&self) -> Result<(), CliError> {
        if self.can_refresh_screen() {
            write_screen(&[])?;
        }
        Ok(())
    }
}

fn write_screen(lines: &[String]) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    // Clear the visible display, never the scrollback buffer (ClearType::Purge).
    // No raw mode, alternate screen, hidden cursor, or input interception is used.
    queue!(stderr, Clear(ClearType::All), MoveTo(0, 0))?;
    for line in lines {
        writeln!(stderr, "{line}")?;
    }
    stderr.flush()
}

impl Report {
    pub(super) fn wizard(
        &mut self,
        title: &str,
        subtitle: &str,
        steps: &[(&str, StepState)],
        compact: bool,
    ) {
        if compact {
            self.renderer.text(
                Text::from(Line::styled(
                    terminal_safe(title),
                    self.renderer.theme.title(),
                )),
                0,
            );
        } else {
            self.card(title, &[(String::new(), subtitle.into())]);
        }
        self.renderer.lines.push(String::new());
        self.renderer.text(
            Text::from(Line::styled("Progress", self.renderer.theme.heading())),
            0,
        );
        // Reserve indentation, the marker, a gap, and the seven-column state label.
        let label_width = steps
            .iter()
            .map(|(label, _)| UnicodeWidthStr::width(*label))
            .max()
            .unwrap_or(0)
            .max(16)
            .min(
                self.renderer
                    .policy
                    .width
                    .map_or(usize::MAX, |width| usize::from(width).saturating_sub(13)),
            );
        for (label, state) in steps {
            let style = match state {
                StepState::Pending | StepState::Skipped => self.renderer.theme.label(),
                StepState::Current => self.renderer.theme.outcome(Outcome::Information),
                StepState::Done => self.renderer.theme.outcome(Outcome::Success),
            };
            let padding = label_width.saturating_sub(UnicodeWidthStr::width(*label)) + 2;
            let line = Line::from(vec![
                Span::styled(
                    format!(
                        "{} {}",
                        state.marker(self.renderer.policy.unicode),
                        terminal_safe(label)
                    ),
                    style,
                ),
                Span::raw(" ".repeat(padding)),
                Span::styled(state.label(), self.renderer.theme.label()),
            ]);
            self.renderer.text(Text::from(line), 2);
        }
        self.renderer.lines.push(String::new());
    }
}
