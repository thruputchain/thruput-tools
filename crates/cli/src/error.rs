use serde_json::{Value, json};
use std::borrow::Cow;
use thiserror::Error;

/// All errors returned by CLI commands.
#[derive(Error, Debug)]
pub enum CliError {
    #[error("cargo not found on PATH")]
    CargoNotFound,
    #[error("wasm-opt not found on PATH")]
    WasmOptNotFound,
    #[error(
        "not a groundhog project \
         (no Cargo.toml with sdk dependency)"
    )]
    NotAProject,
    #[error("cargo failed with exit code {0}")]
    CargoFailed(i32),
    #[error("cargo failed with exit code {code}")]
    CargoFailedWithOutput {
        code: i32,
        stdout: String,
        stderr: String,
    },
    #[error("directory already exists: {0}")]
    DirExists(String),
    #[error("directory is not empty")]
    DirNotEmpty,
    #[error("unknown template: {0}")]
    UnknownTemplate(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("profile `{0}` not found in groundhog.toml")]
    ProfileNotFound(String),
    #[error("profile `{0}` requested but no groundhog.toml found")]
    NoProjectConfig(String),
    #[error("credentials cannot be empty")]
    CredentialsEmpty,
    #[error(
        "registry login failed — run `groundhog login` to try \
         again"
    )]
    RegistryLoginFailed,
    #[error("unsupported OS: {0}")]
    UnsupportedOs(String),
    #[error("wasm-opt optimization failed")]
    WasmOptFailed,
    #[error("key `{0}` already exists")]
    KeyExists(String),
    #[error("key `{0}` not found")]
    KeyNotFound(String),
    #[error("key name cannot be empty")]
    KeyNameEmpty,
    #[error(
        "key name must contain only letters, digits, \
         hyphens, and underscores"
    )]
    KeyNameInvalid,
    #[error("`--json` is not supported for `{0}`")]
    JsonUnsupported(&'static str),
    #[error("not yet implemented")]
    NotImplemented,
    #[error("status check found warnings")]
    StatusHasWarnings,
    #[error("dev node error: {0}")]
    DevNode(String),
    #[error("could not connect to {0}")]
    RpcConnect(String),
    #[error("no rpc_url configured for the selected profile")]
    MissingRpcUrl,
    #[error("no signing key configured")]
    MissingSigningKey,
    #[error("failed to initialize async runtime: {0}")]
    RuntimeInit(String),
    #[error("{method} failed: {message}")]
    RpcMethodFailed {
        method: &'static str,
        message: String,
    },
    #[error("transaction {tx_hash} submitted but no receipt after {seconds}s")]
    ReceiptTimeout { tx_hash: String, seconds: u64 },
    #[error("internal error: receipt polling exhausted unexpectedly")]
    ReceiptPollingExhausted,
    #[error("{0}")]
    Other(String),
}

