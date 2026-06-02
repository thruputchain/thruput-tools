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
    #[error("`.cargo/contract.toml` not found in {0}")]
    ContractTomlNotFound(String),
    #[error("package `{0}` not found in workspace")]
    PackageNotFound(String),
    #[error("could not determine package — specify one with `-p <name>`")]
    PackageRequired,
    #[error("cargo metadata failed: {0}")]
    CargoMetadataFailed(String),
    #[error("expected WASM artifact not found at {0}")]
    WasmNotFound(String),
    #[error("expected RV32 ELF not found at {0}")]
    Rv32ElfNotFound(String),
    #[error("target dir conflicts with CLI passthrough args or config")]
    TargetDirConflict,
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
    #[error("wasm-opt optimization failed{}", if .0.is_empty() { String::new() } else { format!(": {}", .0) })]
    WasmOptFailed(String),
    #[error("RV32 lowering failed: {0}")]
    Rv32LowerFailed(String),
    #[error("WASM instrumentation failed: {0}")]
    WasmInstrumentFailed(#[from] groundhog_wasm_meter::Error),
    #[error("WASM verification failed after instrumentation: {0}")]
    WasmVerifyFailed(String),
    #[error("contract spec finalization failed: {0}")]
    SpecFailed(String),
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
    #[error("`groundhog test --integration` requires the dev-node feature")]
    IntegrationTestsUnavailable,
    #[error("integration test target not found at {0}")]
    IntegrationTestNotFound(String),
    #[error("invalid test options: {0}")]
    InvalidTestOptions(String),
    #[error("invalid build options: {0}")]
    InvalidBuildOptions(String),
    #[error("status check found warnings")]
    StatusHasWarnings,
    #[error("command emitted structured failure output")]
    CommandReportedFailure,
    #[error("node supervisor error: {0}")]
    NodeSupervisor(String),
    #[error("node supervisor error: {message}")]
    NodeSupervisorProtocolIntroRejected {
        message: String,
        expected_protocol: Option<String>,
    },
    #[error("local node already running: {0}")]
    LocalNodeAlreadyRunning(String),
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
    #[error("dev key `{key_name}` requires a local dev node: {message}")]
    LocalDevAccountUnavailable {
        /// Dev key name that required local dev-account metadata.
        key_name: String,
        /// Specific metadata lookup or validation failure.
        message: String,
    },
    #[error("failed to derive wallet address for key `{key_name}`: {message}")]
    WalletAddressDerivationFailed {
        /// Key name whose pubkey-derived wallet address could not be derived.
        key_name: String,
        /// Specific derivation failure.
        message: String,
    },
    #[error("transaction {tx_hash} submitted but no receipt after {seconds}s")]
    ReceiptTimeout { tx_hash: String, seconds: u64 },
    #[error("internal error: receipt polling exhausted unexpectedly")]
    ReceiptPollingExhausted,
    #[error("{path}: {message}")]
    CallEncode { path: String, message: String },
    #[error("{path}: {message}")]
    CallDecode { path: String, message: String },
    #[error("invalid storage key: {0}")]
    InvalidStorageKey(String),
    #[error("invalid view calldata: {0}")]
    InvalidViewCalldata(String),
    #[error("invalid view arguments: {0}")]
    InvalidViewArguments(String),
    #[error("codegen failed: {0}")]
    CodegenFailed(String),
    #[error("{0}")]
    Other(String),
}

