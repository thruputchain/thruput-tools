//! `groundhog init` — interactive first-run wizard.
//!
//! Walks a new user through environment setup, key generation,
//! credential storage, and optional project scaffolding. When
//! everything is already configured, prints a short summary and
//! exits. `init --json` delegates to `status --json`.

use std::io::BufRead;
use std::path::Path;

use crate::{CliError, Print};

/// Run the interactive first-run wizard.
pub fn run(project_start: &Path, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        return crate::status::run(project_start, None, cmd, print);
    }

    let report = crate::checks::collect_report(project_start, None)?;
    let has_project = report.project.config_path.is_some();
    let usable_default_key = usable_default_key_for_init(&report);
    let has_credentials = report.credentials.executor_credentials;

    crate::config::initialize_global_profiles()?;

    if report.environment.cargo
        && report.environment.registry_configured
        && usable_default_key.is_some()
        && has_credentials
    {
        report_ready(&report, usable_default_key, print);
        return Ok(());
    }

    if !report.environment.cargo {
        return Err(CliError::CargoNotFound);
    }

    handle_environment_step(&report, print)?;
    handle_key_step(&report, usable_default_key, print)?;
    handle_credentials_step(&report, print)?;
    handle_scaffold_step(has_project, project_start, &report, print)?;

    print.status("Setup complete.");
    Ok(())
}

fn report_ready(
    report: &crate::checks::StatusReport,
    usable_default_key: Option<&crate::checks::KeySummary>,
    print: &Print,
) {
    print.status("Groundhog is ready to go.");
    if let Some(key) = usable_default_key {
        print.status(format_args!(
            "Default key: `{}` ({})",
            key.name, key.address,
        ));
    }
    if !report.environment.wasm_opt {
        print.status(
            "wasm-opt not installed (optional, needed \
             for `build --optimize`).",
        );
    }
    if matches!(
        report.environment.wasm_target,
        crate::checks::WasmTargetStatus::Missing
    ) {
        print.status(
            "WASM target will be installed automatically \
             on first build.",
        );
    }
    print.status("Run `groundhog status` for full details.");
}

fn handle_environment_step(
    report: &crate::checks::StatusReport,
    print: &Print,
) -> Result<(), CliError> {
    if report.environment.registry_configured {
        print.status("Cargo registry already configured.");
    } else {
        crate::auth::ensure_cargo_registry(&report.environment.cargo_config_path)?;
        print.status("Configured cargo registry.");
    }

    match report.environment.wasm_target {
        crate::checks::WasmTargetStatus::Installed => {
            print.status("WASM target installed.");
        }
        crate::checks::WasmTargetStatus::Missing => {
            print.status("WASM target will be installed automatically on first build.");
        }
        crate::checks::WasmTargetStatus::Unknown => {
            print.status("Could not verify the WASM target (rustup unavailable).");
        }
    }

    if !report.environment.wasm_opt {
        print.status("wasm-opt not installed (optional, needed for `build --optimize`).");
    }

    Ok(())
}

fn handle_key_step(
    report: &crate::checks::StatusReport,
    usable_default_key: Option<&crate::checks::KeySummary>,
    print: &Print,
) -> Result<(), CliError> {
    if report.keys.keys.is_empty() {
        if confirm(print, "No signing key found. Generate one now?", true)? {
            crate::keys::generate(&None, false, "keys.generate", print)?;
        }
    } else if let Some(key) = usable_default_key {
        print.status(format_args!(
            "Default key: `{}` ({})",
            key.name, key.address,
        ));
    } else {
        print.status(
            "You have keys but no usable default is selected. Set one \
             with `groundhog keys use <name>`.",
        );
    }

    Ok(())
}

fn usable_default_key_for_init(
    report: &crate::checks::StatusReport,
) -> Option<&crate::checks::KeySummary> {
    if let Some(key) = report.effective_default_key_summary() {
        return Some(key);
    }

    let global_default = report.keys.global_default_key.as_deref()?;
    report
        .keys
        .keys
        .iter()
        .find(|key| key.name == global_default)
}

fn handle_credentials_step(
    report: &crate::checks::StatusReport,
    print: &Print,
) -> Result<(), CliError> {
    if report.credentials.executor_credentials {
        print.status("Executor credentials stored.");
    } else if confirm(
        print,
        "No test executor credentials found. Run login now?",
        true,
    )? {
        crate::auth::login("login", print)?;
    }

    Ok(())
}

fn handle_scaffold_step(
    has_project: bool,
    project_start: &Path,
    report: &crate::checks::StatusReport,
    print: &Print,
) -> Result<(), CliError> {
    if has_project {
        if let Some(path) = report.project.config_path.as_ref() {
            print.status(format_args!("Found project at {}", path.display()));
        }
        return Ok(());
    }

    if confirm(
        print,
        "Create a starter project? You can always do this later \
         with `groundhog new`.",
        false,
    )? {
        print.prompt("Project name: ")?;
        let mut name = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut name)
            .map_err(CliError::Io)?;
        let name = name.trim();
        if !name.is_empty() {
            let dir = project_start.join(name);
            crate::new::run_in(&dir, name, "counter", "new", print)?;
        }
    }

    Ok(())
}

fn confirm(print: &Print, message: &str, default_yes: bool) -> Result<bool, CliError> {
    if print.is_quiet() {
        return Ok(default_yes);
    }

    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    print.prompt(format_args!("{message} {suffix} "))?;

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(CliError::Io)?;
    let answer = answer.trim();

    if answer.is_empty() {
        return Ok(default_yes);
    }

    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}
