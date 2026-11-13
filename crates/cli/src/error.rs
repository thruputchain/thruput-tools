use serde_json::json;
use std::borrow::Cow;
use thiserror::Error;
use thruput_cli_macros::ErrorMeta;

use crate::terminal::render_shell_arg;

/// Next step after any cargo invocation that failed without a recognizable
/// cause.
const CARGO_FAILED_HINT: &str = "see the cargo output above for the underlying build \
                                 or test failure";

/// Next step for the config files the CLI reads, whichever way one is broken.
const CONFIG_FILE_HINT: &str = "fix the reported config file and rerun the command";

/// Accepted spellings of a key name.
const KEY_NAME_HINT: &str = "key names may only contain letters, digits, \
                             hyphens, and underscores";

/// Accepted spellings of an account or contract the command acts on.
const ADDRESS_TARGET_HINT: &str =
    "use a saved wallet, contract alias, stored key, base58 address, or Z StrKey";

/// Accepted spelling of any 32-byte value passed on the command line.
const HASH_32_BYTES_HINT: &str =
    "pass exactly 32 bytes as case-insensitive hex, with or without a 0x prefix";

/// Accepted spelling of a page size, for every paginated command.
const PAGE_LIMIT_HINT: &str =
    "pass a positive whole-number page size; the selected endpoint enforces its maximum";

/// Next step for a toolchain bundle that could not be installed or fetched.
const TOOLCHAIN_RETRY_HINT: &str =
    "check the artifact source and cache permissions, then retry `thruput toolchain install`";

