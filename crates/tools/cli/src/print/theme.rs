use ratatui::style::{Color, Modifier, Style};

use super::policy::Policy;

// ANSI palette slots can be remapped to unrelated hues by terminal themes.
const SUCCESS: Color = Color::Rgb(79, 232, 143);
const WARNING: Color = Color::Rgb(251, 191, 36);
pub(super) const ERROR_RGB: (u8, u8, u8) = (248, 113, 113);
const ERROR: Color = Color::Rgb(ERROR_RGB.0, ERROR_RGB.1, ERROR_RGB.2);

/// Human outcome labels; these do not determine a command's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Information,
    Warning,
    Error,
    Empty,
    Unchanged,
    DryRun,
    Cancelled,
    Partial,
    Ready,
    Unconfirmed,
}

impl Outcome {
    pub(super) fn heading(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::Warning, true) => "⚠ Warning",
            (Self::Warning, false) => "Warning",
            (Self::Error, true) => "✗ Error",
            (Self::Error, false) => "Error",
            (Self::Information, true) => "• Information",
            (Self::Information, false) => "Information",
            _ => self.prefix(unicode),
        }
    }

    pub(super) fn prefix(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::Success, true) => "✓",
            (Self::Success, false) => "[ok]",
            (Self::Information, true) => "•",
            (Self::Information, false) => "-",
            (Self::Warning, true) => "⚠ warning:",
            (Self::Warning, false) => "warning:",
            (Self::Error, true) => "✗ error:",
            (Self::Error, false) => "error:",
            (Self::Empty, _) => "No results:",
            (Self::Unchanged, _) => "No change:",
            (Self::DryRun, _) => "Dry run (not submitted):",
            (Self::Cancelled, _) => "Cancelled:",
            (Self::Partial, true) => "⚠ Partially completed:",
            (Self::Partial, false) => "Partially completed:",
            (Self::Ready, true) => "✓ Ready:",
            (Self::Ready, false) => "Ready:",
            (Self::Unconfirmed, true) => "⚠ Outcome unconfirmed:",
            (Self::Unconfirmed, false) => "Outcome unconfirmed:",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Success | Self::Ready => SUCCESS,
            Self::Warning | Self::Partial | Self::Unconfirmed => WARNING,
            Self::Error => ERROR,
            _ => Color::Cyan,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Theme(pub Policy);

impl Theme {
    pub fn border(self) -> Style {
        self.style(SUCCESS, Modifier::empty())
    }
    pub fn title(self) -> Style {
        self.style(SUCCESS, Modifier::BOLD)
    }
    pub fn label(self) -> Style {
        self.style(Color::Reset, Modifier::DIM)
    }

    pub fn routine_log(self) -> Style {
        self.label()
    }
    pub fn outcome(self, outcome: Outcome) -> Style {
        self.style(outcome.color(), Modifier::empty())
    }

    pub fn outcome_title(self, outcome: Outcome) -> Style {
        self.style(outcome.color(), Modifier::BOLD)
    }

    pub fn hint(self) -> Style {
        self.style(Color::Cyan, Modifier::BOLD)
    }

    fn style(self, color: Color, modifier: Modifier) -> Style {
        if self.0.color {
            Style::default().fg(color).add_modifier(modifier)
        } else {
            Style::default()
        }
    }
}
