use super::*;
use std::borrow::Cow;

fn policy(width: u16, color: bool) -> Policy {
    Policy {
        width: Some(width.min(100)),
        color,
        unicode: true,
    }
}

fn plain(line: &str) -> String {
    let mut chars = line.chars();
    let mut output = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            assert_eq!(chars.next(), Some('['));
            for ch in chars.by_ref() {
                if ch == 'm' {
                    break;
                }
                assert!(
                    ch.is_ascii_digit() || ch == ';',
                    "only SGR styling is allowed"
                );
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[test]
fn ratatui_card_preserves_columns_at_supported_sizes() {
    let items = vec![
        (
            "Rust Toolchain".into(),
            "nightly-2026-09-01 (rust-src installed)".into(),
        ),
        ("Path".into(), "/workspace/長い/e\u{301}/👩‍💻/".repeat(20)),
        ("Label".repeat(30), "complete value".into()),
    ];
    for width in [24, 40, 60, 80, 100] {
        for color in [false, true] {
            let mut report = Report::new(policy(width, color));
            report.card("Groundhog Status", &items);
            let lines: Vec<_> = report.into_lines().iter().map(|line| plain(line)).collect();
            let card_width = UnicodeWidthStr::width(lines[0].as_str());
            assert!(card_width <= usize::from(width));
            assert!(
                lines
                    .iter()
                    .all(|line| UnicodeWidthStr::width(line.as_str()) == card_width),
                "{lines:?}"
            );
            assert!(lines[0].starts_with("╭─ Groundhog Status"));
            assert!(lines.last().unwrap().ends_with('╯'));
            let content = lines[1..lines.len() - 1]
                .iter()
                .map(|line| line.trim_matches('│').trim())
                .collect::<String>();
            assert!(content.contains(&items[1].1), "{content}");
            assert!(content.contains(&items[2].0));
            assert!(content.contains("complete value"));
        }
    }
}

#[test]
fn tall_card_renders_in_pages_without_clipping_to_terminal_height() {
    let value = "0123456789".repeat(1000);
    let mut report = Report::new(policy(40, false));
    report.card("Tall result", &[("Path".into(), value.clone())]);
    let lines = report.into_lines();
    assert!(lines.len() > usize::from(PAGE_ROWS) * 3);
    let content = lines[1..lines.len() - 1]
        .iter()
        .map(|line| line.trim_matches('│').trim())
        .collect::<String>();
    assert_eq!(content, format!("Path: {value}"));
    assert!(lines.last().unwrap().ends_with('╯'));
}

#[test]
fn card_uses_ratatui_theme_cells_and_crossterm_styling() {
    let mut report = Report::new(policy(80, true));
    report.card("Status", &[("Cargo".into(), "installed".into())]);
    let lines = report.into_lines();
    assert!(lines[0].contains('\x1b'));
    assert!(lines[1].contains("installed"));
    assert!(lines[1].contains("[2m"), "muted labels: {:?}", lines[1]);
    assert!(!lines.join("").contains("?1049"));
    for line in lines {
        plain(&line);
    }
}

#[test]
fn outcome_card_titles_use_severity_color_without_changing_the_border() {
    for (outcome, label, color) in [
        (Outcome::Error, "✗ error:", "38;2;248;113;113m"),
        (
            Outcome::Partial,
            "⚠ Partially completed:",
            "38;2;251;191;36m",
        ),
    ] {
        let mut report = Report::new(policy(80, true));
        report.outcome_card(
            outcome,
            "Deployment",
            &[("Transaction".into(), "0x1234".into())],
        );
        let lines = report.into_lines();
        assert!(plain(&lines[0]).contains(label));
        assert!(lines[0].contains(color));
        assert!(lines.last().unwrap().contains("38;2;79;232;143m"));
    }
}

#[test]
fn modes_separate_layout_color_and_plain_output() {
    let mono = Policy::resolve(
        false,
        true,
        Some("xterm"),
        true,
        Some("en_US.UTF-8"),
        Some(80),
    );
    assert_eq!(mono.width, Some(80));
    assert!(!mono.color);
    assert!(mono.unicode);
    for (no_ui, tty, term) in [
        (true, true, "xterm"),
        (false, false, "xterm"),
        (false, true, "dumb"),
    ] {
        let mode = Policy::resolve(no_ui, tty, Some(term), false, None, Some(80));
        assert!(mode.width.is_none());
        assert!(!mode.color);
    }
    let ascii = Policy::resolve(false, true, Some("xterm"), false, Some("C"), Some(40));
    assert!(!ascii.unicode);
    assert!(
        !Policy::resolve(
            false,
            true,
            Some("xterm"),
            false,
            Some("en_US.ISO8859-1"),
            Some(40)
        )
        .unicode
    );
    let mut report = Report::new(ascii);
    report.card("Status", &[("Cargo".into(), "installed".into())]);
    let lines: Vec<_> = report.into_lines().iter().map(|line| plain(line)).collect();
    assert!(lines[0].starts_with("+- Status"));
    assert!(lines.iter().all(|line| line.is_ascii()));
}

#[test]
fn plain_output_preserves_values_and_has_no_decoration() {
    let value = "/a/".repeat(100);
    let mut report = Report::new(Policy::PLAIN);
    report.card("Status", &[("Path".into(), value.clone())]);
    assert_eq!(
        report.into_lines(),
        [
            "Status".to_string(),
            String::new(),
            format!("Path: {value}")
        ]
    );
}

#[test]
fn warnings_errors_and_outcomes_share_safe_width_aware_components() {
    let outcomes = [
        Outcome::Success,
        Outcome::Information,
        Outcome::Warning,
        Outcome::Error,
        Outcome::Empty,
        Outcome::Unchanged,
        Outcome::DryRun,
        Outcome::Cancelled,
        Outcome::Partial,
        Outcome::Ready,
        Outcome::Unconfirmed,
    ];
    for width in [24, 40, 80] {
        let mut report = Report::new(policy(width, false));
        for outcome in outcomes {
            report.message(
                outcome,
                "A long explanatory message with untrusted \x1b[2J content",
                Some("example_code"),
                Some("run the existing command"),
            );
        }
        report.findings(
            [Finding {
                level: FindingLevel::Warn,
                code: "missing",
                message: "missing tool".into(),
                hint: Some(Cow::Borrowed("install it")),
            }]
            .iter(),
        );
        let lines = report.into_lines();
        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= usize::from(width))
        );
        let content = lines.join("\n");
        assert!(!content.contains('\x1b'));
        assert!(content.contains("Cancelled:"));
        assert!(content.contains("example_code"));
        assert!(content.contains("missing tool"));
    }
}

