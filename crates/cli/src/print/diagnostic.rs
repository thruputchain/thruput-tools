//! Deferred compiler diagnostics for the compact build report.

use super::{Outcome, Print, report::Report};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Diagnostic {
    pub outcome: Outcome,
    pub code: Option<String>,
    pub text: String,
    source: Option<String>,
}

impl Print {
    /// Show build logs as they arrive instead of collecting compiler diagnostics.
    #[must_use]
    pub fn with_verbose_build(mut self, verbose: bool) -> Self {
        self.verbose_build = verbose;
        self
    }

    pub(crate) fn is_compact_build(&self) -> bool {
        self.compact_build
    }

    pub(crate) fn with_compact_build(mut self, enabled: bool) -> Self {
        self.compact_build = enabled && !self.verbose_build;
        self
    }

    fn collect_diagnostic(&self, diagnostic: Diagnostic) {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if diagnostic.outcome == Outcome::Information
            && diagnostic.source.is_none()
            && let Some(previous) = diagnostics.last_mut()
            && previous.outcome == Outcome::Information
            && previous.source.is_none()
        {
            previous.text.push('\n');
            previous.text.push_str(&diagnostic.text);
            return;
        }
        // Cargo may report the same compiler diagnostic for both lib and bin targets.
        // Keep identical messages from different packages separate.
        if diagnostic.source.is_none() || !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
        }
    }

    pub(crate) fn defer_warning(&self, text: String) {
        self.defer_diagnostic(Outcome::Warning, text);
    }

    pub(super) fn defer_diagnostic(&self, outcome: Outcome, text: String) {
        self.collect_diagnostic(Diagnostic {
            outcome,
            code: None,
            text,
            source: None,
        });
    }

    pub(crate) fn collect_build_log(&self, text: &str) {
        let text = text
            .lines()
            .filter(|line| !cargo_status_line(line) && !cargo_warning_count(line))
            .collect::<Vec<_>>()
            .join("\n");
        self.collect_build_output(&text);
    }

    pub(crate) fn collect_build_output(&self, text: &str) {
        if !text.trim().is_empty() {
            self.collect_diagnostic(Diagnostic {
                outcome: Outcome::Information,
                code: None,
                text: text.trim_end_matches(['\r', '\n']).to_owned(),
                source: None,
            });
        }
    }

    /// Return rendered text for Cargo's failure context as well as storing the diagnostic.
    pub(crate) fn collect_compiler_message(&self, record: &serde_json::Value) -> Option<String> {
        let message = record.get("message")?;
        let level = message.get("level")?.as_str()?;
        let rendered = message.get("rendered")?.as_str()?;
        let code = message
            .get("code")
            .and_then(|code| code.get("code"))
            .and_then(serde_json::Value::as_str);
        let prefix = code.map_or_else(|| format!("{level}: "), |code| format!("{level}[{code}]: "));
        let text = rendered
            .strip_prefix(&prefix)
            .or_else(|| rendered.strip_prefix(&format!("{level}: ")))
            .unwrap_or(rendered)
            .trim_end()
            .to_owned();
        if level == "warning" && expected_target_feature_warning(&text) {
            return Some(rendered.to_owned());
        }
        let outcome = match level {
            "warning" => Outcome::Warning,
            "error" | "failure-note" => Outcome::Error,
            _ => Outcome::Information,
        };
        self.collect_diagnostic(Diagnostic {
            outcome,
            code: code.map(str::to_owned),
            text,
            source: record
                .get("package_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        });
        Some(rendered.to_owned())
    }

    pub(super) fn append_diagnostics(&self, report: &mut Report) {
        let diagnostics = std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for diagnostic in diagnostics {
            report.compiler_diagnostic(
                diagnostic.outcome,
                &diagnostic.text,
                diagnostic.code.as_deref(),
            );
        }
    }
}

/// The Thruput rv32 flag set enables these RISC-V features deliberately, so rustc's
/// per-crate "unstable feature" warnings about them are noise in the compact report.
fn expected_target_feature_warning(text: &str) -> bool {
    const EXPECTED: [&str; 2] = ["relax", "unaligned-scalar-mem"];
    text.lines()
        .next()
        .and_then(|line| line.strip_prefix("unstable feature specified for `-Ctarget-feature`: `"))
        .and_then(|feature| feature.strip_suffix('`'))
        .is_some_and(|feature| EXPECTED.contains(&feature))
}

fn cargo_status_line(line: &str) -> bool {
    let content = line.trim_start_matches(' ');
    let Some((label, _)) = content.split_once(' ') else {
        return false;
    };
    // Cargo right-aligns its status labels in twelve columns. Do not hide
    // arbitrary build-script text just because it starts with the same verb.
    line.len() - content.len() + label.len() == 12 && super::log::is_routine(line)
}

fn cargo_warning_count(line: &str) -> bool {
    let Some(rest) = line
        .strip_prefix("warning: `")
        .and_then(|line| line.split_once(" generated ").map(|(_, count)| count))
    else {
        return false;
    };
    let Some((count, suffix)) = rest.split_once(' ') else {
        return false;
    };
    !count.is_empty()
        && count.bytes().all(|byte| byte.is_ascii_digit())
        && (suffix == "warning"
            || suffix == "warnings"
            || (suffix.starts_with("warnings (") && suffix.ends_with("duplicates)")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_diagnostics_from_different_packages_stay_separate() {
        let print = Print::new(false, false);
        let mut record = serde_json::json!({
            "package_id": "first",
            "message": {"level": "warning", "code": {"code":"unused_variables"}, "rendered": "warning: original wording\n  = note: details\n"}
        });
        print.collect_compiler_message(&record);
        print.collect_compiler_message(&record);
        record["package_id"] = "second".into();
        print.collect_compiler_message(&record);
        let diagnostics = print.diagnostics.lock().unwrap();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].text, "original wording\n  = note: details");
    }

    #[test]
    fn expected_target_feature_warnings_are_not_reported() {
        let print = Print::new(false, false);
        for feature in ["relax", "unaligned-scalar-mem", "zbb"] {
            let rendered = format!(
                "warning: unstable feature specified for `-Ctarget-feature`: `{feature}`\n  |\n  \
                 = note: this feature is not stably supported; its behavior can change in the future\n"
            );
            let record = serde_json::json!({
                "package_id": "contract",
                "message": {"level": "warning", "code": null, "rendered": rendered}
            });
            assert_eq!(print.collect_compiler_message(&record), Some(rendered));
        }
        let diagnostics = print.diagnostics.lock().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].text.ends_with("`zbb`\n  |\n  = note: this feature is not stably supported; its behavior can change in the future"));
    }

    #[test]
    fn unknown_build_output_is_preserved_without_inventing_a_severity() {
        let print = Print::new(false, false);
        print.collect_build_log(
            "   Compiling dependency\nCompiling custom build-script message\nerror-looking text\n",
        );
        let diagnostics = print.diagnostics.lock().unwrap();
        assert_eq!(
            diagnostics[0].text,
            "Compiling custom build-script message\nerror-looking text"
        );
        assert_eq!(diagnostics[0].outcome, Outcome::Information);
    }
}