/// All errors returned by CLI commands.
///
/// Every variant carries its own metadata beside its message, and
/// [`ErrorMeta`] derives [`CliError::code`], [`CliError::hint`], and
/// [`CliError::details_json`] from it:
///
/// - the code defaults to the variant name in `snake_case`; `#[code("...")]`
///   overrides it for the variants whose stable code differs,
/// - `#[hint(...)]` is the user-facing next step, formatted over the
///   variant's fields the way `#[error(...)]` formats the message. A variant
///   with no `#[hint]` has no hint, either because its message already states
///   the fix or because nothing actionable can be said.
/// - `#[details(...)]` names the fields the `--json` payload carries, under
///   their `camelCase` keys. A variant with no `#[details]` carries none.
#[derive(Error, Debug, ErrorMeta)]
pub enum CliError {
    #[error("deployment funding unavailable: {message}")]
    #[hint(
        "use an RPC endpoint with authoritative chainConfig execution rules; explicit --fund <AMOUNT> remains available"
    )]
    #[details]
    DeploymentFunding { message: String },
    #[error("invalid wallet `{wallet_name}`: {message}")]
    #[details]
    InvalidWalletBinding {
        wallet_name: String,
        message: String,
    },
    #[error("wallet `{wallet_name}` already names a different account")]
    #[hint("choose a different --name; existing wallet identities are preserved")]
    #[details]
    WalletBindingConflict { wallet_name: String },
    #[error("wallet create no longer accepts --prefund")]
    #[hint("create the empty wallet first, then run thruput airdrop <wallet-name>")]
    WalletCreatePrefundUnsupported,
    #[error("native deployment requires an explicit --fund <AMOUNT> amount")]
    #[hint(
        "choose the native token amount to transfer to the new contract; this is not an automatic storage-cost estimate"
    )]
    NativeDeploymentFundingRequired,
    #[error("--fund and --init require a native wallet")]
    #[hint("existing legacy wallets retain their own deployment funding behavior")]
    NativeDeploymentOptionsForLegacy,
    #[error("invalid deployment initializer: {message}")]
    #[details]
    InvalidDeploymentInitializer { message: String },
    #[error("native deployment response could not be verified: {message}")]
    #[details]
    NativeDeploymentUnverified { message: String },
    #[error(transparent)]
    #[code("docs_error")]
    Docs(#[from] crate::docs::DocsError),
    #[error(transparent)]
    #[code("skill_error")]
    Skill(#[from] crate::skill::SkillError),
    #[error("cargo not found on PATH")]
    #[hint("install Rust and ensure `cargo` is on your PATH")]
    CargoNotFound,
    #[error("`.cargo/contract.toml` not found in {0}")]
    #[hint(
        "every contract build runs as \
         `cargo --config .cargo/contract.toml`; create that file \
         containing `[unstable]` and \
         `build-std = [\"core\", \"alloc\"]`, or run `thruput new` \
         to scaffold a project. Do not put `[target.*]` rustflags in \
         it — cargo joins those with `.cargo/config.toml`, so a flag \
         in both files is passed twice"
    )]
    ContractTomlNotFound(String),
    #[error("package `{0}` not found in workspace")]
    #[hint(
        "available packages can be listed with \
         `cargo metadata --no-deps`; \
         check that `{0}` is spelled correctly"
    )]
    PackageNotFound(String),
    #[error("could not determine package — specify one with `-p <name>`")]
    #[hint(
        "run from inside a package directory, pass `-p <package>` to select one, \
         or pass `--workspace` to `thruput test` to test every member with a \
         `test_rv32` target"
    )]
    PackageRequired,
    #[error("cargo metadata failed: {0}")]
    #[hint(
        "ensure `cargo` is on PATH and you are inside \
         a Cargo project or workspace"
    )]
    CargoMetadataFailed(String),
    #[error("expected RV32 ELF not found at {0}")]
    #[hint(
        "check that the package builds a binary for \
         riscv32im-unknown-none-elf and that its binary name matches \
         the package name"
    )]
    Rv32ElfNotFound(String),
    #[error("expected {kind} artifact not found at {path}")]
    #[hint("run `thruput build` first to produce the {kind} artifact, or pass `--artifact <path>`")]
    ArtifactNotFound { kind: &'static str, path: String },
    #[error("unsupported artifact extension for {0}; expected .rv32a")]
    #[hint("pass a `.rv32a` artifact path")]
    UnsupportedArtifactExtension(String),
    #[error("target dir conflicts with CLI passthrough args or config")]
    #[hint(
        "remove `--target-dir` from passthrough args or \
         config when using the CLI target directory option"
    )]
    TargetDirConflict,
    #[error("cargo failed with exit code {0}")]
    #[hint(text = CARGO_FAILED_HINT)]
    CargoFailed(i32),
    #[error("cargo failed with exit code {code}")]
    #[code("cargo_failed")]
    #[hint(expr = Some(cargo_failure_hint(stderr)))]
    #[details(stdout, stderr)]
    CargoFailedWithOutput {
        code: i32,
        stdout: String,
        stderr: String,
    },
    #[error("directory already exists: {0}")]
    #[hint("remove `{0}`, or choose a different project name")]
    DirExists(String),
    #[error("directory is not empty")]
    #[hint("use an empty directory, or pick a different target path")]
    DirNotEmpty,
    #[error("cannot prepare project directory `{}`: {source}", .path.display())]
    #[hint(
        "choose a destination with an existing parent directory and check its permissions; relative paths, absolute paths, and ~/ paths are supported"
    )]
    #[details(path)]
    ProjectDirectory {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("unknown template: {0}")]
    #[hint(
        "run `thruput templates list` for available templates; omit --template for a minimal contract"
    )]
    UnknownTemplate(String),
    #[error(transparent)]
    #[code("io_error")]
    Io(#[from] std::io::Error),
    #[error("profile `{0}` not found in project or global config")]
    #[hint(
        "create it with `thruput config new {0} --network <NETWORK>`, or switch to \
         an existing one with `--profile` or `THRUPUT_PROFILE`"
    )]
    ProfileNotFound(String),
    #[error("profile `{profile}` not found in {scope} config")]
    #[hint(
        "create the profile with `thruput config new {} --network <NETWORK> --{scope}`",
        render_shell_arg(profile)
    )]
    #[details]
    ProfileNotFoundInScope {
        profile: String,
        scope: &'static str,
    },
    #[error("profile `{profile}` already exists in {scope} config")]
    #[hint(
        "update it with `thruput config set <FIELD> <VALUE> --profile={} --{scope}`",
        render_shell_arg(profile)
    )]
    #[details]
    ProfileAlreadyExists {
        profile: String,
        scope: &'static str,
    },
    #[error("no active profile selected")]
    #[hint(
        "select a profile with `--profile` or `THRUPUT_PROFILE`, or set default_profile in \
         project or global config"
    )]
    NoActiveProfile,
    #[error("invalid profile name: {0}")]
    #[hint("use a non-empty profile name without control characters")]
    ProfileNameInvalid(&'static str),
    #[error("unsupported config field `{0}`")]
    #[hint("supported fields: network, rpc-url, network-hash, default-key")]
    ConfigFieldUnsupported(String),
    #[error("invalid value for config field `{field}`: {message}")]
    #[hint(
        "use local, futurenet, devnet (compatibility alias), or mainnet for network; absolute HTTP(S) URLs; \
         base58 encoding of exactly 32 bytes for network-hash; and a non-empty default key"
    )]
    #[details]
    ConfigValueInvalid { field: String, message: String },
    #[error("failed to parse config `{}`: {message}", .path.display())]
    #[code("config_parse_failed")]
    #[hint(text = CONFIG_FILE_HINT)]
    #[details]
    ConfigParse {
        path: std::path::PathBuf,
        message: String,
        line: Option<usize>,
        column: Option<usize>,
    },
    #[error("invalid config `{}`: {message}", .path.display())]
    #[hint(text = CONFIG_FILE_HINT)]
    #[details]
    ConfigFileInvalid {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("project config not found")]
    #[hint("run from inside a Thruput project or remove `--project`")]
    ProjectConfigUnavailable,
    #[error("`--global` and `--project` cannot be used together")]
    #[hint("choose exactly one of `--global` or `--project`")]
    ConfigScopeConflict,
    #[error("config scope is ambiguous because a project config exists")]
    #[hint("rerun with `--global` or `--project` to choose the destination file")]
    ConfigScopeAmbiguous,
    #[error("config `{}` is busy; another process may be updating it", .path.display())]
    #[hint("wait for the other config command to finish, then retry")]
    #[details]
    ConfigLockTimeout { path: std::path::PathBuf },
    #[error("config `{}` changed while awaiting confirmation", .path.display())]
    #[hint("review the current profile and rerun the removal command")]
    #[details]
    ConfigChangedDuringConfirmation { path: std::path::PathBuf },
    #[error("unsupported OS: {0}")]
    #[hint(
        "`thruput test` currently supports macOS and \
         Linux host targets"
    )]
    UnsupportedOs(String),
    #[error("RV32 lowering failed: {0}")]
    #[hint(expr = Some(rv32_lower_hint(_0)))]
    #[details(expr = Some(json!({ "message": _0.to_string() })))]
    Rv32LowerFailed(#[from] thruput_rv32_lower::LowerError),
    #[error("contract spec finalization failed: {0}")]
    #[hint(
        "check that your contract uses `#[contract]` and \
         `#[contract_type]` attributes correctly"
    )]
    SpecFailed(String),
    /// TypeScript codegen cannot express the spec. The contract itself builds;
    /// the message names the declarations involved and how to rename them.
    #[error("cannot generate TypeScript bindings: {0}")]
    TsBindingsFailed(String),
    /// The contract's raw spec wire format is one this build cannot read.
    ///
    /// Surfaced verbatim rather than reworded: the inner error names both
    /// versions and the way out for that direction, and the SDK reports the
    /// same text, so the two toolchains agree on the diagnosis.
    #[error(transparent)]
    RawSpecFormat(thruput_spec_tool::SpecToolError),
    #[error("key `{0}` already exists")]
    #[hint(
        "use a different name, or remove the existing key \
         with `thruput keys rm -- {}`",
        render_shell_arg(_0)
    )]
    KeyExists(String),
    #[error("key `{0}` not found")]
    #[hint("list available keys with `thruput keys ls`")]
    KeyNotFound(String),
    /// The stored key's scheme cannot be used for the requested operation.
    #[error("key `{key_name}` uses scheme `{scheme}`, which does not support {operation}")]
    #[hint("`thruput keys ls` shows each key's scheme; select a key whose scheme supports this")]
    #[details(key_name, scheme, operation)]
    KeySchemeUnsupported {
        key_name: String,
        scheme: String,
        operation: String,
    },
    #[error("key name cannot be empty")]
    #[hint(text = KEY_NAME_HINT)]
    KeyNameEmpty,
    #[error("wallet name cannot be empty")]
    #[hint("pass a non-empty wallet name")]
    EmptyWalletName,
    #[error(
        "key name must contain only letters, digits, \
         hyphens, and underscores"
    )]
    #[hint(text = KEY_NAME_HINT)]
    KeyNameInvalid,
    #[error("expected an S StrKey or base58 encoding of a 32-byte Ed25519 seed on stdin")]
    #[hint(
        "provide one private seed through stdin or the hidden prompt; never pass it as a command argument"
    )]
    InvalidSecretSeed,
    #[error("integration test target not found at {0}")]
    #[hint("create `tests/integration.rs` before running `thruput test --integration`")]
    IntegrationTestNotFound(String),
    #[error("invalid test options: {0}")]
    #[hint("remove the conflicting `thruput test` option and rerun the command")]
    InvalidTestOptions(String),
    #[error("no workspace package declares a `test_rv32` test target under {0}")]
    #[hint(
        "`--workspace` tests the members that declare a `tests/test_rv32.rs` target, \
         as `thruput new` scaffolds; run `thruput test -p <package>` to test one \
         member"
    )]
    NoWorkspaceTestPackages(String),
    #[error(
        "tests failed in {} of {total} workspace package(s): {}",
        .failed.len(),
        .failed.join(", ")
    )]
    #[hint(
        "see each package's test output above; rerun one package with \
         `thruput test -p <package>`, or `thruput test --workspace --rerun` to \
         run only the recorded failures"
    )]
    #[details]
    WorkspaceTestsFailed { failed: Vec<String>, total: usize },
    #[error(
        "`{0}` is set in the environment, so cargo would ignore the project's \
         `.cargo/config.toml` target rustflags (Thruput linker and codegen flags) and \
         produce a broken artifact"
    )]
    #[hint(
        "unset it for this build (e.g. `env -u {0} thruput build`); to add extra \
         rustflags, append them to the `[target.*]` rustflags lists in the project's \
         `.cargo/config.toml` instead"
    )]
    EnvRustflagsSet(&'static str),
    #[error(
        "no `-T` linker script in the effective rustflags for \
         `[target.riscv32im-unknown-none-elf]`; the rv32 link would use the default memory \
         layout and produce an artifact that fails validation"
    )]
    #[hint(expr = Some(linker_script_hint(workspace_root, member_config.as_deref())))]
    Rv32LinkerScriptNotConfigured {
        workspace_root: String,
        member_config: Option<String>,
    },
    #[error("status check found warnings")]
    StatusHasWarnings,
    #[error("command emitted structured failure output")]
    CommandReportedFailure,
    #[error("node supervisor error: {0}")]
    #[code("node_supervisor_error")]
    #[hint(
        "check `--hog-devnode-binary`; if contract artifacts are installed elsewhere, \
         pass `--contract-artifacts-dir <DIR>`, then inspect the hog-devnode process \
         output and local node logs"
    )]
    NodeSupervisor(String),
    #[error("node supervisor error: {message}")]
    #[code("protocol_intro_rejected")]
    #[hint(expr = Some(protocol_intro_hint(expected_protocol.as_deref())))]
    NodeSupervisorProtocolIntroRejected {
        message: String,
        expected_protocol: Option<String>,
    },
    #[error("local node already running: {0}")]
    #[hint("stop the running `thruput node` process before starting another local node")]
    LocalNodeAlreadyRunning(String),
    #[error("could not connect to {0}")]
    #[hint(
        "run `thruput node` in another terminal, or check that \
         the RPC endpoint at {0} is running"
    )]
    RpcConnect(String),
    #[error("invalid proxy configuration in {variable}: {message}")]
    #[code("proxy_configuration_invalid")]
    #[hint("set `{variable}` to an HTTP(S) proxy URL or host:port, or unset it")]
    ProxyConfiguration {
        variable: &'static str,
        message: &'static str,
    },
    #[error("failed to initialize outbound HTTP client")]
    #[code("http_client_init_failed")]
    #[hint("check the standard proxy environment variables, then retry")]
    HttpClientInit,
    #[error("no rpc_url configured for the selected profile")]
    #[hint(
        "for a local profile, start `thruput node` in another terminal; otherwise run \
         `thruput config set rpc-url <URL> --profile <name>` or pass `--rpc-url <URL>`"
    )]
    MissingRpcUrl,
    #[error(
        "selected profile `{profile}` uses the local network, but no local node endpoint is available"
    )]
    #[code("missing_rpc_url")]
    #[hint(
        "start `thruput node` in another terminal, then retry; if you intended another network, pass `--profile <name>` (for example `--profile futurenet`) or `--rpc-url <URL>`"
    )]
    #[details(expr = Some(json!({ "profile": profile, "network": "local" })))]
    LocalNodeUnavailable { profile: String },
    #[error("profile `{profile}` {message}")]
    #[hint(
        "update the selected profile in project or global config, or choose a profile that \
         matches the requested command"
    )]
    ProfileConfigInvalid { profile: String, message: String },
    #[error("interactive input is unavailable")]
    #[hint("provide the required input explicitly; for key import, pipe the seed on stdin")]
    InteractiveInputRequired,
    #[error("cannot confirm the request to {action}: no interactive terminal is available")]
    #[hint("rerun from an interactive terminal, or pass `--yes` to proceed without a prompt")]
    #[details(expr = Some(json!({ "action": action, "requiredFlag": "--yes" })))]
    ConfirmationRequired { action: &'static str },
    #[error("no signing key configured")]
    #[hint("set `default_key` in project or global config, or run `thruput keys generate`")]
    MissingSigningKey,
    #[error("cannot determine the network hash for this profile")]
    #[hint(
        "signing binds each transaction to a network, so every profile pins its \
         hash: run `thruput init` to pin the built-in Futurenet profile, run \
         `thruput config set network-hash <hash> --profile <name>` for another \
         network, or point the profile at a runtime manifest from `thruput node` \
         via THRUPUT_RUNTIME_MANIFEST or profile.<name>.runtime_manifest"
    )]
    MissingNetworkHash,
    #[error("profile network_hash {value:?} is not base58 encoding of exactly 32 bytes")]
    #[hint(
        "network_hash is the 32-byte network hash in unprefixed base58, \
         as `getNetworkInfo` reports it"
    )]
    InvalidNetworkHash { value: String },
    #[error("invalid native token amount `{input}`: {message}")]
    #[hint("enter a positive decimal native token amount with at most seven decimal places")]
    #[details]
    InvalidNativeTokenAmount { input: String, message: String },
    #[error("address target {target:?} is invalid: {message}")]
    #[hint(text = ADDRESS_TARGET_HINT)]
    #[details]
    InvalidAddressTarget { target: String, message: String },
    #[error("unknown address target {target:?}")]
    #[hint(text = ADDRESS_TARGET_HINT)]
    #[details]
    UnknownAddressTarget { target: String },
    #[error("unknown contract alias `{alias}` in profile `{profile}`")]
    #[hint(
        "use a configured contract alias, base58 address, or Z StrKey; inspect aliases \
         with `thruput config get {} --sources`",
        render_shell_arg(profile)
    )]
    #[details]
    UnknownContractAlias {
        alias: String,
        profile: String,
        config_source: Option<std::path::PathBuf>,
        available_aliases: Vec<String>,
    },
    #[error("invalid transaction hash `{display_input}`: {message}")]
    #[hint(text = HASH_32_BYTES_HINT)]
    #[details(input, message)]
    InvalidTransactionHash {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid transaction history limit `{display_input}`: {message}")]
    #[hint(text = PAGE_LIMIT_HINT)]
    #[details(input, message)]
    InvalidTransactionHistoryLimit {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid transaction cursor `{display_input}`: {message}")]
    #[hint("pass the opaque hex cursor returned by the previous `thruput tx list` page")]
    #[details(input, message)]
    InvalidTransactionCursor {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event {position} value `{display_input}`: {message}")]
    #[hint(text = HASH_32_BYTES_HINT)]
    #[details(input, message)]
    InvalidEventTopic {
        position: &'static str,
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event filter: {position} has {count} values; maximum is {max}")]
    #[hint("pass at most {max} values at each event topic position")]
    #[details]
    InvalidEventFilter {
        position: &'static str,
        count: usize,
        max: u32,
    },
    #[error("invalid event limit `{display_input}`: {message}")]
    #[hint(text = PAGE_LIMIT_HINT)]
    #[details(input, message)]
    InvalidEventLimit {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event cursor `{display_input}`: {message}")]
    #[hint(
        "pass the opaque hex cursor returned by the previous `thruput events list` page \
         with unchanged filters"
    )]
    #[details(input, message)]
    InvalidEventCursor {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid event options: {message}")]
    #[hint(
        "pass `--spec <PATH>` with `--event`, and do not combine a selected event with a \
         conflicting `--topic0`"
    )]
    #[details]
    InvalidEventOptions { message: String },
    #[error("invalid token transfer limit `{display_input}`: {message}")]
    #[code("invalid_event_limit")]
    #[hint(text = PAGE_LIMIT_HINT)]
    #[details(input, message)]
    InvalidTokenTransferLimit {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid token transfer cursor `{display_input}`: {message}")]
    #[code("invalid_event_cursor")]
    #[hint(
        "pass the opaque hex cursor returned by the previous `thruput token transfers` \
         page with the same token and participant filters"
    )]
    #[details(input, message)]
    InvalidTokenTransferCursor {
        input: String,
        display_input: String,
        message: String,
    },
    #[error("invalid token transfer options: {message}")]
    #[code("invalid_event_options")]
    #[hint("use `--account` by itself, or use `--from` and `--to` separately or together")]
    #[details]
    InvalidTokenTransferOptions { message: String },
    #[error("invalid event spec `{display_path}`: {message}")]
    #[hint("pass a trusted local `.xdr` sidecar containing a valid contract event spec")]
    #[details(path, message)]
    InvalidEventSpec {
        path: String,
        display_path: String,
        message: String,
    },
    #[error("failed to initialize async runtime: {0}")]
    #[code("runtime_init_failed")]
    RuntimeInit(String),
    #[error("{method} failed: {message}")]
    #[hint("check the RPC endpoint and node logs for details")]
    RpcMethodFailed {
        method: &'static str,
        message: String,
    },
    #[error("native RPC: {message}")]
    #[hint("check the endpoint and retained evidence before choosing a recovery action")]
    #[details(expr = Some(evidence.clone()))]
    NativeRpcFailure {
        message: String,
        evidence: serde_json::Value,
    },
    #[error("local funding attempt: {0}")]
    #[hint(
        "retry the original request to reconcile its outcome; do not delete an uncertain attempt"
    )]
    LocalFundingAttempt(String),
    #[error("local funding transaction {tx_hash} is unresolved: {message}")]
    #[hint(
        "inspect this hash with thruput tx show, then retry the original recipient and amount; keep the retained attempt until an authoritative receipt resolves it"
    )]
    #[details(tx_hash, attempt_path)]
    LocalFundingUnresolved {
        tx_hash: String,
        attempt_path: String,
        message: String,
    },
    #[error("local runtime for `{key_name}` is unavailable: {message}")]
    #[hint(
        "use a current local runtime manifest; start a fresh node if local state uses an unsupported schema"
    )]
    LocalRuntimeUnavailable {
        /// Account role or key whose runtime metadata was requested.
        key_name: String,
        /// Specific metadata lookup or validation failure.
        message: String,
    },
    #[error(
        "multiple wallets are saved for key `{key_name}` in profile `{profile}`: {wallet_names:?}"
    )]
    #[hint(
        "set `default_wallet` in the selected profile or pass the command's explicit wallet option"
    )]
    #[details]
    SavedWalletAmbiguous {
        key_name: String,
        profile: String,
        wallet_names: Vec<String>,
    },
    #[error("wallet `{wallet_name}` is not saved in profile `{profile}`")]
    #[hint("run `thruput wallet ls` and select a saved wallet name")]
    #[details]
    SavedWalletNotFound {
        wallet_name: String,
        profile: String,
    },
    #[error(
        "wallet `{wallet_name}` owner public key {configured} does not match key `{key_name}` public key {local}"
    )]
    #[hint("select a wallet owned by key `{key_name}`, or change the effective default key")]
    #[details]
    SavedWalletOwnerMismatch {
        wallet_name: String,
        key_name: String,
        configured: String,
        local: String,
    },
    #[error(
        "cannot remove wallet `{wallet_name}` from profile `{profile}` because it is the inherited default wallet"
    )]
    #[hint(
        "run `thruput wallet use <NAME> --profile={}` to select a project default before \
         removing this wallet",
        render_shell_arg(profile)
    )]
    #[details]
    InheritedDefaultWalletRemoval {
        wallet_name: String,
        profile: String,
    },
    #[error(
        "cannot remove wallet `{wallet_name}` from profile `{profile}` because it would expose inherited default wallet `{inherited_wallet}`, which is not saved"
    )]
    #[hint(
        "save the inherited wallet binding or select another project default with `thruput \
         wallet use <NAME> --profile={}`",
        render_shell_arg(profile)
    )]
    #[details]
    WalletRemovalExposesMissingDefault {
        wallet_name: String,
        inherited_wallet: String,
        profile: String,
    },
    #[error("saved wallet `{key_name}` owner_public_key is invalid: {message}")]
    #[hint(
        "recreate the wallet binding with `thruput wallet create`, or fix owner_public_key \
         in the selected profile"
    )]
    #[details]
    SavedWalletOwnerPublicKeyInvalid { key_name: String, message: String },
    #[error("transfer sender and recipient resolve to the same address `{address}`")]
    #[hint("select a different recipient or use `--from` to select another source wallet")]
    #[details]
    TransferToSelf { address: String },
    #[error("{path}: {message}")]
    #[hint(
        "check the reported argument path and use structured JSON for composite contract values"
    )]
    #[details]
    CallEncode { path: String, message: String },
    #[error("{path}: {message}")]
    #[hint(
        "check the reported return-value path and verify the resolved contract types match \
         the on-chain value"
    )]
    #[details]
    CallDecode { path: String, message: String },
    #[error("invalid storage key: {0}")]
    #[hint("pass a 32-byte storage key as 0x-prefixed hex")]
    InvalidStorageKey(String),
    #[error("invalid view calldata: {0}")]
    #[hint("pass raw calldata as 0x-prefixed hex, or omit it for empty calldata")]
    InvalidViewCalldata(String),
    #[error("invalid view arguments: {0}")]
    #[hint("use either positional method arguments or `--calldata`, not both")]
    InvalidViewArguments(String),
    #[error("codegen failed: {0}")]
    #[hint("check that the file contains a valid contract spec")]
    CodegenFailed(String),
    #[error("chain data directory is in use: {0}")]
    #[hint("stop the node using this data directory and retry")]
    ChainDataDirLocked(String),
    #[error("invalid chain archive at {path}: {message}")]
    #[hint("use an unmodified archive produced by `thruput chain dump`")]
    ChainArchiveInvalid { path: String, message: String },
    #[error("chain dump output already exists: {0}")]
    #[hint("review the existing archive and rerun with `--yes`, or choose another output path")]
    ChainDumpExists(String),
    #[error("chain archive path must be outside the data directory: {0}")]
    #[hint("place the archive outside the node data directory")]
    ChainArchiveInsideDataDir(String),
    #[error("chain archive operation failed: {0}")]
    #[hint("check path permissions and available disk space, then retry")]
    ChainArchiveIo(String),
    #[error("invalid {argument}: {message}")]
    #[hint("correct the {argument} and rerun; `--help` shows the expected format")]
    #[details]
    InvalidArgument {
        /// Which command argument was rejected.
        argument: String,
        /// Why the supplied value was rejected.
        message: String,
    },
    #[error("invalid `[package.metadata.thruput] {key}` in package `{package}`: {message}")]
    #[hint(
        "set `{key}` under `[package.metadata.thruput]` in the package's Cargo.toml \
         to a positive byte count: an integer, or a string such as \"128K\" or \
         \"0x20000\""
    )]
    #[details]
    PackageMetadataInvalid {
        /// The package whose manifest carries the value.
        package: String,
        /// The key under `[package.metadata.thruput]`.
        key: &'static str,
        /// Why the value was rejected.
        message: String,
    },
    /// The requested version, target, or host runtime is not supported by the catalog.
    #[error("toolchain catalog error: {0}")]
    #[code("toolchain_catalog_error")]
    #[hint(expr = Some(toolchain_catalog_hint(_0)))]
    ToolchainCatalog(#[from] thruput_toolchain::CatalogError),
    /// A verified bundle could not be installed or accessed in the local cache.
    #[error("toolchain installation error: {0}")]
    #[code("toolchain_install_error")]
    #[hint(expr = Some(toolchain_install_hint(_0)))]
    ToolchainInstall(thruput_toolchain::InstallError),
    /// The artifact source could not provide the selected bundle.
    #[error("toolchain source error: {0}")]
    #[code("toolchain_source_error")]
    #[hint(text = TOOLCHAIN_RETRY_HINT)]
    ToolchainSource(#[from] thruput_toolchain::SourceError),
    /// Offline acquisition cannot satisfy the selected managed bundle.
    #[error(
        "hog-devnode {version} for {target} is not installed and offline mode forbids downloading it"
    )]
    #[hint(
        "rerun `thruput toolchain install` without offline mode, or set \
         THRUPUT_DEVNODE_BINARY to a local build"
    )]
    #[details]
    ToolchainOfflineMissing { version: String, target: String },
    /// A read-only lookup found no complete installation for the selection.
    #[error("hog-devnode {version} for {target} is not installed")]
    #[hint("run `thruput toolchain install {}` first", render_shell_arg(version))]
    #[details]
    ToolchainNotInstalled { version: String, target: String },
    #[error("{0}")]
    Other(String),
}

