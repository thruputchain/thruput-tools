//! `groundhog status` — non-interactive diagnostic.
//!
//! Reports the RV32 toolchain, local devnode, keys, credentials,
//! and project context.
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
                    "profile_default_wallet":
                        report.project.profile_default_wallet,
                    "default_wallet":
                        report.project.default_wallet,
                    "default_wallet_source":
                        report.project.default_wallet_source,
                    "rpc_url": report.project.rpc_url,
                    "wallet_count":
                        report.project.wallet_count,
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
            "Rust Toolchain".into(),
            rust_toolchain_label(&report.environment.rust_toolchain),
        ),
        (
            "RV32 Target".into(),
            rust_target_label(report.environment.rv32_target),
        ),
        (
            "hog-devnode".into(),
            devnode_label(&report.environment.devnode),
        ),
        (
            "Dev Wallet".into(),
            dev_wallet_artifact_label(&report.environment.dev_wallet_artifact),
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
        ("Keys".into(), keys_label(report)),
        ("Default Key".into(), default_key_label(report)),
        ("Mock Executor Auth".into(), credentials_label(report)),
        ("Project".into(), project_label(report)),
    ];

    if report.project.profile_name.is_some() {
        items.push(("Profile".into(), profile_label(report)));
        items.push(("RPC URL".into(), rpc_url_label(report)));
        items.push((
            "Profile Default Wallet".into(),
            profile_default_wallet_label(report),
        ));
        items.push((
            "Effective Default Wallet".into(),
            default_wallet_label(report),
        ));
        items.push(("Wallets".into(), wallets_label(report)));
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

fn rust_target_label(status: checks::RustTargetStatus) -> String {
    match status {
        checks::RustTargetStatus::Installed => "installed".into(),
        checks::RustTargetStatus::Missing => {
            "not found (installed automatically on first build)".into()
        }
        checks::RustTargetStatus::Unknown => "unknown (toolchain unavailable)".into(),
    }
}

fn rust_toolchain_label(status: &checks::RustToolchainStatus) -> String {
    let Some(name) = status.name.as_deref() else {
        return "unavailable".into();
    };
    if !status.available {
        return format!("{name} (not installed)");
    }
    let rust_src = match status.rust_src {
        checks::RustTargetStatus::Installed => "rust-src installed",
        checks::RustTargetStatus::Missing => "rust-src not found",
        checks::RustTargetStatus::Unknown => "rust-src unknown",
    };
    format!("{name} ({rust_src})")
}

fn devnode_label(status: &checks::DevnodeStatus) -> String {
    match (&status.path, &status.version) {
        (Some(path), Some(version)) if status.reachable => {
            format!("{version} ({})", path.display())
        }
        (Some(path), _) if status.reachable => format!("available ({})", path.display()),
        (Some(path), _) => format!("unusable ({})", path.display()),
        (None, _) => "not found".into(),
    }
}

fn dev_wallet_artifact_label(status: &checks::DevWalletArtifactStatus) -> String {
    match (&status.path, &status.sha256) {
        (Some(path), Some(sha256)) if status.valid => {
            let short_hash = sha256.get(..12).unwrap_or(sha256);
            format!("valid ({}, sha256 {short_hash})", path.display())
        }
        (Some(path), _) if status.available => format!("invalid ({})", path.display()),
        _ => "not found".into(),
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
    match report.credentials.source {
        Some(crate::auth::ExecutorCredentialSource::Environment) => {
            "available (environment)".into()
        }
        Some(crate::auth::ExecutorCredentialSource::Config) => "available (config)".into(),
        None => "not configured".into(),
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

fn default_wallet_label(report: &checks::StatusReport) -> String {
    match report.project.default_wallet.as_deref() {
        Some(name) => format!(
            "{} ({})",
            name,
            report
                .project
                .default_wallet_source
                .unwrap_or("unknown source")
        ),
        None => "none selected".into(),
    }
}

fn profile_default_wallet_label(report: &checks::StatusReport) -> String {
    report
        .project
        .profile_default_wallet
        .as_deref()
        .unwrap_or("none configured")
        .into()
}

fn wallets_label(report: &checks::StatusReport) -> String {
    if profile_resolved(report) {
        report.project.wallet_count.to_string()
    } else {
        "unknown".into()
    }
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
