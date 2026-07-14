use crate::config::{self, ExecutorCredentials};
use crate::{CliError, Print};
use serde::Serialize;
use std::env;
use std::ffi::OsString;
use std::io::{self, BufRead};
use std::path::PathBuf;

const FFI_USER_ENV: &str = "HOGD_FFI_USER";
const FFI_PASSWORD_ENV: &str = "HOGD_FFI_PASSWORD";

/// Source supplying credentials for mock-executor FFI downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutorCredentialSource {
    Environment,
    Config,
}

/// Resolve the user's cargo config path.
pub fn cargo_config_path() -> Result<PathBuf, CliError> {
    let cargo_home = env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| config::home_dir().map(|h| h.join(".cargo")))?;
    Ok(cargo_home.join("config.toml"))
}

/// Store mock-executor download credentials during `groundhog init`.
pub(crate) fn configure_executor_credentials_from_init(print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        return Err(CliError::JsonUnsupported("init"));
    }

    print.status(
        "These are used by `groundhog test` when FFI credentials are not in the environment.",
    );
    save_executor_credentials_from_prompt(print)?;
    print.status("\nCredentials saved.");
    Ok(())
}

/// Resolve whether mock-executor FFI download credentials are available.
///
/// `HOGD_FFI_PASSWORD` takes precedence over credentials stored in the
/// Groundhog global config. `HOGD_FFI_USER` is optional because the SDK
/// supplies its own default username.
pub fn executor_credential_source() -> Result<Option<ExecutorCredentialSource>, CliError> {
    if environment_credentials_available() {
        return Ok(Some(ExecutorCredentialSource::Environment));
    }

    Ok(config::load_executor_credentials()?
        .filter(|credentials| !credentials.password.is_empty())
        .map(|_| ExecutorCredentialSource::Config))
}

/// Return config-backed FFI credentials for a child process when the
/// environment does not already provide them.
pub(crate) fn executor_credential_environment() -> Result<Vec<(OsString, OsString)>, CliError> {
    if environment_credentials_available() {
        return Ok(Vec::new());
    }

    let Some(credentials) =
        config::load_executor_credentials()?.filter(|credentials| !credentials.password.is_empty())
    else {
        return Ok(Vec::new());
    };

    let mut environment = vec![(
        OsString::from(FFI_PASSWORD_ENV),
        OsString::from(credentials.password),
    )];
    if !credentials.username.is_empty() {
        environment.push((
            OsString::from(FFI_USER_ENV),
            OsString::from(credentials.username),
        ));
    }
    Ok(environment)
}

fn nonempty_env(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn environment_credentials_available() -> bool {
    if !nonempty_env(FFI_PASSWORD_ENV) {
        return false;
    }

    env::var_os(FFI_USER_ENV).is_none_or(|user| !user.is_empty())
}

fn save_executor_credentials_from_prompt(print: &Print) -> Result<(), CliError> {
    let stdin = io::stdin();

    print.prompt("FFI download username: ")?;
    let mut username = String::new();
    stdin.lock().read_line(&mut username)?;
    let username = username.trim().to_string();

    let password = rpassword::prompt_password("FFI download password: ")
        .map_err(|e| CliError::Other(e.to_string()))?;

    if username.is_empty() || password.is_empty() {
        return Err(CliError::CredentialsEmpty);
    }

    config::save_executor_credentials(&ExecutorCredentials { username, password })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn config_credentials_are_prepared_for_child_processes() {
        let tmp = TempDir::new().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", tmp.path());
            env::remove_var(FFI_USER_ENV);
            env::remove_var(FFI_PASSWORD_ENV);
        }
        config::save_executor_credentials(&ExecutorCredentials {
            username: "marmot".into(),
            password: "secret".into(),
        })
        .unwrap();

        assert_eq!(
            executor_credential_source().unwrap(),
            Some(ExecutorCredentialSource::Config)
        );
        assert_eq!(
            executor_credential_environment().unwrap(),
            vec![
                (OsString::from(FFI_PASSWORD_ENV), OsString::from("secret")),
                (OsString::from(FFI_USER_ENV), OsString::from("marmot")),
            ]
        );

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn environment_credentials_take_precedence_over_config() {
        let tmp = TempDir::new().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", tmp.path());
            env::remove_var(FFI_USER_ENV);
            env::set_var(FFI_PASSWORD_ENV, "environment-secret");
        }
        config::save_executor_credentials(&ExecutorCredentials {
            username: "config-user".into(),
            password: "config-secret".into(),
        })
        .unwrap();

        assert_eq!(
            executor_credential_source().unwrap(),
            Some(ExecutorCredentialSource::Environment)
        );
        assert!(executor_credential_environment().unwrap().is_empty());

        unsafe {
            env::remove_var(FFI_PASSWORD_ENV);
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn empty_environment_user_falls_back_to_config() {
        let tmp = TempDir::new().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", tmp.path());
            env::set_var(FFI_USER_ENV, "");
            env::set_var(FFI_PASSWORD_ENV, "environment-secret");
        }
        config::save_executor_credentials(&ExecutorCredentials {
            username: "config-user".into(),
            password: "config-secret".into(),
        })
        .unwrap();

        assert_eq!(
            executor_credential_source().unwrap(),
            Some(ExecutorCredentialSource::Config)
        );
        assert_eq!(
            executor_credential_environment().unwrap(),
            vec![
                (
                    OsString::from(FFI_PASSWORD_ENV),
                    OsString::from("config-secret")
                ),
                (OsString::from(FFI_USER_ENV), OsString::from("config-user")),
            ]
        );

        unsafe {
            env::remove_var(FFI_USER_ENV);
            env::remove_var(FFI_PASSWORD_ENV);
            env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
