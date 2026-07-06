//! `groundhog status` — non-interactive diagnostic.
//!
//! Reports toolchain, keys, credentials, and project context.
//! Never modifies state. Exits 0 when clean, 1 when findings
//! exist. `--json` emits a single structured object; `--quiet`
//! suppresses output and uses exit code only.

use std::path::Path;

use serde_json::json;

use crate::checks::{self, FindingLevel};
use crate::{CliError, Print};

/// Run the `status` diagnostic command.
pub fn run(
    project_start: &Path,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let report = checks::collect_report(project_start, explicit_profile)?;

    if print.is_json() {
        let all_findings: Vec<_> = report.all_findings().cloned().collect();
        let envelope = crate::result::CommandResult::with_warnings(
            cmd,
            all_findings,
            json!({
                "environment": &report.environment,
                "keys": &report.keys,
                "credentials": &report.credentials,
                "project": {
                    "config_path":
                        report.project.config_path,
                    "profile_name":
                        report.project.profile_name,
                    "profile_key":
                        report.project.profile_key,
                    "rpc_url": report.project.rpc_url,
                    "contract_count":
                        report.project.contract_count,
                },
                "effective_default_key":
                    report.effective_default_key
                        .as_deref(),
                "effective_default_key_source":
                    report.effective_default_key_source,
            }),
        );
        let has_warnings = envelope.has_warnings();
        print.json(&envelope)?;
        return if has_warnings {
            Err(CliError::StatusHasWarnings)
        } else {
            Ok(())
        };
    }

    if print.is_quiet() {
        return if report.has_warnings() {
            Err(CliError::StatusHasWarnings)
        } else {
            Ok(())
        };
    }

    render_summary(&report, print);
    print.status("");
    render_findings(&report, print);

    if report.has_warnings() {
        Err(CliError::StatusHasWarnings)
    } else {
        Ok(())
    }
}

fn render_summary(report: &checks::StatusReport, print: &Print) {
    let items = summary_items(report);
    print.summary_card("Groundhog Status", &items);
}

fn summary_items(report: &checks::StatusReport) -> Vec<(String, String)> {
    let mut items = vec![
        ("Cargo".into(), installed_label(report.environment.cargo)),
        (
            "Wasm Target".into(),
            wasm_target_label(report.environment.wasm_target),
        ),
        (
            "Global Registry".into(),
            if report.environment.registry_configured {
                format!(
                    "configured ({})",
                    report.environment.cargo_config_path.display()
                )
            } else {
                "not configured".into()
            },
        ),
        (
            "wasm-opt".into(),
            optional_installed_label(report.environment.wasm_opt),
        ),
        ("Keys".into(), keys_label(report)),
        ("Default Key".into(), default_key_label(report)),
        ("Credentials".into(), credentials_label(report)),
        ("Project".into(), project_label(report)),
    ];

    if report.project.profile_name.is_some() {
        items.push(("Profile".into(), profile_label(report)));
        items.push(("RPC URL".into(), rpc_url_label(report)));
        items.push(("Contracts".into(), contracts_label(report)));
    }

    items
}

fn installed_label(installed: bool) -> String {
    if installed {
        "installed".into()
    } else {
        "not found".into()
    }
}

fn optional_installed_label(installed: bool) -> String {
    if installed {
        "installed".into()
    } else {
        "not installed (optional)".into()
    }
}

fn wasm_target_label(status: checks::WasmTargetStatus) -> String {
    match status {
        checks::WasmTargetStatus::Installed => "installed".into(),
        checks::WasmTargetStatus::Missing => {
            "not found (installed automatically on first build)".into()
        }
        checks::WasmTargetStatus::Unknown => "unknown (rustup unavailable)".into(),
    }
}

fn keys_label(report: &checks::StatusReport) -> String {
    match report.keys.keys.len() {
        0 => "none".into(),
        1 => "1 key".into(),
        count => format!("{count} keys"),
    }
}

fn default_key_label(report: &checks::StatusReport) -> String {
    match report.effective_default_key.as_deref() {
        Some(name) => {
            let source = report.effective_default_key_source.unwrap_or("global");
            format!("{name} ({source})")
        }
        None => "none selected".into(),
    }
}

fn credentials_label(report: &checks::StatusReport) -> String {
    if report.credentials.executor_credentials {
        "stored".into()
    } else {
        "not stored".into()
    }
}

fn project_label(report: &checks::StatusReport) -> String {
    report.project.config_path.as_ref().map_or_else(
        || "none (not in a project directory)".into(),
        |path| path.display().to_string(),
    )
}

fn profile_label(report: &checks::StatusReport) -> String {
    report
        .project
        .profile_name
        .as_deref()
        .unwrap_or("none")
        .into()
}

fn rpc_url_label(report: &checks::StatusReport) -> String {
    report.project.rpc_url.as_deref().unwrap_or("none").into()
}

fn contracts_label(report: &checks::StatusReport) -> String {
    if profile_resolved(report) {
        report.project.contract_count.to_string()
    } else {
        "unknown".into()
    }
}

fn profile_resolved(report: &checks::StatusReport) -> bool {
    !report
        .project
        .findings
        .iter()
        .any(|finding| finding.code == "profile_not_found" || finding.code == "no_project_config")
}

fn render_findings(report: &checks::StatusReport, print: &Print) {
    for finding in report.all_findings() {
        match finding.level {
            FindingLevel::Warn => {
                print.warn(format_args!("{}: {}", finding.code, finding.message));
            }
            FindingLevel::Info => {
                print.status(format_args!("info: {}", finding.message));
            }
        }
        if let Some(hint) = finding.hint.as_deref() {
            print.status(format_args!("  hint: {hint}"));
        }
    }
}
