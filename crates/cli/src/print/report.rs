//! Reusable static components built from Ratatui widgets.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Style,
    symbols::border,
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::{
    policy::Policy,
    render::{PAGE_ROWS, Renderer, paragraph, safe_line},
    theme::Outcome,
};
use crate::{
    checks::{Finding, FindingLevel},
    terminal::terminal_safe,
};

pub(super) struct Report {
    pub(super) renderer: Renderer,
    summary_width: Option<u16>,
}

impl Report {
    pub fn new(policy: Policy) -> Self {
        Self {
            renderer: Renderer::new(policy),
            summary_width: None,
        }
    }

    pub fn into_lines(self) -> Vec<String> {
        self.renderer.lines
    }

    pub fn line_count(&self) -> usize {
        self.renderer.lines.len()
    }

    pub fn outcome_card(&mut self, outcome: Outcome, title: &str, items: &[(String, String)]) {
        if outcome == Outcome::Information {
            self.card(title, items);
            return;
        }
        let prefix = outcome.prefix(self.renderer.policy.unicode);
        self.card(&format!("{prefix} {title}"), items);
    }

    pub fn card(&mut self, title: &str, items: &[(String, String)]) {
        self.card_with_headings(title, items, &[]);
    }

    pub fn card_with_headings(
        &mut self,
        title: &str,
        items: &[(String, String)],
        headings: &[usize],
    ) {
        let title_style = self.renderer.theme.title();
        self.summary_width = None;
        let title = terminal_safe(title);
        let items: Vec<_> = items
            .iter()
            .map(|(label, value)| (terminal_safe(label), terminal_safe(value)))
            .collect();
        let label_width = items
            .iter()
            .map(|(label, _)| UnicodeWidthStr::width(label.as_str()))
            .max()
            .unwrap_or(0);
        let value_width = items
            .iter()
            .map(|(_, value)| UnicodeWidthStr::width(value.as_str()))
            .max()
            .unwrap_or(0);
        let natural =
            (label_width + value_width.max(18) + 6).max(UnicodeWidthStr::width(title.as_str()) + 5);
        let width = self
            .renderer
            .policy
            .width
            .map(|width| width.min(u16::try_from(natural).unwrap_or(u16::MAX)));
        let Some(width) = width.filter(|width| {
            usize::from(*width) >= (UnicodeWidthStr::width(title.as_str()) + 5).max(8)
        }) else {
            self.plain_card(&title, &items, title_style, headings);
            return;
        };
        let content_width = width - 4;
        let label_width = u16::try_from(label_width).unwrap_or(u16::MAX);
        let value_budget = content_width.saturating_sub(label_width.saturating_add(2));
        // Choose a complete-data fallback before emitting a border if a field exceeds
        // Ratatui's paragraph scroll range. Normal/tall reports render in bounded pages.
        if items.iter().any(|(label, value)| {
            paragraph(label.as_str()).line_count(content_width) > usize::from(u16::MAX)
                || paragraph(value.as_str()).line_count(value_budget.max(2)) > usize::from(u16::MAX)
        }) {
            self.plain_card(&title, &items, title_style, headings);
            return;
        }
        self.edge(&title, width, true, title_style);
        for (index, (label, value)) in items.iter().enumerate() {
            if label.is_empty() {
                let style = if headings.contains(&index) {
                    self.renderer.theme.label()
                } else {
                    Style::default()
                };
                self.body(&paragraph(safe_line(value, style)), width, 0);
            } else if value_budget >= 18 {
                let label = paragraph(safe_line(&format!("{label}:"), self.renderer.theme.label()));
                let value = paragraph(value.as_str());
                self.field(&label, &value, label_width + 2, width);
            } else if UnicodeWidthStr::width(label.as_str())
                + 2
                + UnicodeWidthStr::width(value.as_str())
                <= usize::from(content_width)
            {
                self.body(
                    &paragraph(Line::from(vec![
                        Span::styled(format!("{label}: "), self.renderer.theme.label()),
                        Span::raw(value.clone()),
                    ])),
                    width,
                    0,
                );
            } else {
                self.body(
                    &paragraph(safe_line(&format!("{label}:"), self.renderer.theme.label())),
                    width,
                    0,
                );
                self.body(&paragraph(value.as_str()), width, 2);
            }
        }
        self.edge("", width, false, title_style);
    }

