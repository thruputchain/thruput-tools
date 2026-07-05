use crate::checks::{Finding, FindingLevel};
use crate::config::{self, ExecutorCredentials};
use crate::result::CommandResult;
use crate::{CliError, Print};
use serde_json::json;
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

const REGISTRY_INDEX: &str = "sparse+https://groundhog.dev/crates/api/v1/crates/";
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";

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
    login_inner(cmd, print, LoginMode::Standalone)
}

/// Run login as part of `groundhog init` without nested wizard headings.
pub(crate) fn login_from_init(print: &Print) -> Result<(), CliError> {
    login_inner("login", print, LoginMode::Init)
}

#[derive(Clone, Copy)]
enum LoginMode {
    Standalone,
    Init,
}

fn login_inner(cmd: &'static str, print: &Print, mode: LoginMode) -> Result<(), CliError> {
    if print.is_json() {
        return Err(CliError::JsonUnsupported(cmd));
    }

    crate::require_cargo()?;

    let cargo_config = cargo_config_path()?;

    ensure_cargo_registry(&cargo_config)?;
    match mode {
        LoginMode::Standalone => print.status("Configured cargo registry."),
        LoginMode::Init => print.status("Cargo registry configured."),
    }

    match mode {
        LoginMode::Standalone => {
            print.status("\nRegistry token");
            print.status(
                "Paste your registry token from \
                 https://groundhog.dev/crates/me",
            );
        }
        LoginMode::Init => {
            print.status("");
            init_subheading(print, "Registry token");
            print.status("Open this page and create or copy a token:");
            print.status("https://groundhog.dev/crates/me");
            print.status("");
            print.prompt("Paste registry token: ")?;
        }
    }
    let mut command = Command::new("cargo");
    command.arg("login").arg("--registry").arg("groundhog");
    if matches!(mode, LoginMode::Init) {
        command.arg("--quiet");
    }
    let status = command.status()?;
    if !status.success() {
        return Err(CliError::RegistryLoginFailed);
    }
    if matches!(mode, LoginMode::Init) {
        init_success(print, "Login token for `groundhog` saved");
    }

    match mode {
        LoginMode::Standalone => print.status("\nTest executor credentials"),
        LoginMode::Init => {
            print.status("");
            init_subheading(print, "Executor credentials");
            print.status("These are used by `groundhog test`.");
        }
    }
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

    match mode {
        LoginMode::Standalone => print.status("\nCredentials saved. You're ready to go."),
        LoginMode::Init => print.status("\nCredentials saved."),
    }
    Ok(())
}

fn init_subheading(print: &Print, title: &str) {
    if std::io::stderr().is_terminal() && !print.is_quiet() && !print.is_json() {
        eprintln!("{CYAN}{BOLD}{title}{RESET}");
    } else {
        print.status(title);
    }
}

fn init_success(print: &Print, msg: &str) {
    if std::io::stderr().is_terminal() && !print.is_quiet() && !print.is_json() {
        eprintln!("{GREEN}✓{RESET} {msg}");
    } else {
        print.status(format_args!("[ok] {msg}"));
    }
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
