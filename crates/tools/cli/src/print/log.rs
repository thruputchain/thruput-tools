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
    for (label, outcome) in [("warning", Outcome::Warning), ("error", Outcome::Error)] {
        if let Some(suffix) = content.strip_prefix(label) {
            let heading = if suffix.starts_with(':') {
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
    ]
    .iter()
    .any(|prefix| {
        content
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with(' '))
    })
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
        ] {
            let rendered = line(text, theme(true));
            assert_eq!(rendered.to_string(), text);
            assert!(rendered.style.add_modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn only_diagnostic_headings_receive_severity_color() {
        for (text, color) in [
            ("warning: something needs attention", Color::Yellow),
            ("  warning[unused_imports]: unused import", Color::Yellow),
            ("error[E0425]: cannot find value", Color::Red),
            ("error: could not compile", Color::Red),
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
