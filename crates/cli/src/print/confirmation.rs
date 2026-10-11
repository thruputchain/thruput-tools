//! A confirmation owns only the preview and question it prints, never the screen.

use super::{Outcome, Print, report::Report};
#[cfg(unix)]
use crate::terminal::{stderr_height, stderr_width};
use crate::{CliError, terminal::terminal_safe};
use unicode_width::UnicodeWidthStr;

impl Print {
    /// Present an existing review and confirmation as a transient foreground region.
    /// Rendering must be side-effect free; authorization still uses `confirm` rules.
    pub fn confirm_review(
        &self,
        title: &str,
        force: bool,
        action: &'static str,
        question: &str,
        render: impl FnOnce(&Self),
    ) -> Result<bool, CliError> {
        self.require_confirmation_available(force, action)?;
        if !self.has_human_ui() {
            render(self);
            return self.confirm(force, action, question);
        }
        let content = self.collect_group(render);
        let mut report = Report::new(self.output_policy());
        report.outcome_card(Outcome::Information, title, &content.fields);
        report.warnings(&content.warnings);
        let (approved, erased) = run_recorded(self, force, action, question, Some(report))?;
        if erased {
            for warning in content.warnings {
                self.defer_warning(warning);
            }
        }
        Ok(approved)
    }
}

pub(super) fn run(
    print: &Print,
    force: bool,
    action: &'static str,
    message: &str,
    preview: Option<Report>,
) -> Result<bool, CliError> {
    run_recorded(print, force, action, message, preview).map(|(approved, _)| approved)
}

fn run_recorded(
    print: &Print,
    force: bool,
    action: &'static str,
    message: &str,
    preview: Option<Report>,
) -> Result<(bool, bool), CliError> {
    print.require_confirmation_available(force, action)?;
    let prompt = format!("{} [y/N] ", terminal_safe(message));
    let mut policy = print.output_policy();
    policy.color = false;
    let mut measured = Report::new(policy);
    measured.prompt(&prompt);
    let lines = measured.into_lines();
    let rows = preview.as_ref().map_or(0, Report::line_count) + lines.len().saturating_sub(1);
    let last_width = lines
        .last()
        .map_or(0, |line| UnicodeWidthStr::width(line.as_str()));
    let region = if force {
        None
    } else {
        Region::new(print, rows)
    };
    if let Some(preview) = preview {
        print.write_report(preview);
    }
    if force {
        return Ok((true, false));
    }
    let answer = print.read_line(&prompt)?;
    let approved = super::confirmation_answer(&answer);
    let erased = approved && region.is_some_and(|region| region.erase(print, &answer, last_width));
    Ok((approved, erased))
}

#[cfg(unix)]
struct Region {
    size: (usize, usize),
    rows: usize,
    invalidated: std::sync::Arc<std::sync::atomic::AtomicBool>,
    signals: Vec<signal_hook::SigId>,
}

#[cfg(unix)]
impl Region {
    fn new(print: &Print, rows: usize) -> Option<Self> {
        if !print.can_refresh_screen() || !canonical_echo() {
            return None;
        }
        let size = (stderr_width()?, stderr_height()?);
        if rows + 2 >= size.1 {
            return None;
        }
        let invalidated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut region = Self {
            size,
            rows,
            invalidated,
            signals: Vec::new(),
        };
        for signal in [libc::SIGWINCH, libc::SIGCONT] {
            region
                .signals
                .push(signal_hook::flag::register(signal, region.invalidated.clone()).ok()?);
        }
        Some(region)
    }

    fn erase(&self, print: &Print, answer: &str, prompt_width: usize) -> bool {
        use ratatui::crossterm::{
            cursor::{MoveToColumn, MoveUp},
            queue,
            terminal::{Clear, ClearType},
        };
        use std::{io::Write, sync::atomic::Ordering};
        let Some(answer) = answer.strip_suffix('\n') else {
            return false;
        };
        // Tabs/control sequences have terminal-dependent echo widths. Keep those
        // confirmations in scrollback rather than guessing where they ended.
        if !answer
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b' ')
            || self.invalidated.load(Ordering::Relaxed)
            || !print.can_refresh_screen()
            || !canonical_echo()
            || (stderr_width(), stderr_height()) != (Some(self.size.0), Some(self.size.1))
        {
            return false;
        }
        let rows = self.rows + 1 + (prompt_width + answer.len()).saturating_sub(1) / self.size.0;
        if rows >= self.size.1 {
            return false;
        }
        let Ok(rows) = u16::try_from(rows) else {
            return false;
        };
        let mut stderr = std::io::stderr().lock();
        queue!(
            stderr,
            MoveUp(rows),
            MoveToColumn(0),
            Clear(ClearType::FromCursorDown)
        )
        .and_then(|()| stderr.flush())
        .is_ok()
    }
}

#[cfg(unix)]
impl Drop for Region {
    fn drop(&mut self) {
        for signal in self.signals.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
    }
}

#[cfg(unix)]
fn canonical_echo() -> bool {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr initializes this buffer on success; it does not change input.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, attributes.as_mut_ptr()) } != 0 {
        return false;
    }
    let attributes = unsafe { attributes.assume_init() };
    attributes.c_lflag & (libc::ICANON | libc::ECHO | libc::ECHOCTL)
        == (libc::ICANON | libc::ECHO | libc::ECHOCTL)
        && attributes.c_oflag & (libc::OPOST | libc::ONLCR) == (libc::OPOST | libc::ONLCR)
}

#[cfg(not(unix))]
struct Region;
#[cfg(not(unix))]
impl Region {
    fn new(_: &Print, _: usize) -> Option<Self> {
        None
    }
    fn erase(&self, _: &Print, _: &str, _: usize) -> bool {
        false
    }
}
