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

    render_environment(&report, print);
    print.status("");
    render_keys(&report, print);
    print.status("");
    render_credentials(&report, print);
    print.status("");
    render_project(&report, print);
    print.status("");
    render_findings(&report, print);

    if report.has_warnings() {
        Err(CliError::StatusHasWarnings)
    } else {
        Ok(())
    }
}

fn render_environment(report: &checks::StatusReport, print: &Print) {
    if report.environment.cargo {
        print.status("cargo: installed");
    } else {
        print.status("cargo: NOT FOUND");
    }

    match report.environment.wasm_target {
        checks::WasmTargetStatus::Installed => {
            print.status("wasm target: installed");
        }
        checks::WasmTargetStatus::Missing => {
            print.status(
                "wasm target: not found (installed automatically on \
                 first build)",
            );
        }
        checks::WasmTargetStatus::Unknown => {
            print.status("wasm target: unknown (rustup unavailable)");
        }
    }

    if report.environment.registry_configured {
        print.status(format_args!(
            "registry: configured ({})",
            report.environment.cargo_config_path.display()
        ));
    } else {
        print.status("registry: NOT configured");
    }

    if report.environment.wasm_opt {
        print.status("wasm-opt: installed");
    } else {
        print.status("wasm-opt: not installed (optional)");
    }
}

fn render_keys(report: &checks::StatusReport, print: &Print) {
    if report.keys.keys.is_empty() {
        print.status("keys: none");
    } else {
        for key in &report.keys.keys {
            let marker = if key.is_default { "*" } else { " " };
            print.status(format_args!("key:{marker} {} {}", key.name, key.address,));
        }
    }

    // Only show effective default key when keys exist —
    // "keys: none" already tells the story.
    if !report.keys.keys.is_empty() {
        if let Some(name) = report.effective_default_key.as_deref() {
            let source = report.effective_default_key_source.unwrap_or("global");
            print.status(format_args!("default key ({source}): {name}",));
        } else {
            print.status("default key: none selected");
        }
    }
}

fn render_credentials(report: &checks::StatusReport, print: &Print) {
    if report.credentials.executor_credentials {
        print.status("credentials: stored");
    } else {
        print.status("credentials: NOT stored");
    }
}

fn render_project(report: &checks::StatusReport, print: &Print) {
    if let Some(path) = report.project.config_path.as_ref() {
        print.status(format_args!("project: {}", path.display()));
        if let Some(name) = report.project.profile_name.as_deref() {
            print.status(format_args!("profile: {name}"));
        }
        if let Some(url) = report.project.rpc_url.as_deref() {
            print.status(format_args!("rpc_url: {url}"));
        }
        let profile_resolved = !report
            .project
            .findings
            .iter()
            .any(|f| f.code == "profile_not_found" || f.code == "no_project_config");
        if profile_resolved {
            print.status(format_args!("contracts: {}", report.project.contract_count));
        }
    } else {
        print.status("project: none (not in a project directory)");
    }
}

fn render_findings(report: &checks::StatusReport, print: &Print) {
    for finding in report.all_findings() {
        match finding.level {
            FindingLevel::Warn => {
                print.warn(format_args!("{}: {}", finding.code, finding.message,));
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
