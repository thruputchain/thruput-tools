use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use groundhog_cli::Print;
use groundhog_cli::config::DEFAULT_PROFILE;
use groundhog_cli::result::CommandResult;
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "groundhog",
    about = "Groundhog smart contract development tools",
    version
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Parser)]
struct GlobalArgs {
    /// Suppress status messages
    #[arg(long, short, global = true)]
    quiet: bool,
    /// Emit structured JSON output
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Parser, Debug, Clone)]
struct ArtifactArgs {
    /// Directory for cargo build artifacts
    #[arg(long)]
    target_dir: Option<PathBuf>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum BuildTargetArg {
    Wasm,
    Rv32,
}

impl From<BuildTargetArg> for groundhog_cli::build::BuildTarget {
    fn from(value: BuildTargetArg) -> Self {
        match value {
            BuildTargetArg::Wasm => Self::Wasm,
            BuildTargetArg::Rv32 => Self::Rv32,
        }
    }
}

impl From<BuildTargetArg> for groundhog_cli::artifacts::ArtifactKind {
    fn from(value: BuildTargetArg) -> Self {
        match value {
            BuildTargetArg::Wasm => Self::Wasm,
            BuildTargetArg::Rv32 => Self::Rv32,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum NodeBlockDriverArg {
    Instant,
    Interval,
}

impl From<NodeBlockDriverArg> for groundhog_cli::node::LocalNodeBlockDriverMode {
    fn from(value: NodeBlockDriverArg) -> Self {
        match value {
            NodeBlockDriverArg::Instant => Self::Instant,
            NodeBlockDriverArg::Interval => Self::Interval,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Create a new Groundhog contract project
    New {
        /// Project name
        name: String,
        /// Project template
        #[arg(
            long,
            default_value = "counter",
            value_parser = ["counter", "stub"]
        )]
        template: String,
    },
    /// Interactive first-run setup wizard
    #[command(long_about = "Guided setup for new users: checks tools, \
                      generates a signing key, stores credentials, \
                      and optionally scaffolds a starter project.\n\n\
                      With --json, delegates to `groundhog status \
                      --json`.\n\n\
                      Re-running when already configured is a \
                      no-op.")]
    Init,
    /// Show environment, keys, and project status
    #[command(long_about = "Non-interactive diagnostic. Reports \
                      toolchain, keys, credentials, and project \
                      config. Use --json for scripting.\n\n\
                      Exit code 0 means everything looks good; \
                      exit code 1 means at least one warning-level \
                      finding exists.")]
    Status {
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
    },
    /// Build contract artifact
    #[command(long_about = "Cross-compile the contract to \
                      wasm32-unknown-unknown or \
                      riscv32im-unknown-none-elf via cargo. \
                      Must be run inside a Groundhog \
                      project.\n\n\
                      --optimize requires wasm-opt \
                      (install via `brew install binaryen`). \
                      --target rv32 uses the built-in RV32 lowerer.")]
    Build {
        /// Package to build (required in workspaces)
        #[arg(short, long)]
        package: Option<String>,
        /// Contract artifact target
        #[arg(long, value_enum, default_value_t = BuildTargetArg::Wasm)]
        target: BuildTargetArg,
        /// Run wasm-opt after compilation (requires wasm-opt)
        #[arg(long)]
        optimize: bool,
        /// Generate typed bindings after build (e.g. `ts`)
        #[arg(long)]
        bindings: Vec<String>,
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
        #[command(flatten)]
        artifacts: ArtifactArgs,
        /// Extra arguments passed to cargo
        #[arg(last = true)]
        cargo_args: Vec<String>,
    },
    /// Build and test the contract under WASM or local-node execution
    #[command(long_about = "Builds the contract to WASM, then runs \
                       the test_wasm target on the host. With \
                       --integration, starts an ephemeral hog-devnode \
                       local node, deploys the selected artifact \
                       (--target wasm by default, or --target rv32), and \
                       runs the integration test target with RPC environment \
                       variables. With --json, emits a structured report \
                       with mode, artifact metadata, summary, per-test cases, \
                       and integration node metadata when applicable.")]
    Test {
        /// Package to test (required in workspaces)
        #[arg(short, long)]
        package: Option<String>,
        /// Run only tests whose names contain this substring
        #[arg(long = "match-test")]
        match_test: Option<String>,
        /// Stop the WASM test harness after the first test failure
        #[arg(long)]
        fail_fast: bool,
        /// Show captured stdout for successful tests
        #[arg(long)]
        show_output: bool,
        /// Do not capture stdout/stderr from tests
        #[arg(long = "no-capture", alias = "nocapture")]
        no_capture: bool,
        /// Number of WASM test harness threads to use
        #[arg(long = "test-threads")]
        test_threads: Option<usize>,
        /// Skip tests whose names contain this substring
        #[arg(long)]
        skip: Vec<String>,
        /// Rerun tests that failed in the previous WASM test run
        #[arg(long)]
        rerun: bool,
        /// Rerun WASM tests when project files change.
        ///
        /// With --json, emits one newline-delimited JSON record per run.
        #[arg(long)]
        watch: bool,
        /// Run tests/integration.rs against an ephemeral hog-devnode local node
        #[arg(long)]
        integration: bool,
        /// Contract artifact target to deploy for integration tests
        #[arg(long, value_enum, requires = "integration")]
        target: Option<BuildTargetArg>,
        /// Override the x23 protocol token for the ephemeral integration node
        ///
        /// (Only used with --integration)
        /// Falls back to the `GROUNDHOG_HOGD_PROTOCOL` env var.
        #[arg(long = "hogd-protocol", requires = "integration")]
        hogd_protocol: Option<String>,
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
        #[command(flatten)]
        artifacts: ArtifactArgs,
        /// Extra arguments forwarded to the test harness (after --)
        #[arg(last = true)]
        cargo_args: Vec<String>,
    },
    /// Deploy a contract
    Deploy {
        /// Path to compiled .wasm or .rv32a (default: auto-detect)
        #[arg(long)]
        artifact: Option<PathBuf>,
        /// Artifact target to auto-detect when --artifact is omitted
        #[arg(long, value_enum, conflicts_with = "artifact")]
        target: Option<BuildTargetArg>,
        /// Build and sign the deploy transaction without submitting it
        #[arg(long)]
        dry_run: bool,
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
        #[command(flatten)]
        artifacts: ArtifactArgs,
    },
    /// Submit a transaction that calls a deployed contract method
    #[command(long_about = "Submit a signed transaction that calls a deployed \
                      contract method. Named methods use local or embedded \
                      contract metadata when available. Without metadata, \
                      pass an explicit 0x-prefixed 8-byte entrypoint ID \
                      and one raw hex calldata argument.")]
    Call {
        /// Contract address or alias
        contract: String,
        /// Method name or 0x-prefixed 8-byte entrypoint ID
        method: String,
        /// Method arguments. With an explicit entrypoint, pass one raw hex calldata argument.
        args: Vec<String>,
        /// Resolve, encode, and sign the transaction without submitting it
        #[arg(long)]
        dry_run: bool,
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
        #[command(flatten)]
        artifacts: ArtifactArgs,
    },
    /// Execute a sandboxed contract simulation against committed state
    #[command(long_about = "Execute a sandboxed contract simulation via \
                      groundhog_view. Named methods use local or embedded \
                      contract metadata when available. Without metadata, \
                      pass an explicit 0x-prefixed 8-byte entrypoint ID \
                      and raw calldata.")]
    View {
        /// Contract address or alias
        contract: String,
        /// Method name or 0x-prefixed 8-byte entrypoint ID
        method: String,
        /// Method arguments. With an explicit entrypoint, pass one raw hex calldata argument.
        args: Vec<String>,
        /// Raw calldata for explicit-entrypoint calls
        #[arg(long)]
        calldata: Option<String>,
        /// Simulated caller address, or dev-N on local dev endpoints
        #[arg(long = "from")]
        from: Option<String>,
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
        #[command(flatten)]
        artifacts: ArtifactArgs,
    },
    /// Read latest committed raw contract storage
    #[command(long_about = "Read one latest committed raw storage item via \
                      groundhog_getStorageAt. This is a low-level state \
                      inspection command for address/key reads; use \
                      `groundhog call` for signed transaction invocation \
                      and `groundhog view` for sandboxed contract \
                      simulation.")]
    Storage {
        /// Contract address or alias
        address: String,
        /// 32-byte storage key as 0x-prefixed hex
        key: String,
        /// Config profile to use
        #[arg(
            long = "profile",
            alias = "network",
            default_value = DEFAULT_PROFILE
        )]
        profile: String,
    },
    /// Start a local development node
    #[command(
        long_about = "Start a local development node \
                      backed by a supervised hog-devnode process. The \
                      native_exec/test_ffi prototype backend is no longer \
                      wired into this command.",
        after_help = "Protocol override:\n  --hogd-protocol hogp/<hash> overrides the x23 intro token used for local\n  hog-devnode handshakes. This is useful when a local hog-devnode build is ahead of\n  the published Rust protocol crates; generated XDR clients still fail if the\n  wire shape is actually incompatible.\n\nLogging:\n  By default, groundhog node prints its own lifecycle/status output and keeps forwarded\n  hog-devnode info logs quiet. Use --verbose for the normal debugging view, or RUST_LOG\n  for exact tracing filters. --verbose is also honored with --json; logs stay on stderr.\n\nEnvironment:\n  GROUNDHOG_HOG_DEVNODE_BINARY  Default hog-devnode binary path when --hog-devnode-binary is omitted\n  GROUNDHOG_HOGD_DEV_BINARY     Legacy alias for GROUNDHOG_HOG_DEVNODE_BINARY\n  RUST_LOG                      Overrides --verbose and controls supervisor/forwarded logs\n\nChild log bridge:\n  hog-devnode readiness is read from stdout. hog-devnode logs are requested as JSON on stderr,\n  parsed by node-supervisor, and re-emitted through tracing with hogd_timestamp_ms,\n  hogd_target, hogd_file, hogd_line, and hogd_fields metadata."
    )]
    Node {
        /// Port to listen on
        #[arg(long, default_value = "9545")]
        port: u16,
        /// Show forwarded hog-devnode info logs in addition to supervisor status
        #[arg(long, short = 'v')]
        verbose: bool,
        /// Number of deterministic dev accounts to bootstrap
        #[arg(long)]
        dev_account_count: Option<usize>,
        /// Explicit hog-devnode binary path
        #[arg(
            long = "hog-devnode-binary",
            alias = "hogd-dev-binary",
            value_name = "HOG_DEVNODE_BINARY"
        )]
        hogd_dev_binary: Option<std::path::PathBuf>,
        /// Override hog-devnode x23 intro protocol token
        #[arg(long)]
        hogd_protocol: Option<String>,
        /// Override hog-devnode readiness timeout for smoke tests
        #[arg(long, hide = true)]
        startup_timeout_secs: Option<u64>,
        /// Local block production mode
        #[arg(long, value_enum)]
        block_driver: Option<NodeBlockDriverArg>,
        /// Timer interval for --block-driver interval
        #[arg(long)]
        block_interval_ms: Option<u64>,
        /// Maximum simultaneous public JSON-RPC connections
        #[arg(long)]
        rpc_max_connections: Option<u32>,
    },
    /// Manage signing keys
    #[command(long_about = "Ed25519 signing keys stored in \
                      ~/.config/groundhog/keys/. Keys are \
                      global (not per-profile). Profiles can \
                      override the default key via default_key \
                      in groundhog.toml.")]
    Keys {
        #[command(subcommand)]
        command: KeysCommand,
    },
    /// Configure registry and test executor credentials
    Login,
    /// Remove stored credentials
    Logout,
    /// Generate typed bindings from a contract spec
    Codegen {
        #[command(subcommand)]
        command: CodegenCommand,
    },
    /// Generate shell completions
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Generate a new keypair
    Generate {
        /// Key name (default: "default")
        name: Option<String>,
        /// Fund from testnet faucet (not yet implemented)
        #[arg(long, hide = true)]
        fund: bool,
    },
    /// List stored keys
    Ls,
    /// Set the default signing key
    Use {
        /// Key name to use as default
        name: String,
    },
    /// Remove a stored key
    Rm {
        /// Key name to remove
        name: String,
    },
}