impl CliError {
    /// Stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::CargoNotFound => "cargo_not_found",
            Self::WasmOptNotFound => "wasm_opt_not_found",
            Self::ContractTomlNotFound(_) => "contract_toml_not_found",
            Self::PackageNotFound(_) => "package_not_found",
            Self::PackageRequired => "package_required",
            Self::CargoMetadataFailed(_) => "cargo_metadata_failed",
            Self::WasmNotFound(_) => "wasm_not_found",
            Self::Rv32ElfNotFound(_) => "rv32_elf_not_found",
            Self::TargetDirConflict => "target_dir_conflict",
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
            Self::WasmOptFailed(_) => "wasm_opt_failed",
            Self::Rv32LowerFailed(_) => "rv32_lower_failed",
            Self::WasmInstrumentFailed(_) => "wasm_instrument_failed",
            Self::WasmVerifyFailed(_) => "wasm_verify_failed",
            Self::SpecFailed(_) => "spec_failed",
            Self::KeyExists(_) => "key_exists",
            Self::KeyNotFound(_) => "key_not_found",
            Self::KeyNameEmpty => "key_name_empty",
            Self::KeyNameInvalid => "key_name_invalid",
            Self::JsonUnsupported(_) => "json_unsupported",
            Self::NotImplemented => "not_implemented",
            Self::IntegrationTestsUnavailable => "integration_tests_unavailable",
            Self::IntegrationTestNotFound(_) => "integration_test_not_found",
            Self::InvalidTestOptions(_) => "invalid_test_options",
            Self::InvalidBuildOptions(_) => "invalid_build_options",
            Self::StatusHasWarnings => "status_has_warnings",
            Self::CommandReportedFailure => "command_reported_failure",
            Self::NodeSupervisor(_) => "node_supervisor_error",
            Self::NodeSupervisorProtocolIntroRejected { .. } => "protocol_intro_rejected",
            Self::LocalNodeAlreadyRunning(_) => "local_node_already_running",
            Self::RpcConnect(_) => "rpc_connect",
            Self::MissingRpcUrl => "missing_rpc_url",
            Self::MissingSigningKey => "missing_signing_key",
            Self::RuntimeInit(_) => "runtime_init_failed",
            Self::RpcMethodFailed { .. } => "rpc_method_failed",
            Self::LocalDevAccountUnavailable { .. } => "local_dev_account_unavailable",
            Self::WalletAddressDerivationFailed { .. } => "wallet_address_derivation_failed",
            Self::ReceiptTimeout { .. } => "receipt_timeout",
            Self::ReceiptPollingExhausted => "receipt_polling_exhausted",
            Self::CallEncode { .. } => "call_encode",
            Self::CallDecode { .. } => "call_decode",
            Self::InvalidStorageKey(_) => "invalid_storage_key",
            Self::InvalidViewCalldata(_) => "invalid_view_calldata",
            Self::InvalidViewArguments(_) => "invalid_view_arguments",
            Self::CodegenFailed(_) => "codegen_failed",
            Self::Other(_) => "other",
        }
    }

    /// Optional actionable hint for the user.
    #[allow(clippy::too_many_lines)]
    pub fn hint(&self) -> Option<Cow<'static, str>> {
        match self {
            Self::CargoNotFound => Some(Cow::Borrowed(
                "install Rust and ensure `cargo` is on your PATH",
            )),
            Self::WasmOptNotFound => Some(Cow::Borrowed(
                "install Binaryen (`brew install binaryen`), or \
                 rerun without `--optimize`",
            )),
            Self::ContractTomlNotFound(_) => Some(Cow::Borrowed(
                "create `.cargo/contract.toml` with WASM build flags, \
                 or run `groundhog new` to scaffold a project",
            )),
            Self::PackageNotFound(name) => Some(Cow::Owned(format!(
                "available packages can be listed with \
                 `cargo metadata --no-deps`; \
                 check that `{name}` is spelled correctly"
            ))),
            Self::PackageRequired => Some(Cow::Borrowed(
                "run from inside a package directory, or pass \
                 `-p <package>` to select one",
            )),
            Self::CargoMetadataFailed(_) => Some(Cow::Borrowed(
                "ensure `cargo` is on PATH and you are inside \
                 a Cargo project or workspace",
            )),
            Self::WasmNotFound(_) => Some(Cow::Borrowed(
                "run `groundhog build` first, or check that the crate \
                 does not use a custom [lib] name in Cargo.toml",
            )),
            Self::Rv32ElfNotFound(_) => Some(Cow::Borrowed(
                "check that the package builds a binary for \
                 riscv32im-unknown-none-elf and that its binary name matches \
                 the package name",
            )),
            Self::TargetDirConflict => Some(Cow::Borrowed(
                "remove `--target-dir` from passthrough args or \
                 config when using the CLI target directory option",
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
            Self::NodeSupervisor(_) => Some(Cow::Borrowed(
                "check `--hogd-dev-binary`, the hogd-dev process output, and the local node logs",
            )),
            Self::NodeSupervisorProtocolIntroRejected {
                expected_protocol: Some(protocol),
                ..
            } => Some(Cow::Owned(format!(
                "pass `--hogd-protocol {protocol}` to override, or rebuild groundhog node \
                 against a matching groundhog-xdr"
            ))),
            Self::NodeSupervisorProtocolIntroRejected {
                expected_protocol: None,
                ..
            } => Some(Cow::Borrowed(
                "pass `--hogd-protocol <hogd-protocol>` to override, or rebuild groundhog node \
                 against a matching groundhog-xdr",
            )),
            Self::LocalNodeAlreadyRunning(_) => Some(Cow::Borrowed(
                "stop the running `groundhog node` process before starting another local node",
            )),
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
            Self::RpcMethodFailed { .. } => Some(Cow::Borrowed(
                "check the RPC endpoint and node logs for details",
            )),
            Self::LocalDevAccountUnavailable { .. } => Some(Cow::Borrowed(
                "run `groundhog node` backed by hogd-dev, or use a stored key for non-local endpoints",
            )),
            Self::WalletAddressDerivationFailed { .. } => Some(Cow::Borrowed(
                "check the stored public key material, or regenerate the key",
            )),
            Self::ReceiptTimeout { .. } => Some(Cow::Borrowed(
                "check the node logs, then rerun the command once the transaction is committed or rejected",
            )),
            Self::CallEncode { .. } => Some(Cow::Borrowed(
                "check the reported argument path and use structured JSON for composite contract values",
            )),
            Self::CallDecode { .. } => Some(Cow::Borrowed(
                "check the reported return-value path and verify the resolved contract types match the on-chain value",
            )),
            Self::InvalidStorageKey(_) => Some(Cow::Borrowed(
                "pass a 32-byte storage key as 0x-prefixed hex",
            )),
            Self::InvalidViewCalldata(_) => Some(Cow::Borrowed(
                "pass raw calldata as 0x-prefixed hex, or omit it for empty calldata",
            )),
            Self::InvalidViewArguments(_) => Some(Cow::Borrowed(
                "use either positional method arguments or `--calldata`, not both",
            )),
            Self::CodegenFailed(_) => Some(Cow::Borrowed(
                "check that the file contains a valid embedded contract spec",
            )),
            Self::RuntimeInit(_)
            | Self::ReceiptPollingExhausted
            | Self::Io(_)
            | Self::Other(_)
            | Self::StatusHasWarnings
            | Self::CommandReportedFailure => None,
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
            Self::WasmOptFailed(_) => Some(Cow::Borrowed(
                "see the wasm-opt error above; the WASM binary \
                 may be invalid or malformed",
            )),
            Self::Rv32LowerFailed(_) => Some(Cow::Borrowed(
                "the RV32 ELF may be stripped, missing `_start`, or missing \
                 contract entrypoint symbols",
            )),
            Self::WasmInstrumentFailed(_) => {
                Some(Cow::Borrowed("the WASM binary may be invalid or too large"))
            }
            Self::WasmVerifyFailed(_) => Some(Cow::Borrowed(
                "this is a bug — please report this instrument→verify \
                 failure to the Groundhog team",
            )),
            Self::SpecFailed(_) => Some(Cow::Borrowed(
                "check that your contract uses `#[contract]` and \
                 `#[contracttype]` attributes correctly",
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
            Self::IntegrationTestsUnavailable => Some(Cow::Borrowed(
                "build the CLI with `--features dev-node`, for example: \
                 `cargo install --path crates/tools/cli --features dev-node`",
            )),
            Self::IntegrationTestNotFound(_) => Some(Cow::Borrowed(
                "create `tests/integration.rs` before running `groundhog test --integration`",
            )),
            Self::InvalidTestOptions(_) => Some(Cow::Borrowed(
                "remove the conflicting `groundhog test` option and rerun the command",
            )),
            Self::InvalidBuildOptions(_) => Some(Cow::Borrowed(
                "remove the unsupported `groundhog build` option for the selected target",
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
            Self::Rv32LowerFailed(message) => Some(json!({ "message": message })),
            Self::CallEncode { path, message } | Self::CallDecode { path, message } => {
                Some(json!({
                    "path": path,
                    "message": message,
                }))
            }
            _ => None,
        }
    }
}