/// Hint for a cargo failure whose captured output names a cause the CLI
/// recognizes.
fn cargo_failure_hint(stderr: &str) -> Cow<'static, str> {
    Cow::Borrowed(
        if stderr
            .contains("failed to select a version for the requirement `thruput-json-rpc-client =")
        {
            "the project's thruput-json-rpc-client version is unavailable in the configured registry; \
         use a published CLI with its matching dependencies, or patch the crates.io dependency to \
         the matching local RPC client source when testing an unpublished CLI"
        } else if stderr.contains("incompatible with `immediate-abort`") {
            "the prebuilt `core` uses a different panic strategy than the \
         `-C panic=immediate-abort` in `.cargo/config.toml`, so `core` has to \
         be rebuilt from source; check that `.cargo/contract.toml` still \
         contains `[unstable]` and `build-std = [\"core\", \"alloc\"]`"
        } else if stderr.contains("groundhog-rv32.ld") {
            "the link references `groundhog-rv32.ld`, the linker script's name before \
         Thruput SDK 0.56; SDK 0.56 and later ship it as `thruput-rv32.ld`, so \
         change `-Tgroundhog-rv32.ld` to `-Tthruput-rv32.ld` in `.cargo/config.toml`"
        } else if stderr.contains("thruput-rv32.ld") {
            "the link references `thruput-rv32.ld`, which the Thruput SDK's \
         build script supplies through a linker search path; check that the \
         `sdk` dependency resolves for this build, and rebuild after \
         `cargo clean` to re-run its build script"
        } else {
            CARGO_FAILED_HINT
        },
    )
}