#[derive(Subcommand)]
enum CodegenCommand {
    /// Generate TypeScript bindings
    Ts {
        /// Path to .wasm or .spec.xdr file
        source: std::path::PathBuf,
        /// Output file path (default: stdout)
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
    },
}

impl CodegenCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Ts { .. } => "codegen.ts",
        }
    }
}

impl Command {
    /// Canonical command name for JSON envelopes.
    ///
    /// Used by both success and error JSON output so they
    /// always agree. Init returns "status" because
    /// `init --json` delegates to `status --json`.
    fn command_name(&self) -> &'static str {
        match self {
            Self::New { .. } => "new",
            Self::Init | Self::Status { .. } => "status",
            Self::Build { .. } => "build",
            Self::Test { .. } => "test",
            Self::Deploy { .. } => "deploy",
            Self::Call { .. } => "call",
            Self::View { .. } => "view",
            Self::Storage { .. } => "storage",
            Self::Node { .. } => "node",
            Self::Keys { command } => command.command_name(),
            Self::Codegen { command } => command.command_name(),
            Self::Login => "login",
            Self::Logout => "logout",
            Self::Completion { .. } => "completion",
        }
    }
}

impl KeysCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Generate { .. } => "keys.generate",
            Self::Ls => "keys.ls",
            Self::Use { .. } => "keys.use",
            Self::Rm { .. } => "keys.rm",
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() {
    let cli = Cli::parse();
    let print = Print::new(cli.global.quiet, cli.global.json);
    let cmd_name = cli.command.command_name();

    let result = match cli.command {
        Command::New { name, template } => {
            groundhog_cli::new::run(&name, &template, cmd_name, &print)
        }
        Command::Init => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| groundhog_cli::init::run(&cwd, cmd_name, &print)),
        Command::Status { profile } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| groundhog_cli::status::run(&cwd, Some(&profile), cmd_name, &print)),
        Command::Build {
            package,
            target,
            optimize,
            bindings,
            profile,
            artifacts,
            cargo_args,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                let artifacts = groundhog_cli::artifacts::ArtifactOptions::from_invocation_cwd(
                    artifacts.target_dir,
                    &cwd,
                );
                groundhog_cli::build::run(
                    package.as_deref(),
                    target.into(),
                    optimize,
                    &bindings,
                    &cargo_args,
                    &artifacts,
                    Some(&profile),
                    cmd_name,
                    &print,
                )
            }),
        Command::Test {
            package,
            match_test,
            fail_fast,
            show_output,
            no_capture,
            test_threads,
            skip,
            rerun,
            watch,
            integration,
            target,
            hogd_protocol,
            profile,
            artifacts,
            cargo_args,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                let artifacts = groundhog_cli::artifacts::ArtifactOptions::from_invocation_cwd(
                    artifacts.target_dir,
                    &cwd,
                );
                let options = groundhog_cli::test_cmd::TestOptions {
                    match_test,
                    test_threads,
                    skip,
                    rerun,
                    watch,
                    harness: groundhog_cli::test_cmd::TestHarnessOptions {
                        fail_fast,
                        show_output,
                        no_capture,
                    },
                };
                if integration {
                    groundhog_cli::test_cmd::run_integration_with_options(
                        package.as_deref(),
                        &options,
                        &groundhog_cli::test_cmd::IntegrationTestOptions {
                            hogd_protocol,
                            target: target.unwrap_or(BuildTargetArg::Wasm).into(),
                        },
                        &cargo_args,
                        &artifacts,
                        Some(&profile),
                        cmd_name,
                        &print,
                    )
                } else {
                    groundhog_cli::test_cmd::run_with_options(
                        package.as_deref(),
                        &options,
                        &cargo_args,
                        &artifacts,
                        Some(&profile),
                        cmd_name,
                        &print,
                    )
                }
            }),
        Command::Deploy {
            artifact,
            target,
            dry_run,
            profile,
            artifacts,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                let artifacts = groundhog_cli::artifacts::ArtifactOptions::from_invocation_cwd(
                    artifacts.target_dir,
                    &cwd,
                );
                groundhog_cli::deploy::run(
                    artifact.as_deref(),
                    target.unwrap_or(BuildTargetArg::Wasm).into(),
                    dry_run,
                    &artifacts,
                    &profile,
                    cmd_name,
                    &print,
                )
            }),
        Command::Call {
            contract,
            method,
            args,
            dry_run,
            profile,
            artifacts,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                let artifacts = groundhog_cli::artifacts::ArtifactOptions::from_invocation_cwd(
                    artifacts.target_dir,
                    &cwd,
                );
                groundhog_cli::call::run(
                    &contract, &method, &args, dry_run, &artifacts, &profile, cmd_name, &print,
                )
            }),
        Command::View {
            contract,
            method,
            args,
            calldata,
            from,
            profile,
            artifacts,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                let artifacts = groundhog_cli::artifacts::ArtifactOptions::from_invocation_cwd(
                    artifacts.target_dir,
                    &cwd,
                );
                groundhog_cli::view::run(
                    &contract,
                    &method,
                    &args,
                    calldata.as_deref(),
                    from.as_deref(),
                    &artifacts,
                    &cwd,
                    &profile,
                    cmd_name,
                    &print,
                )
            }),
        Command::Storage {
            address,
            key,
            profile,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                groundhog_cli::storage::run(&address, &key, &cwd, &profile, cmd_name, &print)
            }),
        Command::Node {
            port,
            verbose,
            dev_account_count,
            hogd_dev_binary,
            hogd_protocol,
            startup_timeout_secs,
            block_driver,
            block_interval_ms,
            rpc_max_connections,
        } => groundhog_cli::node::run_with_block_driver(
            port,
            verbose,
            dev_account_count,
            hogd_dev_binary,
            hogd_protocol,
            startup_timeout_secs,
            block_driver.map(Into::into),
            block_interval_ms,
            rpc_max_connections,
            cmd_name,
            &print,
        ),
        Command::Keys { command } => match command {
            KeysCommand::Generate { name, fund } => {
                groundhog_cli::keys::generate(&name, fund, cmd_name, &print)
            }
            KeysCommand::Ls => groundhog_cli::keys::list(cmd_name, &print),
            KeysCommand::Use { name } => groundhog_cli::keys::use_key(&name, cmd_name, &print),
            KeysCommand::Rm { name } => groundhog_cli::keys::remove(&name, cmd_name, &print),
        },
        Command::Codegen { command } => match command {
            CodegenCommand::Ts { source, output } => {
                groundhog_cli::codegen_cmd::run(&source, output.as_deref(), cmd_name, &print)
            }
        },
        Command::Login => groundhog_cli::auth::login(cmd_name, &print),
        Command::Logout => groundhog_cli::auth::logout(cmd_name, &print),
        Command::Completion { shell } => {
            let mut output = Vec::new();
            let shell_name = shell.to_string();
            clap_complete::generate(shell, &mut Cli::command(), "groundhog", &mut output);

            if print.is_json() {
                print.json(&CommandResult::new(
                    cmd_name,
                    json!({
                        "shell": shell_name,
                        "script": String::from_utf8_lossy(&output),
                    }),
                ))
            } else {
                print!("{}", String::from_utf8_lossy(&output));
                Ok(())
            }
        }
    };
    if let Err(e) = result {
        if matches!(
            e,
            groundhog_cli::CliError::StatusHasWarnings
                | groundhog_cli::CliError::CommandReportedFailure
        ) {
            std::process::exit(1);
        }
        if let Err(print_error) = print.command_error(cmd_name, &e) {
            eprintln!("error: {print_error}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_dev_account_count_flag_parses() {
        let cli = Cli::parse_from(["groundhog", "node", "--dev-account-count", "3"]);

        match cli.command {
            Command::Node {
                dev_account_count, ..
            } => assert_eq!(dev_account_count, Some(3)),
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_block_driver_flags_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "node",
            "--block-driver",
            "interval",
            "--block-interval-ms",
            "250",
            "--rpc-max-connections",
            "512",
        ]);

        match cli.command {
            Command::Node {
                block_driver,
                block_interval_ms,
                rpc_max_connections,
                ..
            } => {
                assert!(matches!(block_driver, Some(NodeBlockDriverArg::Interval)));
                assert_eq!(block_interval_ms, Some(250));
                assert_eq!(rpc_max_connections, Some(512));
            }
            _ => panic!("expected node command"),
        }
    }
}
