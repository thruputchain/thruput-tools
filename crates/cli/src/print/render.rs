//! Static cell-buffer output: SGR styling only, with no cursor moves or terminal modes.

use ratatui::{
    backend::IntoCrossterm,
    buffer::Buffer,
    crossterm::style::ContentStyle,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Paragraph, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::{policy::Policy, theme::Theme};

/// Render at most this many rows at once, independent of terminal height.
pub(super) const PAGE_ROWS: u16 = 64;

pub(super) struct Renderer {
    pub policy: Policy,
    pub theme: Theme,
    pub lines: Vec<String>,
}

impl Renderer {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            theme: Theme(policy),
            lines: Vec::new(),
        }
    }

    pub fn append(&mut self, buffer: &Buffer) {
        self.lines.extend(buffer_lines(buffer, self.policy.color));
    }

    pub fn text(&mut self, text: Text<'_>, indent: u16) {
        let Some(width) = self.policy.width else {
            for line in text.lines {
                self.lines
                    .push(format!("{}{line}", " ".repeat(usize::from(indent))));
            }
            return;
        };
        let indent = indent.min(width.saturating_sub(2));
        let paragraph = Paragraph::new(text.clone()).wrap(Wrap { trim: false });
        let height = paragraph.line_count(width - indent).max(1);
        if height > usize::from(u16::MAX) {
            // Ratatui paragraph offsets are u16. Preserve extreme diagnostic payloads
            // as plain terminal text rather than truncate them or wrap the offset.
            for line in text.lines {
                self.lines.push(line.to_string());
            }
            return;
        }
        for start in (0..height).step_by(usize::from(PAGE_ROWS)) {
            let rows =
                u16::try_from((height - start).min(usize::from(PAGE_ROWS))).unwrap_or(PAGE_ROWS);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, rows));
            paragraph
                .clone()
                .scroll((u16::try_from(start).unwrap_or(u16::MAX), 0))
                .render(Rect::new(indent, 0, width - indent, rows), &mut buffer);
            self.append(&buffer);
        }
    }
}

/// Emit visible cell runs. Continuation cells of wide graphemes are not spaces.
pub(super) fn buffer_lines(buffer: &Buffer, color: bool) -> Vec<String> {
    let area = buffer.area;
    let mut lines = Vec::new();
    for y in area.y..area.bottom() {
        let mut line = String::new();
        let mut run = String::new();
        let mut style = Style::default();
        let mut x = area.x;
        while x < area.right() {
            let cell = &buffer[(x, y)];
            let mut next_style = if color {
                cell.style()
            } else {
                Style::default()
            };
            // StyledContent resets each styled run. Default cells need no explicit
            // reset escapes, which would otherwise decorate every plain span.
            if next_style.fg == Some(Color::Reset) {
                next_style.fg = None;
            }
            if next_style.bg == Some(Color::Reset) {
                next_style.bg = None;
            }
            if next_style.underline_color == Some(Color::Reset) {
                next_style.underline_color = None;
            }
            if next_style != style {
                flush_run(&mut line, &mut run, style, color);
                style = next_style;
            }
            run.push_str(cell.symbol());
            x = x.saturating_add(
                u16::try_from(UnicodeWidthStr::width(cell.symbol()).max(1)).unwrap_or(1),
            );
        }
        flush_run(&mut line, &mut run, style, color);
        lines.push(line);
    }
    lines
}

fn flush_run(line: &mut String, run: &mut String, style: Style, color: bool) {
    if run.is_empty() {
        return;
    }
    if color && style != Style::default() {
        let style: ContentStyle = style.into_crossterm();
        line.push_str(&style.apply(run.as_str()).to_string());
    } else {
        line.push_str(run);
    }
    run.clear();
}

pub(super) fn paragraph<'a>(text: impl Into<Text<'a>>) -> Paragraph<'a> {
    Paragraph::new(text).wrap(Wrap { trim: false })
}

pub(super) fn safe_line(text: &str, style: Style) -> Line<'static> {
    Line::styled(crate::terminal::terminal_safe(text), style)
}