/// Hint for a rejected protocol intro, naming the protocol to pass when the
/// node reported which one it expected.
fn protocol_intro_hint(expected_protocol: Option<&str>) -> Cow<'static, str> {
    match expected_protocol {
        Some(protocol) => Cow::Owned(format!(
            "pass `--hogd-protocol={}` to override, or rebuild thruput node \
             against a matching thruput-xdr",
            render_shell_arg(protocol),
        )),
        None => Cow::Borrowed(
            "pass `--hogd-protocol <hogd-protocol>` to override, or rebuild thruput node \
             against a matching thruput-xdr",
        ),
    }
}

/// Hint for a catalog lookup, separating an unsupported host runtime from a
/// selection the embedded catalog does not carry.
fn toolchain_catalog_hint(error: &thruput_toolchain::CatalogError) -> Cow<'static, str> {
    Cow::Borrowed(match error {
        thruput_toolchain::CatalogError::IncompatibleRuntime { .. }
        | thruput_toolchain::CatalogError::RuntimeProbeFailed { .. } => {
            "upgrade the host runtime, or set THRUPUT_DEVNODE_BINARY to a compatible local build"
        }
        _ => {
            "use a hog-devnode version and target present in this thruput release's \
             embedded catalog"
        }
    })
}

/// Hint for a failed installation, separating a cache another process holds
/// from the rest.
fn toolchain_install_hint(error: &thruput_toolchain::InstallError) -> Cow<'static, str> {
    Cow::Borrowed(match error {
        thruput_toolchain::InstallError::Busy { .. } => {
            "stop nodes using this toolchain or wait for installation to finish, then retry clean"
        }
        _ => TOOLCHAIN_RETRY_HINT,
    })
}

