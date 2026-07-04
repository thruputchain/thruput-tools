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
    Rv32LowerFailed(groundhog_rv32_lower::LowerError),
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
    #[error("runtime manifest entrypoint `{field}` for key `{key_name}` is invalid: {message}")]
    ManifestEntrypoint {
        /// Dev key name whose configured runtime manifest was read.
        key_name: String,
        /// Runtime manifest field path containing the malformed entrypoint.
        field: &'static str,
        /// Specific parse or validation failure.
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
            Self::ArtifactNotFound { .. } => "artifact_not_found",
            Self::UnsupportedArtifactExtension(_) => "unsupported_artifact_extension",
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
            Self::EmptyWalletName => "empty_wallet_name",
            Self::KeyNameInvalid => "key_name_invalid",
            Self::JsonUnsupported(_) => "json_unsupported",
            Self::NotImplemented => "not_implemented",
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
            Self::ManifestEntrypoint { .. } => "manifest_entrypoint_invalid",
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
            Self::CargoFailed(_) => Some(Cow::Borrowed(
                "see the cargo output above for the underlying build \
                 or test failure",
            )),
            Self::CargoFailedWithOutput { stderr, .. } => {
                // Older SDK linker scripts enforce the RV32 size caps at link
                // time; recognize rust-lld's region-overflow errors and explain
                // them instead of pointing at raw linker output.
                let code_overflow = stderr.contains("will not fit in region 'CODE'");
                let data_overflow = stderr.contains("will not fit in region 'DATA'");
                Some(Cow::Borrowed(match (code_overflow, data_overflow) {
                    (true, true) => {
                        "the contract overflows both the SDK linker script's \
                         CODE region (reduce code size — common causes are \
                         256-bit integer math, formatted panics pulling in \
                         `core::fmt`, and large generic instantiations) and \
                         its DATA region (reduce static data, or pass a \
                         smaller `groundhog build --stack-size`). Any \
                         `groundhogspecrawv0` linker error above is a \
                         knock-on of the `.text` overflow, not a separate \
                         problem"
                    }
                    (true, false) => {
                        "the contract's code does not fit the SDK linker \
                         script's CODE region; reduce code size — common \
                         causes are 256-bit integer math, formatted panics \
                         pulling in `core::fmt`, and large generic \
                         instantiations. Any `groundhogspecrawv0` linker error \
                         above is a knock-on of the `.text` overflow, not a \
                         separate problem"
                    }
                    (false, true) => {
                        "static data plus the reserved stack exceed the VM's \
                         data memory; reduce static data, or pass a smaller \
                         `groundhog build --stack-size` to shrink the stack \
                         reserve below the data sections"
                    }
                    (false, false) => {
                        "see the cargo output above for the underlying build \
                         or test failure"
                    }
                }))
            }
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
                "run `groundhog node` backed by hog-devnode, or use a stored key for non-local endpoints",
            )),
            Self::ManifestEntrypoint { .. } => Some(Cow::Borrowed(
                "restart the local node with a compatible hog-devnode, or remove the stale runtime manifest reference from groundhog.toml",
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
            Self::Rv32LowerFailed(error) => Some(Cow::Borrowed(match error {
                groundhog_rv32_lower::LowerError::CodeTooLarge { .. } => {
                    "the contract's code section exceeds the current artifact \
                     format's code-size cap; reduce code size — common causes \
                     are 256-bit integer math, formatted panics pulling in \
                     `core::fmt`, and large generic instantiations"
                }
                groundhog_rv32_lower::LowerError::DataTooLarge { .. } => {
                    "static data plus the reserved stack exceed the VM's data \
                     memory; reduce static data, or pass a smaller \
                     `groundhog build --stack-size` to shrink the stack \
                     reserve below the data sections"
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
                 with `groundhog keys rm {name}`"
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
        }
    }

    /// Optional structured details for JSON error payloads.
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
            Self::ManifestEntrypoint {
                key_name,
                field,
                message,
            } => Some(json!({
                "keyName": key_name,
                "field": field,
                "message": message,
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
    fn rv32_lower_failed_details_json_keeps_the_message_field() {
        let error = CliError::Rv32LowerFailed(LowerError::CodeTooLarge {
            size: 65540,
            max: 65536,
        });
        let details = error.details_json().expect("details expected");
        assert_eq!(
            details["message"],
            ".text is 65540 bytes; the current artifact format caps code at \
             65536 bytes (4 bytes over)"
        );
    }

    fn cargo_failed_with_stderr(stderr: &str) -> CliError {
        CliError::CargoFailedWithOutput {
            code: 1,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn cargo_code_region_overflow_hints_at_code_size_and_spec_knock_on() {
        let error = cargo_failed_with_stderr(
            "rust-lld: error: section '.text' will not fit in region 'CODE': \
             overflowed by 16936 bytes",
        );
        let hint = hint(&error);
        assert!(hint.contains("reduce code size"));
        assert!(hint.contains("groundhogspecrawv0"));
    }

    #[test]
    fn cargo_data_region_overflow_hints_at_stack_size() {
        let error = cargo_failed_with_stderr(
            "rust-lld: error: section '.bss' will not fit in region 'DATA': \
             overflowed by 4632 bytes",
        );
        assert!(hint(&error).contains("--stack-size"));
    }

    #[test]
    fn cargo_dual_region_overflow_hints_at_both_fixes() {
        let error = cargo_failed_with_stderr(
            "rust-lld: error: section '.text' will not fit in region 'CODE': \
             overflowed by 16936 bytes\n\
             rust-lld: error: section '.bss' will not fit in region 'DATA': \
             overflowed by 4632 bytes",
        );
        let hint = hint(&error);
        assert!(hint.contains("reduce code size"));
        assert!(hint.contains("--stack-size"));
    }

    #[test]
    fn other_cargo_failures_keep_the_generic_hint() {
        for error in [
            cargo_failed_with_stderr("error[E0308]: mismatched types"),
            CliError::CargoFailed(101),
        ] {
            assert!(hint(&error).contains("see the cargo output above"));
        }
    }
}
