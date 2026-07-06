use crate::checks::{Finding, FindingLevel};
use crate::config::{self, ExecutorCredentials};
use crate::result::CommandResult;
use crate::{CliError, Print};
use serde_json::json;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

const REGISTRY_INDEX: &str = "sparse+https://groundhog.dev/crates/api/v1/crates/";

/// Resolve the user's cargo config path.
pub fn cargo_config_path() -> Result<PathBuf, CliError> {
    let cargo_home = env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| config::home_dir().map(|h| h.join(".cargo")))?;
    Ok(cargo_home.join("config.toml"))
}

/// Configure the Groundhog cargo registry and store
/// test executor credentials.
pub fn login(cmd: &'static str, print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        return Err(CliError::JsonUnsupported(cmd));
    }

    crate::require_cargo()?;

    let cargo_config = cargo_config_path()?;
    ensure_cargo_registry(&cargo_config)?;
    print.status("Configured cargo registry.");

    cargo_registry_login_standalone(print)?;

    print.status("\nTest executor credentials");
    save_executor_credentials_from_prompt(print)?;
    print.status("\nCredentials saved. You're ready to go.");

    Ok(())
}

/// Store executor credentials as part of `groundhog init` without nested wizard headings.
pub(crate) fn executor_login_from_init(print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        return Err(CliError::JsonUnsupported("login"));
    }

    print.status("These are used by `groundhog test`.");
    save_executor_credentials_from_prompt(print)?;
    print.status("\nCredentials saved.");
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum CargoLoginStatus {
    LoggedIn,
    Skipped,
    NotInteractive,
}

/// Offer Cargo registry login after creating a project.
pub(crate) fn offer_cargo_login_after_new(print: &Print) -> Result<CargoLoginStatus, CliError> {
    if print.is_json() || print.is_quiet() {
        return Ok(CargoLoginStatus::NotInteractive);
    }

    if !print.is_human_tty() {
        print.note("Before building, run `cargo login --registry groundhog` if Cargo is not already logged in; Cargo stores that token globally in `CARGO_HOME`, not in this project");
        return Ok(CargoLoginStatus::NotInteractive);
    }

    print.note("Groundhog projects depend on packages from the private Cargo registry");
    print.note("Cargo stores this token globally in `CARGO_HOME`, not in this project");
    if confirm(print, "Log in to Cargo registry now?", true)? {
        cargo_registry_login_after_new(print)?;
        return Ok(CargoLoginStatus::LoggedIn);
    }

    print.note("Run `cargo login --registry groundhog` before building if needed");
    Ok(CargoLoginStatus::Skipped)
}

fn cargo_registry_login_standalone(print: &Print) -> Result<(), CliError> {
    crate::require_cargo()?;

    print.status("\nRegistry token");
    print.status(
        "Paste your registry token from \
         https://groundhog.dev/crates/me",
    );
    let mut command = Command::new("cargo");
    command.arg("login").arg("--registry").arg("groundhog");
    let status = command.status()?;
    if !status.success() {
        return Err(CliError::RegistryLoginFailed);
    }
    Ok(())
}

fn cargo_registry_login_after_new(print: &Print) -> Result<(), CliError> {
    crate::require_cargo()?;

    print.status("");
    print.status("Open this page and create or copy a token:");
    print.status("https://groundhog.dev/crates/me");
    print.status("");

    let status = Command::new("cargo")
        .arg("login")
        .arg("--registry")
        .arg("groundhog")
        .arg("--quiet")
        .status()?;
    if !status.success() {
        return Err(CliError::RegistryLoginFailed);
    }

    Ok(())
}

fn save_executor_credentials_from_prompt(print: &Print) -> Result<(), CliError> {
    let stdin = io::stdin();

    print.prompt("Executor username: ")?;
    let mut username = String::new();
    stdin.lock().read_line(&mut username)?;
    let username = username.trim().to_string();

    let password = rpassword::prompt_password("Executor password: ")
        .map_err(|e| CliError::Other(e.to_string()))?;

    if username.is_empty() || password.is_empty() {
        return Err(CliError::CredentialsEmpty);
    }

    config::save_executor_credentials(&ExecutorCredentials { username, password })?;
    Ok(())
}

fn confirm(print: &Print, message: &str, default_yes: bool) -> Result<bool, CliError> {
    let suffix = if default_yes { " [Y/n] " } else { " [y/N] " };
    print.prompt(format_args!("{message}{suffix}"))?;

    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(default_yes);
    }

    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Remove stored registry token and executor credentials.
pub fn logout(cmd: &'static str, print: &Print) -> Result<(), CliError> {
    crate::require_cargo()?;

    config::remove_executor_credentials()?;

    let logout_ok = if print.is_json() {
        Command::new("cargo")
            .arg("logout")
            .arg("--registry")
            .arg("groundhog")
            .output()?
            .status
            .success()
    } else {
        Command::new("cargo")
            .arg("logout")
            .arg("--registry")
            .arg("groundhog")
            .status()?
            .success()
    };

    let mut warnings = Vec::new();
    if !logout_ok {
        print.warn("cargo logout returned non-zero");
        if print.is_json() {
            warnings.push(Finding {
                level: FindingLevel::Warn,
                code: "cargo_logout_failed",
                message: "cargo logout returned non-zero".into(),
                hint: None,
            });
        }
    }

    print.status("Credentials removed.");
    print.json(&CommandResult::with_warnings(
        cmd,
        warnings,
        json!({
            "config_dir": config::config_dir()?,
        }),
    ))?;
    Ok(())
}

/// Ensure the `[registries.groundhog]` entry exists in
/// the user's cargo config. Appends it if missing.
pub fn ensure_cargo_registry(cargo_config: &Path) -> Result<(), CliError> {
    let contents = match fs::read_to_string(cargo_config) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    if contents.contains("[registries.groundhog]") {
        return Ok(());
    }

    if let Some(parent) = cargo_config.parent() {
        fs::create_dir_all(parent)?;
    }

    let entry = format!("\n[registries.groundhog]\nindex = \"{REGISTRY_INDEX}\"\n");

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cargo_config)?;
    io::Write::write_all(&mut file, entry.as_bytes())?;

    Ok(())
}
