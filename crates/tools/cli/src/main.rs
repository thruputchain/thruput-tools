use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use groundhog_cli::Print;
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
    /// Print each cargo command to stderr before running it
    #[arg(long, global = true)]
    show_cargo_commands: bool,
}

#[derive(Parser, Debug, Clone)]
struct ArtifactArgs {
    /// Directory for cargo build artifacts
    #[arg(long)]
    target_dir: Option<PathBuf>,
}

#[derive(Parser, Debug, Clone, Default)]
struct RpcTargetArgs {
    /// Override the active profile's RPC URL
    #[arg(long)]
    rpc_url: Option<String>,
    /// Override local-dev runtime manifest path for dev-N/bootstrap accounts
    #[arg(long)]
    runtime_manifest: Option<PathBuf>,
}

impl From<RpcTargetArgs> for groundhog_cli::config::ProfileOverrides {
    fn from(value: RpcTargetArgs) -> Self {
        Self {
            rpc_url: value.rpc_url,
            runtime_manifest: value.runtime_manifest,
        }
    }
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
                      generates a signing key, stores mock-executor download credentials, \
                      and optionally scaffolds a starter project.\n\n\
                      With --json, delegates to `groundhog status \
                      --json`.\n\n\
                      Re-running when already configured is a \
                      no-op.")]
    Init,
    /// Show RV32, devnode, keys, and project status
    #[command(long_about = "Non-interactive diagnostic. Reports \
                      the RV32 toolchain, hog-devnode availability, keys, mock-executor \
                      download authentication, and project config. Use --json for \
                      scripting.\n\n\
                      Exit code 0 means everything looks good; \
                      exit code 1 means at least one warning-level \
                      finding exists.")]
    Status {
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
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
        #[arg(long, value_enum, default_value_t = BuildTargetArg::Rv32)]
        target: BuildTargetArg,
        /// Run wasm-opt after compilation (requires wasm-opt)
        #[arg(long)]
        optimize: bool,
        /// Generate typed bindings after build (e.g. `ts`)
        #[arg(long)]
        bindings: Vec<String>,
        /// Guest stack size in bytes (decimal or 0x hex; multiple of 16,
        /// below 64K). RV32 only; sets the SDK linker script's `__stack_size`.
        #[arg(long, value_parser = groundhog_cli::build::parse_stack_size)]
        stack_size: Option<u32>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        artifacts: ArtifactArgs,
        /// Extra arguments passed to cargo
        #[arg(last = true)]
        cargo_args: Vec<String>,
    },
    /// Build and test the contract under RV32, WASM, or local-node execution
    #[command(
        long_about = "Builds the contract to the selected local test target \
                       (--target rv32 by default, or --target wasm), then runs \
                       test_rv32 or test_wasm on the host. With \
                       --integration, starts an ephemeral hog-devnode \
                       local node, deploys the selected artifact \
                       (--target rv32 by default, or --target wasm), and \
                       runs the integration test target with RPC environment \
                       variables. With --integration --differential, runs the \
                       same integration target against WASM and RV32 artifacts \
                       on isolated local nodes. With --json, emits a structured report \
                       with mode, artifact metadata, summary, per-test cases, \
                       and integration node metadata when applicable.",
        after_help = "Logging:\n  With --integration, local-node supervisor info logs \
                        are written to stderr;\n  add --verbose/-v to also forward \
                        hog-devnode info logs. RUST_LOG overrides\n  the default filter, \
                        for example \
                        RUST_LOG=groundhog_node_supervisor=debug,hogd_dev=debug."
    )]
    Test {
        /// Package to test (required in workspaces)
        #[arg(short, long)]
        package: Option<String>,
        /// Run only tests whose names contain this substring
        #[arg(long = "match-test")]
        match_test: Option<String>,
        /// Stop the selected local test harness after the first test failure
        #[arg(long)]
        fail_fast: bool,
        /// Show captured stdout for successful tests
        #[arg(long)]
        show_output: bool,
        /// Do not capture stdout/stderr from tests
        #[arg(long = "no-capture", alias = "nocapture")]
        no_capture: bool,
        /// Number of local test harness threads to use
        #[arg(long = "test-threads")]
        test_threads: Option<usize>,
        /// Skip tests whose names contain this substring
        #[arg(long)]
        skip: Vec<String>,
        /// Rerun tests that failed in the previous selected-target test run
        #[arg(long)]
        rerun: bool,
        /// Rerun selected-target tests when project files change.
        ///
        /// With --json, emits one newline-delimited JSON record per run.
        #[arg(long)]
        watch: bool,
        /// Run tests/integration.rs against an ephemeral hog-devnode local node
        #[arg(long)]
        integration: bool,
        /// Contract artifact target to test
        #[arg(long, value_enum)]
        target: Option<BuildTargetArg>,
        /// Run integration tests against both WASM and RV32 artifacts
        #[arg(long, requires = "integration", conflicts_with = "target")]
        differential: bool,
        /// Show local-node supervisor and forwarded hog-devnode diagnostics
        ///
        /// Only used with --integration. Logs are written to stderr.
        #[arg(long, short = 'v', requires = "integration")]
        verbose: bool,
        /// Override the x23 protocol token for the ephemeral integration node
        ///
        /// (Only used with --integration)
        /// Falls back to the `GROUNDHOG_HOGD_PROTOCOL` env var.
        #[arg(long = "hogd-protocol", requires = "integration")]
        hogd_protocol: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
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
        /// Saved wallet to deploy from
        #[arg(long)]
        wallet: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
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
        /// Saved wallet to call from
        #[arg(long)]
        wallet: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
        #[command(flatten)]
        artifacts: ArtifactArgs,
    },
    /// Transfer native HOG from the effective profile wallet
    Transfer {
        /// Wallet name, contract alias, dev-N account, or direct address
        recipient: String,
        /// Amount in HOG, with up to 7 decimal places
        amount: String,
        /// Build and sign the transfer without submitting it
        #[arg(long)]
        dry_run: bool,
        /// Saved source wallet owned by the effective signing key
        #[arg(long = "from")]
        from: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// Request HOG funding for a wallet
    Airdrop {
        /// Wallet name, dev-N wallet, or direct 0x-prefixed wallet address
        recipient: String,
        /// Amount in HOG, with up to 7 decimal places
        #[arg(long)]
        amount: Option<String>,
        /// Build and sign the airdrop transaction without submitting it
        #[arg(long)]
        dry_run: bool,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// Read native HOG balance for an address, wallet, contract, or dev account
    Balance {
        /// Wallet name, contract alias, dev-N account, or direct 0x-prefixed address
        target: String,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
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
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
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
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// Start a local development node
    #[command(
        long_about = "Start a local development node \
                      backed by a supervised hog-devnode process. The \
                      native_exec/test_ffi prototype backend is no longer \
                      wired into this command.",
        after_help = "Protocol override:\n  --hogd-protocol hogp/<hash> overrides the x23 intro token used for local\n  hog-devnode handshakes. This is useful when a local hog-devnode build is ahead of\n  the published Rust protocol crates; generated XDR clients still fail if the\n  wire shape is actually incompatible.\n\nLogging:\n  By default, groundhog node prints its own lifecycle/status output and keeps forwarded\n  hog-devnode info logs quiet. Use --verbose for the normal debugging view, or RUST_LOG\n  for exact tracing filters. --verbose is also honored with --json; logs stay on stderr.\n\nEnvironment:\n  GROUNDHOG_DEVNODE_BINARY  Default hog-devnode binary path when --hog-devnode-binary is omitted\n  RUST_LOG                  Overrides --verbose and controls supervisor/forwarded logs\n\nChild log bridge:\n  hog-devnode readiness is read from stdout. hog-devnode logs are requested as JSON on stderr,\n  parsed by node-supervisor, and re-emitted through tracing with hogd_timestamp_ms,\n  hogd_target, hogd_file, hogd_line, and hogd_fields metadata."
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
        #[arg(long = "hog-devnode-binary", value_name = "GROUNDHOG_DEVNODE_BINARY")]
        hogd_dev_binary: Option<std::path::PathBuf>,
        /// Directory for persistent hog-devnode state; omit for an ephemeral node
        #[arg(long, value_name = "DIR")]
        data_dir: Option<PathBuf>,
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
                       in project or global config.")]
    Keys {
        #[command(subcommand)]
        command: KeysCommand,
    },
    /// Manage contract wallets
    Wallet {
        #[command(subcommand)]
        command: WalletCommand,
    },
    /// Generate typed bindings from a contract spec
    Codegen {
        #[command(subcommand)]
        command: CodegenCommand,
    },
    /// Inspect public transactions and address history
    Tx {
        #[command(subcommand)]
        command: TxCommand,
    },
    /// Inspect indexed contract events
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Inspect canonical token activity
    Token {
        #[command(subcommand)]
        command: TokenCommand,
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
enum WalletCommand {
    /// List saved project wallets
    Ls {
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
    },
    /// Set the default transaction wallet
    Use {
        /// Saved wallet name to use by default
        name: String,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
    },
    /// Prefund and create a wallet for a stored signing key
    Create {
        /// Existing key name that should own the new wallet
        #[arg(long)]
        key: Option<String>,
        /// Saved wallet name; defaults to the resolved key name
        #[arg(long)]
        name: Option<String>,
        /// Build and sign the wallet create transaction without submitting it
        #[arg(long)]
        dry_run: bool,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// Remove a saved project wallet entry
    Rm {
        /// Saved wallet name to remove from project or global config
        name: String,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
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

#[derive(Subcommand)]
enum TxCommand {
    /// Show one transaction's public status and receipt
    #[command(
        long_about = "Perform a one-shot public transaction lookup without waiting.\n\n\
                       Shows only public status and receipt fields. Raw transaction bodies, \
                       inferred transfers, and deep traces are not available."
    )]
    Show {
        /// Transaction hash as exactly 32 bytes of hex
        tx_hash: String,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// List one page of root-invocation transaction history
    #[command(
        long_about = "List exactly one page of transactions whose root invocation targets \
                       the resolved address. Incoming wallet-wrapped transfers may not appear \
                       when querying the recipient.\n\nThe continuation cursor is opaque and reusable. \
                       Receipts are not fetched; inspect selected hashes with `groundhog tx show`."
    )]
    List {
        /// Wallet name, contract alias, dev-N account, or direct address
        target: String,
        /// Number of transaction references to request (default: 50)
        #[arg(long, allow_hyphen_values = true)]
        limit: Option<String>,
        /// Opaque cursor returned by a previous page
        #[arg(long)]
        cursor: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
}

#[derive(Subcommand)]
enum EventsCommand {
    /// List one page of raw events emitted by a contract
    #[command(
        long_about = "List exactly one newest-first page of raw events emitted by one resolved \
                       contract address. A global event scan is not performed.\n\nRepeat a topic \
                       option for OR within that position; constraints across topic positions \
                       combine with AND. Raw topics and data are authoritative. The continuation \
                       cursor is opaque and must be reused with the same contract and filters."
    )]
    List {
        /// Wallet name, contract alias, dev-N account, or direct address
        contract: String,
        /// Topic-0 value; repeat for OR within this position
        #[arg(long)]
        topic0: Vec<String>,
        /// Topic-1 value; repeat for OR within this position
        #[arg(long)]
        topic1: Vec<String>,
        /// Topic-2 value; repeat for OR within this position
        #[arg(long)]
        topic2: Vec<String>,
        /// Number of events to request (default: 50)
        #[arg(long, allow_hyphen_values = true)]
        limit: Option<String>,
        /// Opaque cursor returned by a previous page
        #[arg(long)]
        cursor: Option<String>,
        /// Trusted local .wasm or .xdr artifact used to decode matching events
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Case-sensitive event name declared by --spec
        #[arg(long)]
        event: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// List one page of canonical ERC20 transfer history
    #[command(
        long_about = "List exactly one newest-first page of canonical ERC20 TransferEvent records.\n\n\
                      Participant filters are applied by the server. --account matches transfers \
                      where the address is either sender or recipient. Amounts remain unformatted \
                      base-unit decimal strings; token metadata is not fetched."
    )]
    Transfers {
        /// Token contract alias, wallet name, dev-N account, or direct address
        token: String,
        /// Include only transfers from this resolved address
        #[arg(long)]
        from: Option<String>,
        /// Include only transfers to this resolved address
        #[arg(long)]
        to: Option<String>,
        /// Include transfers where this address is sender or recipient
        #[arg(long)]
        account: Option<String>,
        /// Number of transfer records to request (default: 50)
        #[arg(long, allow_hyphen_values = true)]
        limit: Option<String>,
        /// Opaque cursor returned by a previous page
        #[arg(long)]
        cursor: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
}

impl CodegenCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Ts { .. } => "codegen.ts",
        }
    }
}

