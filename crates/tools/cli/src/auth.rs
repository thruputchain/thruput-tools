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

/// Environment changes needed to apply config-backed credentials to a child.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ExecutorCredentialEnvironment {
    pub(crate) set: Vec<(OsString, OsString)>,
    pub(crate) remove: Vec<OsString>,
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
pub(crate) fn executor_credential_environment() -> Result<ExecutorCredentialEnvironment, CliError> {
    if environment_credentials_available() {
        return Ok(ExecutorCredentialEnvironment::default());
    }

    let Some(credentials) =
        config::load_executor_credentials()?.filter(|credentials| !credentials.password.is_empty())
    else {
        return Ok(ExecutorCredentialEnvironment::default());
    };

    let mut environment = ExecutorCredentialEnvironment {
        set: vec![(
            OsString::from(FFI_PASSWORD_ENV),
            OsString::from(credentials.password),
        )],
        remove: Vec::new(),
    };
    if credentials.username.is_empty() {
        // Config fallback is a single credential source. Clear an inherited
        // username so the SDK applies its default instead of pairing a stale
        // environment username with the config password.
        environment.remove.push(OsString::from(FFI_USER_ENV));
    } else {
        environment.set.push((
            OsString::from(FFI_USER_ENV),
            OsString::from(credentials.username),
        ));
    }
    Ok(environment)
}

fn nonempty_env(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Warn before a cargo run that will need the mock-executor FFI library.
///
/// Without credentials the SDK's build script falls back to an
/// unauthenticated download and, when that fails, cargo reports it as a
/// build-script failure many lines into its output, mentioning neither
/// `groundhog` nor what to do. Say it up front instead.
pub(crate) fn warn_if_executor_credentials_missing(print: &Print) {
    if matches!(executor_credential_source(), Ok(None)) {
        print.warn(
            "mock-executor FFI download credentials not configured; the \
             contract test runtime is downloaded during the build and this \
             will likely fail. Run `groundhog init`, or set \
             HOGD_FFI_PASSWORD (with HOGD_FFI_USER unset or nonempty).",
        );
    }
}

fn environment_credentials_available() -> bool {
    if !nonempty_env(FFI_PASSWORD_ENV) {
        return false;
    }

    env::var_os(FFI_USER_ENV).is_none_or(|user| !user.is_empty())
}

fn save_executor_credentials_from_prompt(print: &Print) -> Result<(), CliError> {
    let stdin = io::stdin();

    print.prompt("FFI download username (optional): ")?;
    let mut username = String::new();
    stdin.lock().read_line(&mut username)?;
    let username = username.trim().to_string();

    let password = rpassword::prompt_password("FFI download password: ")
        .map_err(|e| CliError::Other(e.to_string()))?;

    save_executor_credentials(username, password)
}

fn save_executor_credentials(username: String, password: String) -> Result<(), CliError> {
    if password.is_empty() {
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
            ExecutorCredentialEnvironment {
                set: vec![
                    (OsString::from(FFI_PASSWORD_ENV), OsString::from("secret")),
                    (OsString::from(FFI_USER_ENV), OsString::from("marmot")),
                ],
                remove: Vec::new(),
            }
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
        assert_eq!(
            executor_credential_environment().unwrap(),
            ExecutorCredentialEnvironment::default()
        );

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
            ExecutorCredentialEnvironment {
                set: vec![
                    (
                        OsString::from(FFI_PASSWORD_ENV),
                        OsString::from("config-secret")
                    ),
                    (OsString::from(FFI_USER_ENV), OsString::from("config-user")),
                ],
                remove: Vec::new(),
            }
        );

        unsafe {
            env::remove_var(FFI_USER_ENV);
            env::remove_var(FFI_PASSWORD_ENV);
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn password_only_config_clears_inherited_environment_user() {
        let tmp = TempDir::new().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", tmp.path());
            env::set_var(FFI_USER_ENV, "stale-user");
            env::remove_var(FFI_PASSWORD_ENV);
        }
        config::save_executor_credentials(&ExecutorCredentials {
            username: String::new(),
            password: "config-secret".into(),
        })
        .unwrap();

        assert_eq!(
            executor_credential_environment().unwrap(),
            ExecutorCredentialEnvironment {
                set: vec![(
                    OsString::from(FFI_PASSWORD_ENV),
                    OsString::from("config-secret")
                )],
                remove: vec![OsString::from(FFI_USER_ENV)],
            }
        );

        unsafe {
            env::remove_var(FFI_USER_ENV);
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn password_only_credentials_can_be_saved() {
        let tmp = TempDir::new().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        save_executor_credentials(String::new(), "secret".into()).unwrap();

        assert_eq!(
            config::load_executor_credentials().unwrap(),
            Some(ExecutorCredentials {
                username: String::new(),
                password: "secret".into(),
            })
        );

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn empty_password_is_rejected() {
        assert!(matches!(
            save_executor_credentials(String::new(), String::new()),
            Err(CliError::CredentialsEmpty)
        ));
    }
}