/// Hint for a lowering failure, keyed on the stage that rejected the ELF.
fn rv32_lower_hint(error: &thruput_rv32_lower::LowerError) -> Cow<'static, str> {
    Cow::Borrowed(match error {
        thruput_rv32_lower::LowerError::DataTooLarge { .. } => {
            "static data plus the reserved stack exceed the guest data \
             memory the contract declares; reduce static data, pass a \
             smaller `thruput build --stack-size` to shrink the stack \
             reserve, or raise `[package.metadata.thruput] \
             initial_memory_bytes` if the target chain's \
             `max_rv32_memory_bytes` admits more"
        }
        thruput_rv32_lower::LowerError::Verify(_) => {
            "this is a bug — please report this lower→verify \
             failure to the Thruput team"
        }
        _ => {
            "the RV32 ELF may have a malformed `.text`/data layout or a \
             nonzero entry point; see the lower error above"
        }
    })
}

/// Hint for a missing RV32 linker script, naming the member config to copy
/// when the workspace has one cargo will not read.
fn linker_script_hint(workspace_root: &str, member_config: Option<&str>) -> Cow<'static, str> {
    Cow::Owned(match member_config {
        Some(member_config) => format!(
            "cargo reads `.cargo/config.toml` from its invocation directory upward \
             and the build runs cargo at the workspace root — copy {member_config} \
             (and the adjacent `contract.toml`) into {workspace_root}/.cargo/ so the \
             Thruput rustflags apply"
        ),
        None => format!(
            "merge the `[target.riscv32im-unknown-none-elf]` rustflags from a \
             `thruput new` project's `.cargo/config.toml` into \
             {workspace_root}/.cargo/config.toml"
        ),
    })
}

impl From<thruput_toolchain::InstallError> for CliError {
    fn from(error: thruput_toolchain::InstallError) -> Self {
        match error {
            thruput_toolchain::InstallError::Catalog(error) => Self::ToolchainCatalog(error),
            thruput_toolchain::InstallError::Source(error) => Self::ToolchainSource(error),
            error => Self::ToolchainInstall(error),
        }
    }
}

impl From<thruput_client::codec::Error> for CliError {
    fn from(error: thruput_client::codec::Error) -> Self {
        use thruput_client::codec::Error;
        match error {
            Error::SpecFailed(message) => Self::SpecFailed(message),
            Error::InvalidArgument { argument, message } => {
                Self::InvalidArgument { argument, message }
            }
            Error::CallEncode { path, message } => Self::CallEncode { path, message },
            Error::CallDecode { path, message } => Self::CallDecode { path, message },
            Error::Other(message) => Self::Other(message),
        }
    }
}