impl CliError {
    /// Stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::CargoNotFound => "cargo_not_found",
            Self::WasmOptNotFound => "wasm_opt_not_found",
            Self::NotAProject => "not_a_project",
            Self::CargoFailed(_) | Self::CargoFailedWithOutput { .. } => "cargo_failed",
            Self::DirExists(_) => "dir_exists",
            Self::DirNotEmpty => "dir_not_empty",
            Self::UnknownTemplate(_) => "unknown_template",
            Self::Io(_) => "io_error",
            Self::ProfileNotFound(_) => "profile_not_found",
            Self::NoProjectConfig(_) => "no_project_config",
            Self::CredentialsEmpty => "credentials_empty",
            Self::RegistryLoginFailed => "registry_login_failed",
            Self::UnsupportedOs(_) => "unsupported_os",
            Self::WasmOptFailed => "wasm_opt_failed",
            Self::KeyExists(_) => "key_exists",
            Self::KeyNotFound(_) => "key_not_found",
            Self::KeyNameEmpty => "key_name_empty",
            Self::KeyNameInvalid => "key_name_invalid",
            Self::JsonUnsupported(_) => "json_unsupported",
            Self::NotImplemented => "not_implemented",
            Self::StatusHasWarnings => "status_has_warnings",
            Self::DevNode(_) => "dev_node_error",
            Self::RpcConnect(_) => "rpc_connect",
            Self::MissingRpcUrl => "missing_rpc_url",
            Self::MissingSigningKey => "missing_signing_key",
            Self::RuntimeInit(_) => "runtime_init_failed",
            Self::RpcMethodFailed { .. } => "rpc_method_failed",
            Self::ReceiptTimeout { .. } => "receipt_timeout",
            Self::ReceiptPollingExhausted => "receipt_polling_exhausted",
            Self::Other(_) => "other",
        }
    }

    /// Optional actionable hint for the user.
    pub fn hint(&self) -> Option<Cow<'static, str>> {
        match self {
            Self::CargoNotFound => Some(Cow::Borrowed(
                "install Rust and ensure `cargo` is on your PATH",
            )),
            Self::WasmOptNotFound => Some(Cow::Borrowed(
                "install Binaryen (`brew install binaryen`), or \
                 rerun without `--optimize`",
            )),
            Self::NotAProject => Some(Cow::Borrowed(
                "run `groundhog new my-project`, or change into an \
                 existing Groundhog project root",
            )),
            Self::CargoFailed(_) | Self::CargoFailedWithOutput { .. } => Some(Cow::Borrowed(
                "see the cargo output above for the underlying build \
                 or test failure",
            )),
            Self::DirExists(path) => Some(Cow::Owned(format!(
                "remove `{path}`, or choose a different project name"
            ))),
            Self::DirNotEmpty => Some(Cow::Borrowed(
                "use an empty directory, or pick a different target path",
            )),
            Self::UnknownTemplate(_) => {
                Some(Cow::Borrowed("available templates: `counter`, `stub`"))
            }
            Self::DevNode(_) => Some(Cow::Borrowed("check the dev node output for details")),
            Self::RpcConnect(url) => Some(Cow::Owned(format!(
                "run `groundhog node` in another terminal, or check that \
                 the RPC endpoint at {url} is running"
            ))),
            Self::MissingRpcUrl => Some(Cow::Borrowed(
                "set `rpc_url` in the selected profile in `groundhog.toml`",
            )),
            Self::MissingSigningKey => Some(Cow::Borrowed(
                "set `default_key` in `groundhog.toml`, or run `groundhog keys generate`",
            )),
            Self::RuntimeInit(_) => None,
            Self::RpcMethodFailed { .. } => Some(Cow::Borrowed(
                "check the RPC endpoint and node logs for details",
            )),
            Self::ReceiptTimeout { .. } => Some(Cow::Borrowed(
                "check the node logs, then rerun the command once the transaction is committed or rejected",
            )),
            Self::ReceiptPollingExhausted => None,
            Self::Io(_) | Self::Other(_) | Self::StatusHasWarnings => None,
            Self::ProfileNotFound(_) => Some(Cow::Borrowed(
                "add the profile to `groundhog.toml`, or switch to \
                 an existing one with `--profile` or \
                 `GROUNDHOG_PROFILE`",
            )),
            Self::NoProjectConfig(_) => Some(Cow::Borrowed(
                "create `groundhog.toml`, unset `GROUNDHOG_PROFILE`, \
                 or use the default profile",
            )),
            Self::CredentialsEmpty => Some(Cow::Borrowed(
                "rerun `groundhog login` and provide both username \
                 and password",
            )),
            Self::RegistryLoginFailed => Some(Cow::Borrowed(
                "check your registry token and rerun \
                 `groundhog login`",
            )),
            Self::UnsupportedOs(_) => Some(Cow::Borrowed(
                "`groundhog test` currently supports macOS and \
                 Linux host targets",
            )),
            Self::WasmOptFailed => Some(Cow::Borrowed(
                "check the wasm-opt output above; the WASM binary \
                 may be invalid",
            )),
            Self::KeyExists(name) => Some(Cow::Owned(format!(
                "use a different name, or remove the existing key \
                 with `groundhog keys rm {name}`"
            ))),
            Self::KeyNotFound(_) => Some(Cow::Borrowed(
                "list available keys with `groundhog keys ls`",
            )),
            Self::KeyNameEmpty | Self::KeyNameInvalid => Some(Cow::Borrowed(
                "key names may only contain letters, digits, \
                 hyphens, and underscores",
            )),
            Self::JsonUnsupported(command) => Some(Cow::Owned(format!(
                "run `groundhog {command}` without `--json`"
            ))),
            Self::NotImplemented => Some(Cow::Borrowed(
                "this command is planned for a future release",
            )),
        }
    }

    /// Optional structured details for JSON error payloads.
    pub fn details_json(&self) -> Option<Value> {
        match self {
            Self::CargoFailedWithOutput { stdout, stderr, .. } => Some(json!({
                "stdout": stdout,
                "stderr": stderr,
            })),
            _ => None,
        }
    }
}