#[test]
fn diagnostic_hierarchy_styles_hint_and_matches_card_width() {
    let mut report = Report::new(policy(80, true));
    report.card("Status", &[("Cargo".into(), "installed".into())]);
    report.findings(
        [Finding {
            level: FindingLevel::Warn,
            code: "missing_tool",
            message: "The selected local node is not installed on this machine.".into(),
            hint: Some("Install the tool or set the existing binary override.".into()),
        }]
        .iter(),
    );
    let styled = report.into_lines();
    let lines: Vec<_> = styled.iter().map(|line| plain(line)).collect();
    let width = UnicodeWidthStr::width(lines[0].as_str());
    let heading = lines
        .iter()
        .position(|line| line.starts_with("⚠ Warning"))
        .unwrap();
    let hint = lines
        .iter()
        .position(|line| line.trim() == "↳ Hint")
        .unwrap();
    assert!(lines[heading].contains("missing_tool"));
    assert!(styled[heading].contains("[2m"), "code should be muted");
    assert!(styled[hint].contains("38;5;6m"), "hint should be cyan");
    assert!(styled[hint].contains("[1m"), "hint heading should be bold");
    assert!(!lines[hint - 1].is_empty());
    assert!(
        lines[heading + 1..hint]
            .iter()
            .all(|line| line.starts_with("  "))
    );
    assert!(
        lines[hint + 1..]
            .iter()
            .all(|line| line.starts_with("    "))
    );
    assert!(
        lines[heading..]
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= width)
    );
}