impl TxCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Show { .. } => "tx.show",
            Self::List { .. } => "tx.list",
        }
    }
}

impl EventsCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::List { .. } => "events.list",
        }
    }
}

impl TokenCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Transfers { .. } => "token.transfers",
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
            Self::Transfer { .. } => "transfer",
            Self::Airdrop { .. } => "airdrop",
            Self::Balance { .. } => "balance",
            Self::View { .. } => "view",
            Self::Storage { .. } => "storage",
            Self::Node { .. } => "node",
            Self::Keys { command } => command.command_name(),
            Self::Wallet { command } => command.command_name(),
            Self::Codegen { command } => command.command_name(),
            Self::Tx { command } => command.command_name(),
            Self::Events { command } => command.command_name(),
            Self::Token { command } => command.command_name(),
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

impl WalletCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Ls { .. } => "wallet.ls",
            Self::Use { .. } => "wallet.use",
            Self::Create { .. } => "wallet.create",
            Self::Rm { .. } => "wallet.rm",
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() {
    let cli = Cli::parse();
    let print = Print::new(cli.global.quiet, cli.global.json)
        .with_command_trace(cli.global.show_cargo_commands);
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
            .and_then(|cwd| groundhog_cli::status::run(&cwd, profile.as_deref(), cmd_name, &print)),
        Command::Build {
            package,
            target,
            optimize,
            bindings,
            stack_size,
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
                    stack_size,
                    &cargo_args,
                    &artifacts,
                    profile.as_deref(),
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
            differential,
            verbose,
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
                    target: target.unwrap_or(BuildTargetArg::Rv32).into(),
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
                            target: options.target,
                            differential,
                            verbose,
                        },
                        &cargo_args,
                        &artifacts,
                        profile.as_deref(),
                        cmd_name,
                        &print,
                    )
                } else {
                    groundhog_cli::test_cmd::run_with_options(
                        package.as_deref(),
                        &options,
                        &cargo_args,
                        &artifacts,
                        profile.as_deref(),
                        cmd_name,
                        &print,
                    )
                }
            }),
        Command::Deploy {
            artifact,
            target,
            dry_run,
            wallet,
            profile,
            target_args,
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
                    target.unwrap_or(BuildTargetArg::Rv32).into(),
                    dry_run,
                    wallet.as_deref(),
                    &artifacts,
                    profile.as_deref(),
                    &target_args.into(),
                    cmd_name,
                    &print,
                )
            }),
        Command::Call {
            contract,
            method,
            args,
            dry_run,
            wallet,
            profile,
            target_args,
            artifacts,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                let artifacts = groundhog_cli::artifacts::ArtifactOptions::from_invocation_cwd(
                    artifacts.target_dir,
                    &cwd,
                );
                groundhog_cli::call::run(
                    &contract,
                    &method,
                    &args,
                    dry_run,
                    wallet.as_deref(),
                    &artifacts,
                    profile.as_deref(),
                    &target_args.into(),
                    cmd_name,
                    &print,
                )
            }),
        Command::Transfer {
            recipient,
            amount,
            dry_run,
            from,
            profile,
            target_args,
        } => groundhog_cli::transfer::run(
            &recipient,
            &amount,
            from.as_deref(),
            dry_run,
            profile.as_deref(),
            &target_args.into(),
            cmd_name,
            &print,
        ),
        Command::Airdrop {
            recipient,
            amount,
            dry_run,
            profile,
            target_args,
        } => groundhog_cli::airdrop::run(
            &recipient,
            amount.as_deref(),
            dry_run,
            profile.as_deref(),
            &target_args.into(),
            cmd_name,
            &print,
        ),
        Command::Balance {
            target,
            profile,
            target_args,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                groundhog_cli::balance::run(
                    &target,
                    &cwd,
                    profile.as_deref(),
                    &target_args.into(),
                    cmd_name,
                    &print,
                )
            }),
        Command::View {
            contract,
            method,
            args,
            calldata,
            from,
            profile,
            target_args,
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
                    profile.as_deref(),
                    &target_args.into(),
                    cmd_name,
                    &print,
                )
            }),
        Command::Storage {
            address,
            key,
            profile,
            target_args,
        } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| {
                groundhog_cli::storage::run(
                    &address,
                    &key,
                    &cwd,
                    profile.as_deref(),
                    &target_args.into(),
                    cmd_name,
                    &print,
                )
            }),
        Command::Node {
            port,
            verbose,
            dev_account_count,
            hogd_dev_binary,
            data_dir,
            hogd_protocol,
            startup_timeout_secs,
            block_driver,
            block_interval_ms,
            rpc_max_connections,
        } => {
            let mut options = groundhog_cli::node::LocalNodeOptions::default();
            options.port = port;
            options.hogd_dev_binary = hogd_dev_binary;
            options.data_dir = data_dir;
            options.hogd_protocol = hogd_protocol;
            options.startup_timeout = startup_timeout_secs.map(std::time::Duration::from_secs);
            options.dev_account_count = dev_account_count;
            options.block_driver = block_driver.map(Into::into);
            options.block_interval = block_interval_ms.map(std::time::Duration::from_millis);
            options.rpc_max_connections = rpc_max_connections;
            groundhog_cli::node::run_with_options(options, verbose, cmd_name, &print)
        }
        Command::Keys { command } => match command {
            KeysCommand::Generate { name, fund } => {
                groundhog_cli::keys::generate(&name, fund, cmd_name, &print)
            }
            KeysCommand::Ls => groundhog_cli::keys::list(cmd_name, &print),
            KeysCommand::Use { name } => groundhog_cli::keys::use_key(&name, cmd_name, &print),
            KeysCommand::Rm { name } => groundhog_cli::keys::remove(&name, cmd_name, &print),
        },
        Command::Wallet { command } => match command {
            WalletCommand::Ls { profile } => {
                groundhog_cli::wallet::list(profile.as_deref(), cmd_name, &print)
            }
            WalletCommand::Use { name, profile } => {
                groundhog_cli::wallet::use_wallet(&name, profile.as_deref(), cmd_name, &print)
            }
            WalletCommand::Create {
                key,
                name,
                dry_run,
                profile,
                target_args,
            } => groundhog_cli::wallet::create(
                key.as_deref(),
                name.as_deref(),
                dry_run,
                profile.as_deref(),
                &target_args.into(),
                cmd_name,
                &print,
            ),
            WalletCommand::Rm { name, profile } => {
                groundhog_cli::wallet::remove(&name, profile.as_deref(), cmd_name, &print)
            }
        },
        Command::Codegen { command } => match command {
            CodegenCommand::Ts { source, output } => {
                groundhog_cli::codegen_cmd::run(&source, output.as_deref(), cmd_name, &print)
            }
        },
        Command::Tx { command } => match command {
            TxCommand::Show {
                tx_hash,
                profile,
                target_args,
            } => std::env::current_dir()
                .map_err(groundhog_cli::CliError::Io)
                .and_then(|cwd| {
                    groundhog_cli::tx::show(
                        &tx_hash,
                        &cwd,
                        profile.as_deref(),
                        &target_args.into(),
                        cmd_name,
                        &print,
                    )
                }),
            TxCommand::List {
                target,
                limit,
                cursor,
                profile,
                target_args,
            } => std::env::current_dir()
                .map_err(groundhog_cli::CliError::Io)
                .and_then(|cwd| {
                    groundhog_cli::tx::list(
                        &target,
                        limit.as_deref(),
                        cursor.as_deref(),
                        &cwd,
                        profile.as_deref(),
                        &target_args.into(),
                        cmd_name,
                        &print,
                    )
                }),
        },
        Command::Events { command } => match command {
            EventsCommand::List {
                contract,
                topic0,
                topic1,
                topic2,
                limit,
                cursor,
                spec,
                event,
                profile,
                target_args,
            } => std::env::current_dir()
                .map_err(groundhog_cli::CliError::Io)
                .and_then(|cwd| {
                    groundhog_cli::events::list(
                        &contract,
                        &topic0,
                        &topic1,
                        &topic2,
                        limit.as_deref(),
                        cursor.as_deref(),
                        spec.as_deref(),
                        event.as_deref(),
                        &cwd,
                        profile.as_deref(),
                        &target_args.into(),
                        cmd_name,
                        &print,
                    )
                }),
        },
        Command::Token { command } => match command {
            TokenCommand::Transfers {
                token,
                from,
                to,
                account,
                limit,
                cursor,
                profile,
                target_args,
            } => std::env::current_dir()
                .map_err(groundhog_cli::CliError::Io)
                .and_then(|cwd| {
                    groundhog_cli::token::transfers(
                        &token,
                        from.as_deref(),
                        to.as_deref(),
                        account.as_deref(),
                        limit.as_deref(),
                        cursor.as_deref(),
                        &cwd,
                        profile.as_deref(),
                        &target_args.into(),
                        cmd_name,
                        &print,
                    )
                }),
        },
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
    fn node_data_dir_flag_parses() {
        let cli = Cli::parse_from(["groundhog", "node", "--data-dir", "/tmp/groundhog"]);

        match cli.command {
            Command::Node { data_dir, .. } => {
                assert_eq!(data_dir, Some(PathBuf::from("/tmp/groundhog")));
            }
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

    #[test]
    fn node_profile_flag_is_not_supported() {
        let result = Cli::try_parse_from(["groundhog", "node", "--profile", "local"]);
        let Err(err) = result else {
            panic!("node --profile should be rejected");
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn wallet_create_key_flag_parses() {
        let cli = Cli::parse_from(["groundhog", "wallet", "create", "--key", "alice"]);

        match cli.command {
            Command::Wallet {
                command:
                    WalletCommand::Create {
                        key,
                        name,
                        dry_run,
                        profile,
                        ..
                    },
            } => {
                assert_eq!(key.as_deref(), Some("alice"));
                assert_eq!(name, None);
                assert!(!dry_run);
                assert_eq!(profile, None);
            }
            _ => panic!("expected wallet create command"),
        }
    }

    #[test]
    fn wallet_ls_flags_parse() {
        let cli = Cli::parse_from(["groundhog", "wallet", "ls"]);

        match cli.command {
            Command::Wallet {
                command: WalletCommand::Ls { profile },
            } => assert_eq!(profile, None),
            _ => panic!("expected wallet ls command"),
        }
    }

    #[test]
    fn wallet_use_flags_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "wallet",
            "use",
            "trading",
            "--profile",
            "devnet",
        ]);

        match cli.command {
            Command::Wallet {
                command: WalletCommand::Use { name, profile },
            } => {
                assert_eq!(name, "trading");
                assert_eq!(profile.as_deref(), Some("devnet"));
            }
            _ => panic!("expected wallet use command"),
        }
    }

    #[test]
    fn wallet_create_key_flag_is_optional() {
        let cli = Cli::parse_from(["groundhog", "wallet", "create", "--dry-run"]);

        match cli.command {
            Command::Wallet {
                command:
                    WalletCommand::Create {
                        key,
                        name,
                        dry_run,
                        profile,
                        ..
                    },
            } => {
                assert_eq!(key, None);
                assert_eq!(name, None);
                assert!(dry_run);
                assert_eq!(profile, None);
            }
            _ => panic!("expected wallet create command"),
        }
    }

    #[test]
    fn wallet_create_name_flag_parses() {
        let cli = Cli::parse_from([
            "groundhog",
            "wallet",
            "create",
            "--key",
            "alice",
            "--name",
            "trading",
        ]);

        match cli.command {
            Command::Wallet {
                command:
                    WalletCommand::Create {
                        key,
                        name,
                        dry_run,
                        profile,
                        ..
                    },
            } => {
                assert_eq!(key.as_deref(), Some("alice"));
                assert_eq!(name.as_deref(), Some("trading"));
                assert!(!dry_run);
                assert_eq!(profile, None);
            }
            _ => panic!("expected wallet create command"),
        }
    }

    #[test]
    fn wallet_create_name_flag_without_key_parses() {
        let cli = Cli::parse_from([
            "groundhog",
            "wallet",
            "create",
            "--name",
            "trading",
            "--dry-run",
        ]);

        match cli.command {
            Command::Wallet {
                command:
                    WalletCommand::Create {
                        key,
                        name,
                        dry_run,
                        profile,
                        ..
                    },
            } => {
                assert_eq!(key, None);
                assert_eq!(name.as_deref(), Some("trading"));
                assert!(dry_run);
                assert_eq!(profile, None);
            }
            _ => panic!("expected wallet create command"),
        }
    }

    #[test]
    fn wallet_rm_flags_parse() {
        let cli = Cli::parse_from(["groundhog", "wallet", "rm", "alice"]);

        match cli.command {
            Command::Wallet {
                command: WalletCommand::Rm { name, profile },
            } => {
                assert_eq!(name, "alice");
                assert_eq!(profile, None);
            }
            _ => panic!("expected wallet rm command"),
        }
    }

    #[test]
    fn airdrop_flags_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "airdrop",
            "alice",
            "--amount",
            "1.5",
            "--dry-run",
        ]);

        match cli.command {
            Command::Airdrop {
                recipient,
                amount,
                dry_run,
                profile,
                ..
            } => {
                assert_eq!(recipient, "alice");
                assert_eq!(amount.as_deref(), Some("1.5"));
                assert!(dry_run);
                assert_eq!(profile, None);
            }
            _ => panic!("expected airdrop command"),
        }
    }

    #[test]
    fn transfer_flags_and_command_name_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "transfer",
            "alice",
            "1.5",
            "--dry-run",
            "--from",
            "trading",
            "--profile",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--runtime-manifest",
            "/tmp/runtime.json",
        ]);

        assert_eq!(cli.command.command_name(), "transfer");
        match cli.command {
            Command::Transfer {
                recipient,
                amount,
                dry_run,
                from,
                profile,
                target_args,
            } => {
                assert_eq!(recipient, "alice");
                assert_eq!(amount, "1.5");
                assert!(dry_run);
                assert_eq!(from.as_deref(), Some("trading"));
                assert_eq!(profile.as_deref(), Some("devnet"));
                assert_eq!(target_args.rpc_url.as_deref(), Some("https://rpc.example"));
                assert_eq!(
                    target_args.runtime_manifest.as_deref(),
                    Some(std::path::Path::new("/tmp/runtime.json"))
                );
            }
            _ => panic!("expected transfer command"),
        }
    }

    #[test]
    fn tx_show_flags_and_command_name_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "tx",
            "show",
            &format!("0x{}", "ab".repeat(32)),
            "--profile",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--runtime-manifest",
            "/tmp/runtime.json",
        ]);

        assert_eq!(cli.command.command_name(), "tx.show");
        match cli.command {
            Command::Tx {
                command:
                    TxCommand::Show {
                        tx_hash,
                        profile,
                        target_args,
                    },
            } => {
                assert_eq!(tx_hash, format!("0x{}", "ab".repeat(32)));
                assert_eq!(profile.as_deref(), Some("devnet"));
                assert_eq!(target_args.rpc_url.as_deref(), Some("https://rpc.example"));
                assert_eq!(
                    target_args.runtime_manifest.as_deref(),
                    Some(std::path::Path::new("/tmp/runtime.json"))
                );
            }
            _ => panic!("expected tx show command"),
        }
    }

    #[test]
    fn tx_list_flags_and_command_name_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "tx",
            "list",
            "alice",
            "--limit",
            "25",
            "--cursor",
            "0xaabb",
            "--profile",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--runtime-manifest",
            "/tmp/runtime.json",
        ]);

        assert_eq!(cli.command.command_name(), "tx.list");
        match cli.command {
            Command::Tx {
                command:
                    TxCommand::List {
                        target,
                        limit,
                        cursor,
                        profile,
                        target_args,
                    },
            } => {
                assert_eq!(target, "alice");
                assert_eq!(limit.as_deref(), Some("25"));
                assert_eq!(cursor.as_deref(), Some("0xaabb"));
                assert_eq!(profile.as_deref(), Some("devnet"));
                assert_eq!(target_args.rpc_url.as_deref(), Some("https://rpc.example"));
                assert_eq!(
                    target_args.runtime_manifest.as_deref(),
                    Some(std::path::Path::new("/tmp/runtime.json"))
                );
            }
            _ => panic!("expected tx list command"),
        }
    }

    #[test]
    fn events_list_flags_and_command_name_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "events",
            "list",
            "counter",
            "--topic0",
            "0x11",
            "--topic0",
            "0x22",
            "--topic1",
            "0x33",
            "--topic2",
            "0x44",
            "--limit",
            "25",
            "--cursor",
            "0xaabb",
            "--spec",
            "./counter.spec.xdr",
            "--event",
            "PingEvent",
            "--profile",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--runtime-manifest",
            "/tmp/runtime.json",
        ]);

        assert_eq!(cli.command.command_name(), "events.list");
        match cli.command {
            Command::Events {
                command:
                    EventsCommand::List {
                        contract,
                        topic0,
                        topic1,
                        topic2,
                        limit,
                        cursor,
                        spec,
                        event,
                        profile,
                        target_args,
                    },
            } => {
                assert_eq!(contract, "counter");
                assert_eq!(topic0, ["0x11", "0x22"]);
                assert_eq!(topic1, ["0x33"]);
                assert_eq!(topic2, ["0x44"]);
                assert_eq!(limit.as_deref(), Some("25"));
                assert_eq!(cursor.as_deref(), Some("0xaabb"));
                assert_eq!(
                    spec.as_deref(),
                    Some(std::path::Path::new("./counter.spec.xdr"))
                );
                assert_eq!(event.as_deref(), Some("PingEvent"));
                assert_eq!(profile.as_deref(), Some("devnet"));
                assert_eq!(target_args.rpc_url.as_deref(), Some("https://rpc.example"));
                assert_eq!(
                    target_args.runtime_manifest.as_deref(),
                    Some(std::path::Path::new("/tmp/runtime.json"))
                );
            }
            _ => panic!("expected events list command"),
        }
    }

    #[test]
    fn token_transfers_flags_and_command_name_parse() {
        let cli = Cli::parse_from([
            "groundhog",
            "token",
            "transfers",
            "alula",
            "--from",
            "alice",
            "--to",
            "bob",
            "--limit",
            "25",
            "--cursor",
            "0xaabb",
            "--profile",
            "local",
            "--rpc-url",
            "https://rpc.example",
            "--runtime-manifest",
            "/tmp/runtime.json",
        ]);

        assert_eq!(cli.command.command_name(), "token.transfers");
        match cli.command {
            Command::Token {
                command:
                    TokenCommand::Transfers {
                        token,
                        from,
                        to,
                        account,
                        limit,
                        cursor,
                        profile,
                        target_args,
                    },
            } => {
                assert_eq!(token, "alula");
                assert_eq!(from.as_deref(), Some("alice"));
                assert_eq!(to.as_deref(), Some("bob"));
                assert!(account.is_none());
                assert_eq!(limit.as_deref(), Some("25"));
                assert_eq!(cursor.as_deref(), Some("0xaabb"));
                assert_eq!(profile.as_deref(), Some("local"));
                assert_eq!(target_args.rpc_url.as_deref(), Some("https://rpc.example"));
                assert_eq!(
                    target_args.runtime_manifest.as_deref(),
                    Some(std::path::Path::new("/tmp/runtime.json"))
                );
            }
            _ => panic!("expected token transfers command"),
        }
    }

    #[test]
    fn test_integration_verbose_flag_parses() {
        let cli = Cli::parse_from(["groundhog", "test", "--integration", "-v"]);

        match cli.command {
            Command::Test {
                integration,
                verbose,
                ..
            } => {
                assert!(integration);
                assert!(verbose);
            }
            _ => panic!("expected test command"),
        }
    }
}
