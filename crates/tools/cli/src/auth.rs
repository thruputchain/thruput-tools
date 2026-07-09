use crate::checks::{Finding, FindingLevel};
use crate::config::{self, ExecutorCredentials};
use crate::{CliError, Print};
use serde_json::json;
use std::env;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::process::Command;

/// Resolve the user's cargo config path.
pub fn cargo_config_path() -> Result<PathBuf, CliError> {
    let cargo_home = env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| config::home_dir().map(|h| h.join(".cargo")))?;
    Ok(cargo_home.join("config.toml"))
}

/// Store executor credentials as part of `groundhog init` without nested wizard headings.
pub(crate) fn executor_login_from_init(print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        return Err(CliError::JsonUnsupported("init"));
    }

    print.status("These are used by `groundhog test`.");
    save_executor_credentials_from_prompt(print)?;
    print.status("\nCredentials saved.");
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
    print.json(&crate::result::CommandResult::with_warnings(
        cmd,
        warnings,
        json!({
            "config_dir": config::config_dir()?,
        }),
    ))?;
    Ok(())
}
