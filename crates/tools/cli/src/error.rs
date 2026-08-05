use serde_json::{Value, json};
use std::borrow::Cow;
use thiserror::Error;

use crate::terminal::{render_shell_arg, terminal_safe};

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
    #[error("expected {kind} artifact not found at {path}")]
    ArtifactNotFound { kind: &'static str, path: String },
    #[error("unsupported artifact extension for {0}; expected .wasm or .rv32a")]
    UnsupportedArtifactExtension(String),
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
    #[error("profile `{0}` not found in project or global config")]
    ProfileNotFound(String),
    #[error("profile `{profile}` not found in {scope} config")]
    ProfileNotFoundInScope {
        profile: String,
        scope: &'static str,
    },
    #[error("profile `{profile}` already exists in {scope} config")]
    ProfileAlreadyExists {
        profile: String,
        scope: &'static str,
    },
    #[error("no active profile selected")]
    NoActiveProfile,
    #[error("invalid profile name: {0}")]
    ProfileNameInvalid(&'static str),
    #[error("unsupported config field `{0}`")]
    ConfigFieldUnsupported(String),
    #[error("invalid value for config field `{field}`: {message}")]
    ConfigValueInvalid { field: String, message: String },
    #[error("failed to parse config `{}`: {message}", .path.display())]
    ConfigParse {
        path: std::path::PathBuf,
        message: String,
        line: Option<usize>,
        column: Option<usize>,
    },
    #[error("invalid config `{}`: {message}", .path.display())]
    ConfigFileInvalid {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("project config not found")]
    ProjectConfigUnavailable,
    #[error("`--global` and `--project` cannot be used together")]
    ConfigScopeConflict,
    #[error("config scope is ambiguous because a project config exists")]
    ConfigScopeAmbiguous,
    #[error("config `{}` is busy; another process may be updating it", .path.display())]
    ConfigLockTimeout { path: std::path::PathBuf },
    #[error("config `{}` changed while awaiting confirmation", .path.display())]
    ConfigChangedDuringConfirmation { path: std::path::PathBuf },
    #[error("credential password cannot be empty")]
    CredentialsEmpty,
    #[error("unsupported OS: {0}")]
    UnsupportedOs(String),
    #[error("wasm-opt optimization failed{}", if .0.is_empty() { String::new() } else { format!(": {}", .0) })]
    WasmOptFailed(String),
    #[error("RV32 lowering failed: {0}")]
    Rv32LowerFailed(#[from] groundhog_rv32_lower::LowerError),
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
    #[error("wallet name cannot be empty")]
    EmptyWalletName,
    #[error(
        "key name must contain only letters, digits, \
         hyphens, and underscores"
    )]
    KeyNameInvalid,
    #[error("`--json` is not supported for `{0}`")]
    JsonUnsupported(&'static str),
    #[error("not yet implemented")]
    NotImplemented,
    #[error("integration test target not found at {0}")]
    IntegrationTestNotFound(String),
    #[error("invalid test options: {0}")]
    InvalidTestOptions(String),
    #[error("invalid build options: {0}")]
    InvalidBuildOptions(String),
    #[error(
        "`{0}` is set in the environment, so cargo would ignore the project's \
         `.cargo/config.toml` target rustflags (Groundhog linker and codegen flags) and \
         produce a broken artifact"
    )]
    EnvRustflagsSet(&'static str),
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
    #[error("profile `{profile}` {message}")]
    ProfileConfigInvalid { profile: String, message: String },
    #[error("no signing key configured")]
    MissingSigningKey,
    #[error("invalid HOG amount `{input}`: {message}")]
    InvalidHogAmount { input: String, message: String },
    #[error("address target {target:?} is invalid: {message}")]
    InvalidAddressTarget { target: String, message: String },
    #[error("unknown address target {target:?}")]
    UnknownAddressTarget { target: String },
    #[error("invalid transaction hash `{display_input}`: {message}")]
    InvalidTransactionHash {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid transaction history limit `{display_input}`: {message}")]
    InvalidTransactionHistoryLimit {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid transaction cursor `{display_input}`: {message}")]
    InvalidTransactionCursor {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event {position} value `{display_input}`: {message}")]
    InvalidEventTopic {
        position: &'static str,
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event filter: {position} has {count} values; maximum is {max}")]
    InvalidEventFilter {
        position: &'static str,
        count: usize,
        max: u32,
    },
    #[error("invalid event limit `{display_input}`: {message}")]
    InvalidEventLimit {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event cursor `{display_input}`: {message}")]
    InvalidEventCursor {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event options: {message}")]
    InvalidEventOptions { message: String },
    #[error("invalid token transfer limit `{display_input}`: {message}")]
    InvalidTokenTransferLimit {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid token transfer cursor `{display_input}`: {message}")]
    InvalidTokenTransferCursor {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid token transfer options: {message}")]
    InvalidTokenTransferOptions { message: String },
    #[error("invalid event spec `{display_path}`: {message}")]
    InvalidEventSpec {
        path: String,
        display_path: String,
        message: String,
    },
    #[error("failed to initialize async runtime: {0}")]
    RuntimeInit(String),
    #[error("{method} failed: {message}")]
    RpcMethodFailed {
        method: &'static str,
        message: String,
    },
    #[error("RPC endpoint does not support {method}: {message}")]
    RpcCapabilityUnsupported {
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
    #[error("runtime manifest entrypoint `{field}` for key `{key_name}` is invalid: {message}")]
    ManifestEntrypoint {
        /// Dev key name whose configured runtime manifest was read.
        key_name: String,
        /// Runtime manifest field path containing the malformed entrypoint.
        field: &'static str,
        /// Specific parse or validation failure.
        message: String,
    },
    #[error("hosted network descriptor field `{field}` is invalid: {message}")]
    NetworkDescriptorInvalid {
        /// Public descriptor field containing invalid data.
        field: &'static str,
        /// Specific validation failure.
        message: String,
    },
    #[error(
        "hosted network descriptor field `{field}` does not match the selected profile: expected `{expected}`, got `{actual}`"
    )]
    NetworkDescriptorMismatch {
        /// Public descriptor field that did not match local expectations.
        field: &'static str,
        /// Value expected from the profile or runtime manifest.
        expected: String,
        /// Value advertised by the public descriptor.
        actual: String,
    },
    #[error("failed to derive wallet address for key `{key_name}`: {message}")]
    WalletAddressDerivationFailed {
        /// Key name whose pubkey-derived wallet address could not be derived.
        key_name: String,
        /// Specific derivation failure.
        message: String,
    },
    #[error(
        "no wallet saved for key `{}` in profile `{}`; run `groundhog wallet create --key={} --profile={}`",
        terminal_safe(.key_name.as_str()),
        terminal_safe(.profile.as_str()),
        render_shell_arg(.key_name.as_str()),
        render_shell_arg(.profile.as_str()),
    )]
    SavedWalletMissing { key_name: String, profile: String },
    #[error(
        "multiple wallets are saved for key `{key_name}` in profile `{profile}`: {wallet_names:?}"
    )]
    SavedWalletAmbiguous {
        key_name: String,
        profile: String,
        wallet_names: Vec<String>,
    },
    #[error("wallet `{wallet_name}` is not saved in profile `{profile}`")]
    SavedWalletNotFound {
        wallet_name: String,
        profile: String,
    },
    #[error(
        "wallet `{wallet_name}` owner public key {configured} does not match key `{key_name}` public key {local}"
    )]
    SavedWalletOwnerMismatch {
        wallet_name: String,
        key_name: String,
        configured: String,
        local: String,
    },
    #[error("dev key `{key_name}` uses its manifest wallet and cannot select `{wallet_name}`")]
    DevWalletSelectionUnsupported {
        key_name: String,
        wallet_name: String,
    },
    #[error(
        "cannot remove wallet `{wallet_name}` from profile `{profile}` because it is the inherited default wallet"
    )]
    InheritedDefaultWalletRemoval {
        wallet_name: String,
        profile: String,
    },
    #[error(
        "cannot remove wallet `{wallet_name}` from profile `{profile}` because it would expose inherited default wallet `{inherited_wallet}`, which is not saved"
    )]
    WalletRemovalExposesMissingDefault {
        wallet_name: String,
        inherited_wallet: String,
        profile: String,
    },
    #[error("saved wallet `{key_name}` owner_public_key is invalid: {message}")]
    SavedWalletOwnerPublicKeyInvalid { key_name: String, message: String },
    #[error("transfer sender and recipient resolve to the same address `{address}`")]
    TransferToSelf { address: String },
    #[error("transaction {tx_hash} submitted but no receipt after {seconds}s")]
    ReceiptTimeout { tx_hash: String, seconds: u64 },
    #[error("transaction {tx_hash} was rejected: {reason}")]
    TransactionInvalid { tx_hash: String, reason: String },
    #[error(
        "transaction {tx_hash} failed at height {attempt_height}: {conflict_code} conflict at {conflict_address}"
    )]
    TransactionRecentFailure {
        tx_hash: String,
        attempt_height: i64,
        conflict_address: String,
        conflict_key: String,
        conflict_code: String,
    },
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
    #[allow(clippy::too_many_lines)]
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
            Self::ArtifactNotFound { .. } => "artifact_not_found",
            Self::UnsupportedArtifactExtension(_) => "unsupported_artifact_extension",
            Self::TargetDirConflict => "target_dir_conflict",
            Self::CargoFailed(_) | Self::CargoFailedWithOutput { .. } => "cargo_failed",
            Self::DirExists(_) => "dir_exists",
            Self::DirNotEmpty => "dir_not_empty",
            Self::UnknownTemplate(_) => "unknown_template",
            Self::Io(_) => "io_error",
            Self::ProfileNotFound(_) => "profile_not_found",
            Self::ProfileNotFoundInScope { .. } => "profile_not_found_in_scope",
            Self::ProfileAlreadyExists { .. } => "profile_already_exists",
            Self::NoActiveProfile => "no_active_profile",
            Self::ProfileNameInvalid(_) => "profile_name_invalid",
            Self::ConfigFieldUnsupported(_) => "config_field_unsupported",
            Self::ConfigValueInvalid { .. } => "config_value_invalid",
            Self::ConfigParse { .. } => "config_parse_failed",
            Self::ConfigFileInvalid { .. } => "config_file_invalid",
            Self::ProjectConfigUnavailable => "project_config_unavailable",
            Self::ConfigScopeConflict => "config_scope_conflict",
            Self::ConfigScopeAmbiguous => "config_scope_ambiguous",
            Self::ConfigLockTimeout { .. } => "config_lock_timeout",
            Self::ConfigChangedDuringConfirmation { .. } => "config_changed_during_confirmation",
            Self::CredentialsEmpty => "credentials_empty",
            Self::UnsupportedOs(_) => "unsupported_os",
            Self::WasmOptFailed(_) => "wasm_opt_failed",
            Self::Rv32LowerFailed(_) => "rv32_lower_failed",
            Self::WasmInstrumentFailed(_) => "wasm_instrument_failed",
            Self::WasmVerifyFailed(_) => "wasm_verify_failed",
            Self::SpecFailed(_) => "spec_failed",
            Self::KeyExists(_) => "key_exists",
            Self::KeyNotFound(_) => "key_not_found",
            Self::KeyNameEmpty => "key_name_empty",
            Self::EmptyWalletName => "empty_wallet_name",
            Self::KeyNameInvalid => "key_name_invalid",
            Self::JsonUnsupported(_) => "json_unsupported",
            Self::NotImplemented => "not_implemented",
            Self::IntegrationTestNotFound(_) => "integration_test_not_found",
            Self::InvalidTestOptions(_) => "invalid_test_options",
            Self::InvalidBuildOptions(_) => "invalid_build_options",
            Self::EnvRustflagsSet(_) => "env_rustflags_set",
            Self::StatusHasWarnings => "status_has_warnings",
            Self::CommandReportedFailure => "command_reported_failure",
            Self::NodeSupervisor(_) => "node_supervisor_error",
            Self::NodeSupervisorProtocolIntroRejected { .. } => "protocol_intro_rejected",
            Self::LocalNodeAlreadyRunning(_) => "local_node_already_running",
            Self::RpcConnect(_) => "rpc_connect",
            Self::MissingRpcUrl => "missing_rpc_url",
            Self::ProfileConfigInvalid { .. } => "profile_config_invalid",
            Self::MissingSigningKey => "missing_signing_key",
            Self::InvalidHogAmount { .. } => "invalid_hog_amount",
            Self::InvalidAddressTarget { .. } => "invalid_address_target",
            Self::UnknownAddressTarget { .. } => "unknown_address_target",
            Self::InvalidTransactionHash { .. } => "invalid_transaction_hash",
            Self::InvalidTransactionHistoryLimit { .. } => "invalid_transaction_history_limit",
            Self::InvalidTransactionCursor { .. } => "invalid_transaction_cursor",
            Self::InvalidEventTopic { .. } => "invalid_event_topic",
            Self::InvalidEventFilter { .. } => "invalid_event_filter",
            Self::InvalidEventLimit { .. } | Self::InvalidTokenTransferLimit { .. } => {
                "invalid_event_limit"
            }
            Self::InvalidEventCursor { .. } | Self::InvalidTokenTransferCursor { .. } => {
                "invalid_event_cursor"
            }
            Self::InvalidEventOptions { .. } | Self::InvalidTokenTransferOptions { .. } => {
                "invalid_event_options"
            }
            Self::InvalidEventSpec { .. } => "invalid_event_spec",
            Self::RuntimeInit(_) => "runtime_init_failed",
            Self::RpcMethodFailed { .. } => "rpc_method_failed",
            Self::RpcCapabilityUnsupported { .. } => "rpc_capability_unsupported",
            Self::LocalDevAccountUnavailable { .. } => "local_dev_account_unavailable",
            Self::ManifestEntrypoint { .. } => "manifest_entrypoint_invalid",
            Self::NetworkDescriptorInvalid { .. } => "network_descriptor_invalid",
            Self::NetworkDescriptorMismatch { .. } => "network_descriptor_mismatch",
            Self::WalletAddressDerivationFailed { .. } => "wallet_address_derivation_failed",
            Self::SavedWalletMissing { .. } => "saved_wallet_missing",
            Self::SavedWalletAmbiguous { .. } => "saved_wallet_ambiguous",
            Self::SavedWalletNotFound { .. } => "saved_wallet_not_found",
            Self::SavedWalletOwnerMismatch { .. } => "saved_wallet_owner_mismatch",
            Self::DevWalletSelectionUnsupported { .. } => "dev_wallet_selection_unsupported",
            Self::InheritedDefaultWalletRemoval { .. } => "inherited_default_wallet_removal",
            Self::WalletRemovalExposesMissingDefault { .. } => {
                "wallet_removal_exposes_missing_default"
            }
            Self::SavedWalletOwnerPublicKeyInvalid { .. } => {
                "saved_wallet_owner_public_key_invalid"
            }
            Self::TransferToSelf { .. } => "transfer_to_self",
            Self::ReceiptTimeout { .. } => "receipt_timeout",
            Self::TransactionInvalid { .. } => "transaction_invalid",
            Self::TransactionRecentFailure { .. } => "transaction_recent_failure",
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
            Self::ArtifactNotFound { kind, .. } => Some(Cow::Owned(format!(
                "run `groundhog build --target {kind}` first, or pass `--artifact <path>`"
            ))),
            Self::UnsupportedArtifactExtension(_) => {
                Some(Cow::Borrowed("pass a `.wasm` or `.rv32a` artifact path"))
            }
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
                "check `--hog-devnode-binary`, the hog-devnode process output, and the local node logs",
            )),
            Self::NodeSupervisorProtocolIntroRejected {
                expected_protocol: Some(protocol),
                ..
            } => Some(Cow::Owned(format!(
                "pass `--hogd-protocol={}` to override, or rebuild groundhog node \
                 against a matching groundhog-xdr",
                render_shell_arg(protocol),
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
                "set `rpc_url` in the selected profile in project or global config",
            )),
            Self::ProfileConfigInvalid { .. } => Some(Cow::Borrowed(
                "update the selected profile in project or global config, or choose a profile that matches the requested command",
            )),
            Self::MissingSigningKey => Some(Cow::Borrowed(
                "set `default_key` in project or global config, or run `groundhog keys generate`",
            )),
            Self::InvalidHogAmount { .. } => Some(Cow::Borrowed(
                "enter a positive decimal HOG amount with at most seven decimal places",
            )),
            Self::InvalidAddressTarget { .. } | Self::UnknownAddressTarget { .. } => {
                Some(Cow::Borrowed(
                    "use a saved wallet, contract alias, dev-N account, or 32-byte 0x-prefixed address",
                ))
            }
            Self::InvalidTransactionHash { .. } | Self::InvalidEventTopic { .. } => {
                Some(Cow::Borrowed(
                    "pass exactly 32 bytes as case-insensitive hex, with or without a 0x prefix",
                ))
            }
            Self::InvalidTransactionHistoryLimit { .. }
            | Self::InvalidEventLimit { .. }
            | Self::InvalidTokenTransferLimit { .. } => Some(Cow::Borrowed(
                "pass a positive whole-number page size; the selected endpoint enforces its maximum",
            )),
            Self::InvalidTransactionCursor { .. } => Some(Cow::Borrowed(
                "pass the opaque hex cursor returned by the previous `groundhog tx list` page",
            )),
            Self::InvalidEventFilter { max, .. } => Some(Cow::Owned(format!(
                "pass at most {max} values at each event topic position"
            ))),
            Self::InvalidEventCursor { .. } => Some(Cow::Borrowed(
                "pass the opaque hex cursor returned by the previous `groundhog events list` page with unchanged filters",
            )),
            Self::InvalidEventOptions { .. } => Some(Cow::Borrowed(
                "pass `--spec <PATH>` with `--event`, and do not combine a selected event with a conflicting `--topic0`",
            )),
            Self::InvalidTokenTransferCursor { .. } => Some(Cow::Borrowed(
                "pass the opaque hex cursor returned by the previous `groundhog token transfers` page with the same token and participant filters",
            )),
            Self::InvalidTokenTransferOptions { .. } => Some(Cow::Borrowed(
                "use `--account` by itself, or use `--from` and `--to` separately or together",
            )),
            Self::InvalidEventSpec { .. } => Some(Cow::Borrowed(
                "pass a trusted local `.wasm` or `.xdr` artifact containing a valid contract event spec",
            )),
            Self::RpcMethodFailed { .. } => Some(Cow::Borrowed(
                "check the RPC endpoint and node logs for details",
            )),
            Self::RpcCapabilityUnsupported { .. } => Some(Cow::Borrowed(
                "use a full local devnode or a compatible remote RPC that supports the required RPC method",
            )),
            Self::LocalDevAccountUnavailable { .. } => Some(Cow::Borrowed(
                "run `groundhog node` backed by hog-devnode, or use a stored key for non-local endpoints",
            )),
            Self::ManifestEntrypoint { .. } => Some(Cow::Borrowed(
                "restart the local node with a compatible hog-devnode, or remove the stale runtime manifest reference from project or global config",
            )),
            Self::NetworkDescriptorInvalid { .. } | Self::NetworkDescriptorMismatch { .. } => {
                Some(Cow::Borrowed(
                    "check that the selected profile's RPC and faucet URLs refer to the same hosted network, then retry",
                ))
            }
            Self::WalletAddressDerivationFailed { .. } => Some(Cow::Borrowed(
                "check the stored public key material, or regenerate the key",
            )),
            Self::SavedWalletMissing { key_name, profile } => Some(Cow::Owned(format!(
                "run `groundhog wallet create --key={} --profile={}` before using this stored key",
                render_shell_arg(key_name),
                render_shell_arg(profile),
            ))),
            Self::SavedWalletAmbiguous { .. } => Some(Cow::Borrowed(
                "set `default_wallet` in the selected profile or pass the command's explicit wallet option",
            )),
            Self::SavedWalletNotFound { .. } => Some(Cow::Borrowed(
                "run `groundhog wallet ls` and select a saved wallet name",
            )),
            Self::SavedWalletOwnerMismatch { key_name, .. } => Some(Cow::Owned(format!(
                "select a wallet owned by key `{key_name}`, or change the effective default key"
            ))),
            Self::DevWalletSelectionUnsupported { .. } => Some(Cow::Borrowed(
                "remove the explicit/default wallet selection when using a manifest-backed dev key",
            )),
            Self::InheritedDefaultWalletRemoval { profile, .. } => Some(Cow::Owned(format!(
                "run `groundhog wallet use <NAME> --profile={}` to select a project default before removing this wallet",
                render_shell_arg(profile),
            ))),
            Self::WalletRemovalExposesMissingDefault { profile, .. } => Some(Cow::Owned(format!(
                "save the inherited wallet binding or select another project default with `groundhog wallet use <NAME> --profile={}`",
                render_shell_arg(profile),
            ))),
            Self::SavedWalletOwnerPublicKeyInvalid { .. } => Some(Cow::Borrowed(
                "recreate the wallet binding with `groundhog wallet create`, or fix owner_public_key in the selected profile",
            )),
            Self::TransferToSelf { .. } => Some(Cow::Borrowed(
                "select a different recipient or use `--from` to select another source wallet",
            )),
            Self::ReceiptTimeout { .. } => Some(Cow::Borrowed(
                "check the node logs, then rerun the command once the transaction is committed or rejected",
            )),
            Self::TransactionInvalid { .. } => Some(Cow::Borrowed(
                "check the transaction arguments and signing wallet, then submit a new transaction",
            )),
            Self::TransactionRecentFailure { conflict_code, .. }
                if conflict_code == "boundsExceeded" =>
            {
                Some(Cow::Borrowed(
                    "check the signing wallet balance and run `groundhog airdrop` before retrying; the submitted transaction has not committed",
                ))
            }
            Self::TransactionRecentFailure { .. } => Some(Cow::Borrowed(
                "the failure is advisory and the transaction may be retried; check its status before submitting a replacement",
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
                "add the profile to project or global config, or switch to \
                 an existing one with `--profile` or \
                 `GROUNDHOG_PROFILE`",
            )),
            Self::ProfileNotFoundInScope { profile, scope } => Some(Cow::Owned(format!(
                "create the profile with `groundhog config new {} --network <NETWORK> --{scope}`",
                render_shell_arg(profile)
            ))),
            Self::ProfileAlreadyExists { profile, scope } => Some(Cow::Owned(format!(
                "update it with `groundhog config set <FIELD> <VALUE> --profile={} --{scope}`",
                render_shell_arg(profile)
            ))),
            Self::NoActiveProfile => Some(Cow::Borrowed(
                "select a profile with `--profile` or `GROUNDHOG_PROFILE`, or set default_profile in project or global config",
            )),
            Self::ProfileNameInvalid(_) => Some(Cow::Borrowed(
                "use a non-empty profile name without control characters",
            )),
            Self::ConfigFieldUnsupported(_) => Some(Cow::Borrowed(
                "supported fields: network, rpc-url, faucet-url, default-key",
            )),
            Self::ConfigValueInvalid { .. } => Some(Cow::Borrowed(
                "use local, devnet, or mainnet for network; absolute HTTP(S) URLs; and a non-empty default key",
            )),
            Self::ConfigParse { .. } | Self::ConfigFileInvalid { .. } => Some(Cow::Borrowed(
                "fix the reported config file and rerun the command",
            )),
            Self::ProjectConfigUnavailable => Some(Cow::Borrowed(
                "run from inside a Groundhog project or remove `--project`",
            )),
            Self::ConfigScopeConflict => Some(Cow::Borrowed(
                "choose exactly one of `--global` or `--project`",
            )),
            Self::ConfigScopeAmbiguous => Some(Cow::Borrowed(
                "rerun with `--global` or `--project` to choose the destination file",
            )),
            Self::ConfigLockTimeout { .. } => Some(Cow::Borrowed(
                "wait for the other config command to finish, then retry",
            )),
            Self::ConfigChangedDuringConfirmation { .. } => Some(Cow::Borrowed(
                "review the current profile and rerun the removal command",
            )),
            Self::CredentialsEmpty => Some(Cow::Borrowed(
                "rerun `groundhog init` and provide a password; the username \
                 is optional",
            )),
            Self::UnsupportedOs(_) => Some(Cow::Borrowed(
                "`groundhog test` currently supports macOS and \
                 Linux host targets",
            )),
            Self::WasmOptFailed(_) => Some(Cow::Borrowed(
                "see the wasm-opt error above; the WASM binary \
                 may be invalid or malformed",
            )),
            Self::Rv32LowerFailed(error) => Some(Cow::Borrowed(match error {
                groundhog_rv32_lower::LowerError::CodeTooLarge { .. } => {
                    "the contract's code section exceeds the current artifact \
                     format's code-size cap; reduce code size — common causes \
                     are formatted panics pulling in `core::fmt` and large \
                     generic instantiations"
                }
                groundhog_rv32_lower::LowerError::DataTooLarge { .. } => {
                    "static data plus the reserved stack exceed the VM's data \
                     memory; reduce static data, or pass a smaller \
                     `groundhog build --stack-size` to shrink the stack \
                     reserve below the data sections"
                }
                groundhog_rv32_lower::LowerError::Verify(_) => {
                    "this is a bug — please report this lower→verify \
                     failure to the Groundhog team"
                }
                _ => {
                    "the RV32 ELF may have a malformed `.text`/data layout or a \
                     nonzero entry point; see the lower error above"
                }
            })),
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
                 with `groundhog keys rm -- {}`",
                render_shell_arg(name),
            ))),
            Self::KeyNotFound(_) => Some(Cow::Borrowed(
                "list available keys with `groundhog keys ls`",
            )),
            Self::KeyNameEmpty | Self::KeyNameInvalid => Some(Cow::Borrowed(
                "key names may only contain letters, digits, \
                 hyphens, and underscores",
            )),
            Self::EmptyWalletName => Some(Cow::Borrowed("pass a non-empty wallet name")),
            Self::JsonUnsupported(command) => Some(Cow::Owned(format!(
                "run `groundhog {command}` without `--json`"
            ))),
            Self::NotImplemented => Some(Cow::Borrowed(
                "this command is planned for a future release",
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
            Self::EnvRustflagsSet(var) => Some(Cow::Owned(format!(
                "unset it for this build (e.g. `env -u {var} groundhog build`); to add extra \
                 rustflags, append them to the `[target.*]` rustflags lists in the project's \
                 `.cargo/config.toml` instead"
            ))),
        }
    }

    /// Optional structured details for JSON error payloads.
    #[allow(clippy::too_many_lines)]
    pub fn details_json(&self) -> Option<Value> {
        match self {
            Self::CargoFailedWithOutput { stdout, stderr, .. } => Some(json!({
                "stdout": stdout,
                "stderr": stderr,
            })),
            Self::Rv32LowerFailed(error) => Some(json!({ "message": error.to_string() })),
            Self::CallEncode { path, message } | Self::CallDecode { path, message } => {
                Some(json!({
                    "path": path,
                    "message": message,
                }))
            }
            Self::ConfigParse {
                path,
                message,
                line,
                column,
            } => Some(json!({
                "path": path,
                "message": message,
                "line": line,
                "column": column,
            })),
            Self::ConfigFileInvalid { path, message } => Some(json!({
                "path": path,
                "message": message,
            })),
            Self::ConfigValueInvalid { field, message } => Some(json!({
                "field": field,
                "message": message,
            })),
            Self::ProfileNotFoundInScope { profile, scope }
            | Self::ProfileAlreadyExists { profile, scope } => Some(json!({
                "profile": profile,
                "scope": scope,
            })),
            Self::ConfigLockTimeout { path } | Self::ConfigChangedDuringConfirmation { path } => {
                Some(json!({
                    "path": path,
                }))
            }
            Self::RpcCapabilityUnsupported { method, message } => Some(json!({
                "method": method,
                "message": message,
            })),
            Self::ManifestEntrypoint {
                key_name,
                field,
                message,
            } => Some(json!({
                "keyName": key_name,
                "field": field,
                "message": message,
            })),
            Self::NetworkDescriptorInvalid { field, message } => Some(json!({
                "field": field,
                "message": message,
            })),
            Self::NetworkDescriptorMismatch {
                field,
                expected,
                actual,
            } => Some(json!({
                "field": field,
                "expected": expected,
                "actual": actual,
            })),
            Self::SavedWalletMissing { key_name, profile } => Some(json!({
                "keyName": key_name,
                "profile": profile,
            })),
            Self::SavedWalletAmbiguous {
                key_name,
                profile,
                wallet_names,
            } => Some(json!({
                "keyName": key_name,
                "profile": profile,
                "walletNames": wallet_names,
            })),
            Self::SavedWalletNotFound {
                wallet_name,
                profile,
            }
            | Self::InheritedDefaultWalletRemoval {
                wallet_name,
                profile,
            } => Some(json!({
                "walletName": wallet_name,
                "profile": profile,
            })),
            Self::WalletRemovalExposesMissingDefault {
                wallet_name,
                inherited_wallet,
                profile,
            } => Some(json!({
                "walletName": wallet_name,
                "inheritedWallet": inherited_wallet,
                "profile": profile,
            })),
            Self::SavedWalletOwnerMismatch {
                wallet_name,
                key_name,
                configured,
                local,
            } => Some(json!({
                "walletName": wallet_name,
                "keyName": key_name,
                "configured": configured,
                "local": local,
            })),
            Self::DevWalletSelectionUnsupported {
                key_name,
                wallet_name,
            } => Some(json!({
                "keyName": key_name,
                "walletName": wallet_name,
            })),
            Self::SavedWalletOwnerPublicKeyInvalid { key_name, message } => Some(json!({
                "keyName": key_name,
                "message": message,
            })),
            Self::TransferToSelf { address } => Some(json!({
                "address": address,
            })),
            Self::InvalidHogAmount { input, message }
            | Self::InvalidTransactionHash { input, message, .. }
            | Self::InvalidTransactionHistoryLimit { input, message, .. }
            | Self::InvalidTransactionCursor { input, message, .. }
            | Self::InvalidEventTopic { input, message, .. }
            | Self::InvalidEventLimit { input, message, .. }
            | Self::InvalidEventCursor { input, message, .. }
            | Self::InvalidTokenTransferLimit { input, message, .. }
            | Self::InvalidTokenTransferCursor { input, message, .. } => Some(json!({
                "input": input,
                "message": message,
            })),
            Self::InvalidEventFilter {
                position,
                count,
                max,
            } => Some(json!({
                "position": position,
                "count": count,
                "max": max,
            })),
            Self::InvalidEventOptions { message }
            | Self::InvalidTokenTransferOptions { message } => Some(json!({
                "message": message,
            })),
            Self::InvalidEventSpec { path, message, .. } => Some(json!({
                "path": path,
                "message": message,
            })),
            Self::InvalidAddressTarget { target, message } => Some(json!({
                "target": target,
                "message": message,
            })),
            Self::UnknownAddressTarget { target } => Some(json!({
                "target": target,
            })),
            Self::TransactionInvalid { tx_hash, reason } => Some(json!({
                "txHash": tx_hash,
                "reason": reason,
            })),
            Self::TransactionRecentFailure {
                tx_hash,
                attempt_height,
                conflict_address,
                conflict_key,
                conflict_code,
            } => Some(json!({
                "txHash": tx_hash,
                "attemptHeight": attempt_height,
                "conflict": {
                    "address": conflict_address,
                    "key": conflict_key,
                    "code": conflict_code,
                },
            })),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_rv32_lower::LowerError;

    fn hint(error: &CliError) -> String {
        error.hint().expect("hint expected").into_owned()
    }

    #[test]
    fn rv32_lower_code_too_large_hints_at_code_size_reduction() {
        let error = CliError::Rv32LowerFailed(LowerError::CodeTooLarge {
            size: 82472,
            max: 65536,
        });
        assert!(hint(&error).contains("reduce code size"));
    }

    #[test]
    fn rv32_lower_data_too_large_hints_at_stack_size() {
        let error = CliError::Rv32LowerFailed(LowerError::DataTooLarge {
            section: ".bss".to_string(),
            end: 70168,
            max: 65536,
        });
        assert!(hint(&error).contains("--stack-size"));
    }

    #[test]
    fn rv32_lower_invalid_keeps_the_malformed_elf_hint() {
        let error = CliError::Rv32LowerFailed(LowerError::Invalid(
            ".text must start at vaddr 0, got 4".to_string(),
        ));
        assert!(hint(&error).contains("malformed"));
    }

    #[test]
    fn rv32_lower_verify_hints_at_reporting_a_bug() {
        let error =
            CliError::Rv32LowerFailed(LowerError::Verify("code digest mismatch".to_string()));
        assert!(hint(&error).contains("bug"));
    }

    #[test]
    fn rv32_lower_failed_details_json_keeps_the_message_field() {
        let inner = LowerError::CodeTooLarge {
            size: 65540,
            max: 65536,
        };
        let message = inner.to_string();
        let details = CliError::Rv32LowerFailed(inner)
            .details_json()
            .expect("details expected");
        assert_eq!(details["message"], message);
    }

    #[test]
    fn generated_command_hints_quote_dynamic_arguments() {
        let error = CliError::SavedWalletMissing {
            key_name: "-alice;echo bad\n".into(),
            profile: "-dev&net\u{202e}".into(),
        };
        let rendered = error.to_string();
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{202e}'));
        assert_eq!(
            hint(&error),
            r"run `groundhog wallet create --key='-alice;echo bad\n' --profile='-dev&net\u{202e}'` before using this stored key"
        );

        let error = CliError::NodeSupervisorProtocolIntroRejected {
            message: "rejected".into(),
            expected_protocol: Some("hogp/test;echo bad".into()),
        };
        assert_eq!(
            hint(&error),
            "pass `--hogd-protocol='hogp/test;echo bad'` to override, or rebuild groundhog node against a matching groundhog-xdr"
        );
    }
}
