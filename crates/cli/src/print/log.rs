//! Cosmetic log styling only. Unknown lines and diagnostic bodies stay readable;
//! classification never affects command outcomes or rewrites the original text.

use ratatui::{
    backend::IntoCrossterm,
    crossterm::style::ContentStyle,
    text::{Line, Span},
};

use super::theme::{Outcome, Theme};

pub(super) fn line(text: &str, theme: Theme) -> Line<'_> {
    let content = text.trim_start();
    let indent = text.len() - content.len();
    for (label, outcome) in [
        ("warning", Outcome::Warning),
        ("error", Outcome::Error),
        ("WARN", Outcome::Warning),
        ("ERROR", Outcome::Error),
    ] {
        if let Some(suffix) = content.strip_prefix(label) {
            let heading = if matches!(label, "WARN" | "ERROR") && suffix.starts_with(' ') {
                Some(label.len())
            } else if suffix.starts_with(':') {
                Some(label.len() + 1)
            } else if suffix.starts_with('[') {
                suffix.find("]:").map(|end| label.len() + end + 2)
            } else {
                None
            };
            if let Some(heading) = heading {
                let end = indent + heading;
                return Line::from(vec![
                    Span::raw(&text[..indent]),
                    Span::styled(&text[indent..end], theme.outcome(outcome)),
                    Span::raw(&text[end..]),
                ]);
            }
        }
    }
    if is_routine(text) {
        Line::styled(text, theme.routine_log())
    } else {
        Line::raw(text)
    }
}

pub(super) fn is_routine(text: &str) -> bool {
    let content = text.trim_start();
    [
        "Updating",
        "Locking",
        "Adding",
        "Downloading",
        "Downloaded",
        "Compiling",
        "Checking",
        "Fresh",
        "Finished",
        "Wrote",
        "INFO",
        "DEBUG",
        "TRACE",
    ]
    .iter()
    .any(|prefix| {
        content
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with(' '))
    })
}

/// Buffers one tracing record so styles and UTF-8 are not split across writes.
#[derive(Debug)]
pub(crate) struct Writer {
    policy: super::policy::Policy,
    bytes: Vec<u8>,
}

impl Writer {
    pub(super) fn new(policy: super::policy::Policy) -> Self {
        Self {
            policy,
            bytes: Vec::new(),
        }
    }
}

impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let bytes = std::mem::take(&mut self.bytes);
        let mut stderr = std::io::stderr().lock();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let safe = crate::terminal::terminal_safe(line);
            stderr.write_all(append_line(&safe, Theme(self.policy)).as_bytes())?;
        }
        stderr.flush()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.flush();
    }
}

/// Use the same styles before the inline viewport starts and after its fallback.
pub(super) fn append_line(text: &str, theme: Theme) -> String {
    let line = line(text, theme);
    let mut output = String::new();
    for span in line.spans {
        let style: ContentStyle = line.style.patch(span.style).into_crossterm();
        if style == ContentStyle::default() {
            output.push_str(&span.content);
        } else {
            output.push_str(&style.apply(span.content).to_string());
        }
    }
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::policy::Policy;
    use ratatui::style::{Color, Modifier, Style};

    fn theme(color: bool) -> Theme {
        Theme(Policy {
            width: Some(80),
            color,
            unicode: true,
        })
    }

    #[test]
    fn routine_lines_are_muted_without_changing_content_or_indentation() {
        for text in [
            "    Updating crates.io index",
            "     Locking 277 packages",
            "      Adding foo v1.0.0",
            " Downloading crates ...",
            "  Downloaded foo v1.0.0",
            "   Compiling foo v1.0.0 (/a long/path)",
            "    Checking foo",
            "       Fresh foo",
            "    Finished `release` profile in 1s",
            "Wrote /a/path/contract.spec.xdr",
            " INFO service started",
            "DEBUG connection opened",
        ] {
            let rendered = line(text, theme(true));
            assert_eq!(rendered.to_string(), text);
            assert!(rendered.style.add_modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn only_diagnostic_headings_receive_severity_color() {
        for (text, color) in [
            (
                "warning: something needs attention",
                Color::Rgb(251, 191, 36),
            ),
            (
                "  warning[unused_imports]: unused import",
                Color::Rgb(251, 191, 36),
            ),
            ("error[E0425]: cannot find value", Color::Rgb(248, 113, 113)),
            ("error: could not compile", Color::Rgb(248, 113, 113)),
            (" WARN service warning", Color::Rgb(251, 191, 36)),
            ("ERROR service failure", Color::Rgb(248, 113, 113)),
        ] {
            let rendered = line(text, theme(true));
            assert_eq!(rendered.to_string(), text);
            assert_eq!(rendered.spans[1].style.fg, Some(color));
            assert_eq!(rendered.spans[2].style, Style::default());
            assert!(!rendered.spans[1].style.add_modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn source_carets_notes_and_unknown_lines_stay_at_normal_brightness() {
        for text in [
            "  12 | let Compiling = 1;",
            "     |     ^^^^^^^^^",
            "     = note: details remain readable",
            "CompilingSomething is an identifier",
            "error[not a heading]",
            "custom build-script output",
            "",
        ] {
            let rendered = line(text, theme(true));
            assert_eq!(rendered.to_string(), text);
            assert_eq!(rendered.style, Style::default());
            assert!(
                rendered
                    .spans
                    .iter()
                    .all(|span| span.style == Style::default())
            );
        }
    }

    #[test]
    fn append_only_styling_respects_no_color() {
        for text in [
            "   Compiling demo",
            "warning: keep visible",
            "error[E0001]: keep visible",
        ] {
            assert_eq!(append_line(text, theme(false)), format!("{text}\n"));
            assert!(append_line(text, theme(true)).contains('\x1b'));
        }
        assert!(append_line("   Compiling demo", theme(true)).contains("[2m"));
    }
}