#[test]
fn diagnostic_hint_has_ascii_and_plain_fallbacks() {
    let mut report = Report::new(Policy {
        width: Some(40),
        color: false,
        unicode: false,
    });
    report.message(
        Outcome::Error,
        "Something failed",
        Some("failure"),
        Some("Try the existing command"),
    );
    let output = report.into_lines().join("\n");
    assert!(output.contains("Error - failure"));
    assert!(output.contains("  > Hint"));
    assert!(output.is_ascii() && !output.contains('\x1b'));

    let mut report = Report::new(Policy::PLAIN);
    report.message(
        Outcome::Error,
        "Something failed",
        Some("failure"),
        Some("Try the existing command"),
    );
    assert_eq!(
        report.into_lines(),
        [
            "error: Something failed",
            "  hint: Try the existing command"
        ]
    );
}

#[test]
fn compiler_notes_share_hint_layout_without_removing_source_gutters() {
    for unicode in [true, false] {
        for excerpt in ["", " --> src/main.rs:2:1\n2 | value;\n  | ^^^^^\n"] {
            let mut report = Report::new(Policy {
                width: Some(80),
                color: false,
                unicode,
            });
            report.compiler_diagnostic(
                Outcome::Warning,
                &format!(
                    "Compiler warning\n{excerpt}  |\n  = note: first line\n          second line"
                ),
                None,
            );
            let lines: Vec<_> = report
                .into_lines()
                .iter()
                .map(|line| line.trim_end().to_owned())
                .collect();
            let arrow = if unicode { "↳" } else { ">" };
            assert!(lines.contains(&format!("  {arrow} Note")));
            assert!(lines.contains(&"    first line".to_owned()));
            assert!(lines.contains(&"    second line".to_owned()));
            assert_eq!(
                lines.iter().any(|line| line.trim() == "|"),
                !excerpt.is_empty()
            );
            if !excerpt.is_empty() {
                assert!(lines.iter().any(|line| line.contains("2 | value;")));
                assert!(lines.iter().any(|line| line.contains("| ^^^^^")));
            }
        }
    }
}

#[test]
fn tables_wrap_cells_and_stack_complete_records_on_narrow_screens() {
    let headers = vec!["Name".into(), "Address".into()];
    let address = "0123456789abcdef".repeat(8);
    let rows = vec![
        vec!["alice".into(), address.clone()],
        vec!["bob".into(), "other".into()],
    ];
    for width in [24, 40, 80] {
        let mut report = Report::new(policy(width, false));
        report.table("Wallets", &headers, &rows);
        let lines = report.into_lines();
        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= usize::from(width))
        );
        let content = lines
            .iter()
            .map(|line| line.trim_matches('│').trim())
            .collect::<String>();
        assert!(content.contains("alice"));
        assert!(content.contains(&address), "{content}");
        assert!(content.contains("bob"));
        assert!(content.contains("other"));
    }
}

#[test]
fn empty_tables_and_unmatched_cells_have_explicit_complete_output() {
    let mut report = Report::new(policy(80, false));
    report.table("Wallets", &[], &[]);
    assert!(report.into_lines().join("\n").contains("No results"));
    let mut report = Report::new(policy(24, false));
    report.table(
        "Rows",
        &["Name".into()],
        &[vec!["alice".into(), "extra".into()]],
    );
    assert!(report.into_lines().join("\n").contains("extra"));
}

#[test]
fn prompt_and_next_steps_use_same_layout_without_changing_input_rules() {
    let mut report = Report::new(policy(24, false));
    report.prompt("Confirm this operation for the current wallet? [y/N] ");
    let lines = report.into_lines();
    assert!(lines.last().unwrap().ends_with("[y/N] "));
    assert!(
        lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 24)
    );
    let mut report = Report::new(policy(24, false));
    report.next_steps(&["groundhog status --profile local".into()]);
    let content = report
        .into_lines()
        .iter()
        .map(|line| line.trim())
        .collect::<String>();
    assert!(content.contains("Next:"));
    assert!(content.contains("groundhog status"));
}

#[test]
fn tiny_width_fallback_retains_complete_title_and_values() {
    let mut report = Report::new(policy(10, false));
    report.card(
        "Groundhog Status",
        &[("Path".into(), "abcdefghijklmnop".into())],
    );
    let lines = report.into_lines();
    assert!(!lines[0].contains('╭'));
    assert!(
        lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 10)
    );
    assert!(
        lines
            .iter()
            .map(|line| line.trim())
            .collect::<String>()
            .contains("abcdefghijklmnop")
    );
}