    fn plain_card(
        &mut self,
        title: &str,
        items: &[(String, String)],
        title_style: Style,
        headings: &[usize],
    ) {
        self.renderer
            .text(Text::from(safe_line(title, title_style)), 0);
        self.renderer.lines.push(String::new());
        for (index, (label, value)) in items.iter().enumerate() {
            if label.is_empty() {
                let style = if headings.contains(&index) {
                    self.renderer.theme.label()
                } else {
                    Style::default()
                };
                self.renderer.text(Text::from(safe_line(value, style)), 0);
            } else if self.renderer.policy.width.is_some() {
                self.renderer.text(Text::from(format!("{label}:")), 0);
                self.renderer.text(Text::from(value.clone()), 2);
            } else {
                self.renderer.lines.push(format!("{label}: {value}"));
            }
        }
    }

    fn frame(&self, borders: Borders) -> Block<'static> {
        let block = Block::default()
            .borders(borders)
            .border_type(BorderType::Rounded)
            .border_style(self.renderer.theme.border());
        if self.renderer.policy.unicode {
            block
        } else {
            block.border_set(border::Set {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                vertical_left: "|",
                vertical_right: "|",
                horizontal_top: "-",
                horizontal_bottom: "-",
            })
        }
    }

    fn edge(&mut self, title: &str, width: u16, top: bool, title_style: Style) {
        if !top {
            self.summary_width = Some(width);
        }
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        let borders =
            Borders::LEFT | Borders::RIGHT | if top { Borders::TOP } else { Borders::BOTTOM };
        let mut block = self.frame(borders);
        if top {
            let lead = if self.renderer.policy.unicode {
                "─"
            } else {
                "-"
            };
            block = block.title(Line::styled(format!("{lead} {title} "), title_style));
        }
        block.render(area, &mut buffer);
        self.renderer.append(&buffer);
    }

    fn body(&mut self, text: &Paragraph<'_>, width: u16, indent: u16) {
        let height = text.line_count(width - 4 - indent).max(1);
        for start in (0..height).step_by(usize::from(PAGE_ROWS)) {
            let rows = page_height(height - start);
            let area = Rect::new(0, 0, width, rows);
            let mut buffer = Buffer::empty(area);
            self.frame(Borders::LEFT | Borders::RIGHT)
                .render(area, &mut buffer);
            text.clone()
                .scroll((u16::try_from(start).unwrap_or(u16::MAX), 0))
                .render(
                    Rect::new(2 + indent, 0, width - 4 - indent, rows),
                    &mut buffer,
                );
            self.renderer.append(&buffer);
        }
    }

    fn field(
        &mut self,
        label: &Paragraph<'_>,
        value: &Paragraph<'_>,
        label_width: u16,
        width: u16,
    ) {
        let height = value.line_count(width - 4 - label_width).max(1);
        for start in (0..height).step_by(usize::from(PAGE_ROWS)) {
            let rows = page_height(height - start);
            let area = Rect::new(0, 0, width, rows);
            let mut buffer = Buffer::empty(area);
            self.frame(Borders::LEFT | Borders::RIGHT)
                .render(area, &mut buffer);
            let [left, right] =
                Layout::horizontal([Constraint::Length(label_width), Constraint::Fill(1)])
                    .areas(Rect::new(2, 0, width - 4, rows));
            if start == 0 {
                label.clone().render(left, &mut buffer);
            }
            value
                .clone()
                .scroll((u16::try_from(start).unwrap_or(u16::MAX), 0))
                .render(right, &mut buffer);
            self.renderer.append(&buffer);
        }
    }

    pub fn findings<'a>(&mut self, findings: impl Iterator<Item = &'a Finding>) {
        for finding in findings {
            self.renderer.lines.push(String::new());
            let outcome = match finding.level {
                FindingLevel::Warn => Outcome::Warning,
                FindingLevel::Info => Outcome::Information,
            };
            if self.renderer.policy.width.is_some() {
                self.message(
                    outcome,
                    &finding.message,
                    Some(finding.code),
                    finding.hint.as_deref(),
                );
            } else {
                let message = if finding.level == FindingLevel::Warn {
                    format!("{}: {}", finding.code, finding.message)
                } else {
                    finding.message.clone()
                };
                self.message(outcome, &message, None, finding.hint.as_deref());
            }
        }
    }

    pub fn warnings(&mut self, warnings: &[String]) {
        for warning in warnings {
            self.renderer.lines.push(String::new());
            self.message(Outcome::Warning, warning, None, None);
        }
    }

    pub fn message(
        &mut self,
        outcome: Outcome,
        message: &str,
        code: Option<&str>,
        hint: Option<&str>,
    ) {
        if self.renderer.policy.width.is_some()
            && (code.is_some()
                || hint.is_some()
                || matches!(outcome, Outcome::Warning | Outcome::Error))
        {
            let width = self.renderer.policy.width;
            self.renderer.policy.width = self.summary_width.or(width);
            self.diagnostic(outcome, message, code, hint);
            self.renderer.policy.width = width;
            return;
        }
        let prefix = outcome.prefix(self.renderer.policy.unicode);
        self.renderer.text(
            Text::from(Line::from(vec![
                Span::styled(format!("{prefix} "), self.renderer.theme.outcome(outcome)),
                Span::raw(terminal_safe(message)),
            ])),
            0,
        );
        if let Some(hint) = hint {
            self.renderer.text(
                Text::from(safe_line(&format!("hint: {hint}"), Style::default())),
                2,
            );
        }
    }

    pub fn compiler_diagnostic(&mut self, outcome: Outcome, text: &str, code: Option<&str>) {
        self.renderer.lines.push(String::new());
        let width = self.renderer.policy.width;
        self.renderer.policy.width = self.summary_width.or(width);
        let has_excerpt = text.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("-->")
                || line.starts_with(":::")
                || line.split_once('|').is_some_and(|(number, _)| {
                    let number = number.trim();
                    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                })
        });
        let mut lines = text.lines();
        self.diagnostic(outcome, lines.next().unwrap_or_default(), code, None);
        let mut note_indent = None;
        for line in lines {
            if !matches!(outcome, Outcome::Warning | Outcome::Error) {
                self.renderer.text(Text::from(terminal_safe(line)), 2);
                continue;
            }
            if let Some(note) = line.trim_start().strip_prefix("= note: ") {
                note_indent = Some(line.len() - note.len());
                self.annotation("Note", note);
                continue;
            }
            if let Some(indent) = note_indent
                && let Some(continuation) = line.strip_prefix(&" ".repeat(indent))
            {
                self.renderer
                    .text(Text::from(terminal_safe(continuation)), 4);
                continue;
            }
            note_indent = None;
            if !has_excerpt && line.trim() == "|" {
                continue;
            }
            self.renderer.text(Text::from(terminal_safe(line)), 2);
        }
        self.renderer.policy.width = width;
    }

    fn diagnostic(
        &mut self,
        outcome: Outcome,
        message: &str,
        code: Option<&str>,
        hint: Option<&str>,
    ) {
        let mut heading = vec![Span::styled(
            outcome.heading(self.renderer.policy.unicode),
            self.renderer.theme.outcome_title(outcome),
        )];
        if let Some(code) = code {
            let separator = if self.renderer.policy.unicode {
                " · "
            } else {
                " - "
            };
            heading.push(Span::styled(
                format!("{separator}{}", terminal_safe(code)),
                self.renderer.theme.label(),
            ));
        }
        self.renderer.text(Text::from(Line::from(heading)), 0);
        self.renderer.text(Text::from(terminal_safe(message)), 2);
        if let Some(hint) = hint {
            self.annotation("Hint", hint);
        }
    }

    fn annotation(&mut self, label: &str, message: &str) {
        let arrow = if self.renderer.policy.unicode {
            "↳"
        } else {
            ">"
        };
        self.renderer.text(
            Text::from(safe_line(
                &format!("{arrow} {label}"),
                self.renderer.theme.hint(),
            )),
            2,
        );
        self.renderer.text(Text::from(terminal_safe(message)), 4);
    }

    pub fn next_steps(&mut self, steps: &[String]) {
        if steps.is_empty() {
            return;
        }
        self.renderer.lines.push(String::new());
        self.renderer.text(
            Text::from(safe_line("Next:", self.renderer.theme.label())),
            0,
        );
        for step in steps {
            self.renderer.text(Text::from(terminal_safe(step)), 2);
        }
    }

    pub fn detail(&mut self, label: &str, value: &str) {
        self.renderer.text(
            Text::from(Line::from(vec![
                Span::styled(
                    format!("{}: ", terminal_safe(label)),
                    self.renderer.theme.label(),
                ),
                Span::raw(terminal_safe(value)),
            ])),
            2,
        );
    }

    pub fn prompt(&mut self, message: &str) {
        let width = self.renderer.policy.width;
        self.renderer.policy.width = width.map(|width| width.saturating_sub(1).max(1));
        self.renderer
            .text(Text::from(safe_line(message, Style::default())), 0);
        self.renderer.policy.width = width;
        // The caller flushes without a newline so canonical line input stays on this row.
        if let Some(last) = self.renderer.lines.last_mut() {
            *last = last.trim_end().to_string();
            last.push(' ');
        }
    }

    /// Readable records at narrow widths; Ratatui tables when every column has room.
    pub fn table(&mut self, title: &str, headers: &[String], records: &[Vec<String>]) {
        let columns = records
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(headers.len());
        if columns == 0 || records.is_empty() {
            self.card(title, &[("Result".into(), "No results".into())]);
            return;
        }
        let headers: Vec<_> = (0..columns)
            .map(|index| {
                headers.get(index).map_or_else(
                    || format!("Column {}", index + 1),
                    |value| terminal_safe(value),
                )
            })
            .collect();
        let records: Vec<Vec<String>> = records
            .iter()
            .map(|record| {
                (0..columns)
                    .map(|index| {
                        record
                            .get(index)
                            .map_or_else(String::new, |value| terminal_safe(value))
                    })
                    .collect()
            })
            .collect();
        let width = self.renderer.policy.width.unwrap_or(0);
        let column_widths: Vec<_> = (0..columns)
            .map(|column| {
                std::iter::once(&headers[column])
                    .chain(records.iter().map(|record| &record[column]))
                    .map(|value| UnicodeWidthStr::width(value.as_str()))
                    .max()
                    .unwrap_or(0)
                    .max(12)
            })
            .collect();
        let available = usize::from(width.saturating_sub(4)).saturating_sub(columns - 1);
        // Keep addresses and other identifiers together. If the columns do not
        // fit, stacked cards provide more readable wrapping than interleaved cells.
        if column_widths.iter().sum::<usize>() > available
            || UnicodeWidthStr::width(terminal_safe(title).as_str()) + 5 > usize::from(width)
        {
            self.stacked_records(title, &headers, &records);
            return;
        }
        let column_widths: Vec<_> = column_widths
            .into_iter()
            .map(|width| u16::try_from(width).unwrap_or(12))
            .collect();
        self.edge(
            &terminal_safe(title),
            width,
            true,
            self.renderer.theme.title(),
        );
        for (index, record) in std::iter::once(&headers).chain(records.iter()).enumerate() {
            let cells: Vec<_> = record
                .iter()
                .zip(&column_widths)
                .map(|(value, &width)| wrapped_cell(value, width))
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for start in (0..height).step_by(usize::from(PAGE_ROWS)) {
                let rows = page_height(height - start);
                let table_rows = (start..start + usize::from(rows)).map(|line| {
                    Row::new(
                        cells
                            .iter()
                            .map(|cell| Cell::from(cell.get(line).cloned().unwrap_or_default())),
                    )
                    .style(if index == 0 {
                        self.renderer.theme.label()
                    } else {
                        Style::default()
                    })
                });
                let area = Rect::new(0, 0, width, rows);
                let mut buffer = Buffer::empty(area);
                self.frame(Borders::LEFT | Borders::RIGHT)
                    .render(area, &mut buffer);
                Table::new(
                    table_rows,
                    column_widths.iter().copied().map(Constraint::Length),
                )
                .column_spacing(1)
                .render(Rect::new(2, 0, width - 4, rows), &mut buffer);
                self.renderer.append(&buffer);
            }
        }
        self.edge("", width, false, self.renderer.theme.title());
    }

    fn stacked_records(&mut self, title: &str, headers: &[String], records: &[Vec<String>]) {
        for (index, record) in records.iter().enumerate() {
            if index > 0 {
                self.renderer.lines.push(String::new());
            }
            let fields: Vec<_> = headers
                .iter()
                .cloned()
                .zip(record.iter().cloned())
                .collect();
            self.card(
                &format!("{title} ({}/{})", index + 1, records.len()),
                &fields,
            );
        }
    }
}

fn page_height(remaining: usize) -> u16 {
    u16::try_from(remaining.min(usize::from(PAGE_ROWS))).unwrap_or(PAGE_ROWS)
}

fn wrapped_cell(value: &str, width: u16) -> Vec<String> {
    let mut renderer = Renderer::new(Policy {
        width: Some(width),
        color: false,
        unicode: true,
    });
    renderer.text(Text::from(value.to_owned()), 0);
    renderer.lines
}

#[cfg(test)]
mod tests;
