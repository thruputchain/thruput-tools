use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use groundhog_cli::Print;
use groundhog_cli::result::CommandResult;
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "groundhog",
    about = "Groundhog smart contract development tools"
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
    /// Manage persisted Groundhog configuration
    #[command(
        long_about = "Inspect and manage persisted global and project profiles. Inspection \
                      commands show the merged configuration. Mutations update one physical \
                      layer: inside a project, pass --global or --project; outside a project, \
                      omitted scope means global. Use --json for stable machine-readable output."
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Build contract artifact
    #[command(long_about = "Cross-compile the contract to \
                      riscv32im-unknown-none-elf via cargo, then lower the ELF \
                      to a Groundhog RV32 artifact. Must be run inside a \
                      Groundhog project.")]
    Build {
        /// Package to build (required in workspaces)
        #[arg(short, long)]
        package: Option<String>,
        /// Generate typed bindings after build (e.g. `ts`)
        #[arg(long)]
        bindings: Vec<String>,
        /// Guest stack size (decimal, 0x hex, or a K/M/G suffix such as 32K;
        /// multiple of 16, below 64K). RV32 only; sets the SDK linker
        /// script's `__stack_size`.
        #[arg(long, value_name = "SIZE", value_parser = groundhog_cli::build::parse_stack_size)]
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
    /// Build and test the contract under RV32 or local-node execution
    #[command(
        long_about = "Builds the contract to RV32, then runs test_rv32 on the host. With \
                       --integration, starts an ephemeral hog-devnode \
                       local node, deploys the RV32 artifact, and \
                       runs the integration test target with RPC environment \
                       variables. With --json, emits a structured report \
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
        /// Package to deploy (required at a workspace root)
        #[arg(short, long, conflicts_with = "artifact")]
        package: Option<String>,
        /// Path to compiled .rv32a (default: auto-detect)
        #[arg(long)]
        artifact: Option<PathBuf>,
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
                      contract method. Named methods use local or built \
                      contract-spec metadata when available. Without metadata, \
                      pass an explicit 0x-prefixed 8-byte entrypoint ID \
                      and one raw hex calldata argument.")]
    Call {
        /// Contract alias or direct 0x-prefixed address
        contract: String,
        /// Method name or 0x-prefixed 8-byte entrypoint ID
        method: String,
        /// Method arguments. With an explicit entrypoint, pass one raw hex calldata argument.
        args: Vec<String>,
        /// Package whose contract metadata resolves the method (required in workspaces)
        #[arg(short, long)]
        package: Option<String>,
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
        /// Wallet name, dev-N wallet, or direct address (default: effective wallet)
        recipient: Option<String>,
        /// Amount in HOG, with up to 7 decimal places
        #[arg(long)]
        amount: Option<String>,
        /// Build and sign the airdrop transaction without submitting it
        #[arg(long)]
        dry_run: bool,
        /// Confirm a low local faucet balance without prompting
        #[arg(long)]
        yes: bool,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// Read native HOG balance for an address, wallet, contract, or dev account
    Balance {
        /// Wallet name, contract alias, dev-N account, or address (default: effective wallet)
        target: Option<String>,
        /// Config profile to use
        #[arg(long = "profile")]
        profile: Option<String>,
        #[command(flatten)]
        target_args: RpcTargetArgs,
    },
    /// Execute a sandboxed contract simulation against committed state
    #[command(long_about = "Execute a sandboxed contract simulation via \
                      groundhog_view. Named methods use local or built \
                      contract-spec metadata when available. Without metadata, \
                      pass an explicit 0x-prefixed 8-byte entrypoint ID \
                      and raw calldata.")]
    View {
        /// Contract alias or direct 0x-prefixed address
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
        /// Package whose contract metadata resolves the method (required in workspaces)
        #[arg(short, long)]
        package: Option<String>,
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
        /// Contract alias or direct 0x-prefixed address
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
        after_help = "Network exposure:\n  The public JSON-RPC listener binds to 127.0.0.1 by default. --host changes only\n  this public facade; hog-devnode's internal services stay on loopback. Binding to\n  0.0.0.0 or another non-loopback address exposes an unauthenticated development RPC\n  endpoint. Use it only on a trusted network or behind an appropriate firewall.\n\nProtocol override:\n  --hogd-protocol hogp/<hash> overrides the x23 intro token used for local\n  hog-devnode handshakes. This is useful when a local hog-devnode build is ahead of\n  the published Rust protocol crates; generated XDR clients still fail if the\n  wire shape is actually incompatible.\n\nLogging:\n  By default, groundhog node prints its own lifecycle/status output and keeps forwarded\n  hog-devnode info logs quiet. Use --verbose for the normal debugging view, or RUST_LOG\n  for exact tracing filters. --verbose is also honored with --json; logs stay on stderr.\n\nEnvironment:\n  GROUNDHOG_DEVNODE_BINARY  Default hog-devnode binary path when --hog-devnode-binary is omitted\n  RUST_LOG                  Overrides --verbose and controls supervisor/forwarded logs\n\nChild log bridge:\n  hog-devnode readiness is read from stdout. hog-devnode logs are requested as JSON on stderr,\n  parsed by node-supervisor, and re-emitted through tracing with hogd_timestamp_ms,\n  hogd_target, hogd_file, hogd_line, and hogd_fields metadata."
    )]
    Node {
        /// Host/interface for the public JSON-RPC listener
        #[arg(long, default_value = "127.0.0.1", value_name = "ADDRESS")]
        host: std::net::IpAddr,
        /// Port to listen on
        #[arg(long, default_value = "9545")]
        port: u16,
        /// Show forwarded hog-devnode info logs in addition to supervisor status
        #[arg(long, short = 'v')]
        verbose: bool,
        /// Number of dev-1+ user accounts to bootstrap; dev-0 faucet is always present
        #[arg(long)]
        dev_account_count: Option<usize>,
        /// Initial HOG allocated to each dev-1+ user account (default: 100; up to 7 decimals)
        #[arg(long, value_name = "HOG", value_parser = groundhog_cli::node::parse_dev_account_hog)]
        dev_account_hog: Option<u64>,
        /// Explicit hog-devnode binary path
        #[arg(long = "hog-devnode-binary", value_name = "GROUNDHOG_DEVNODE_BINARY")]
        hogd_dev_binary: Option<std::path::PathBuf>,
        /// Contract artifact directory passed to hog-devnode; may be repeated
        #[arg(long = "contract-artifacts-dir", value_name = "DIR")]
        contract_artifact_directories: Vec<PathBuf>,
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
        /// RAM database capacity for hog-devnode (decimal, 0x hex, or a K/M/G
        /// suffix such as 2G); omit for the devnode default
        #[arg(long, value_name = "SIZE", value_parser = groundhog_cli::byte_size::parse_byte_size)]
        ramdb_bytes: Option<u64>,
    },
    /// Dump or restore persistent local-chain state
    Chain {
        #[command(subcommand)]
        command: ChainCommand,
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
    /// Export a private key for client use
    #[command(
        long_about = "Export a CLI-managed Ed25519 private key for use with a client signer.\n\nThe 32-byte private key is written to stdout as lowercase, 0x-prefixed hex. Treat it as a secret: do not expose it in logs, CI output, or committed files."
    )]
    Export {
        /// Key name to export
        name: String,
    },
    /// Set the default signing key
    Use {
        /// Key name to use as default
        name: String,
    },
    /// Remove a stored key
    Rm {
        /// Key name to remove
        name: String,
        /// Remove the key without prompting; this cannot be undone
        #[arg(short = 'f', long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Show one persisted profile
    #[command(
        long_about = "Show one persisted profile merged from global and project configuration. \
                      This excludes local-node state, signer fallback, and command-specific \
                      overrides."
    )]
    Get {
        /// Profile name; omit to use active-profile selection
        profile: Option<String>,
        /// Include the global or project source of each value
        #[arg(long)]
        sources: bool,
    },
    /// List global and project profiles
    Profiles,
    /// Create a complete profile in one config layer
    #[command(long_about = "Create a complete profile in one atomic config update. \
                      Outside a project, omitted scope means global. When a project config is \
                      discoverable, choose --global or --project explicitly. The two scope flags \
                      cannot be used together.")]
    New {
        /// Profile to create
        profile: String,
        /// Network: local, devnet, or mainnet
        #[arg(long)]
        network: String,
        /// Absolute HTTP(S) RPC URL
        #[arg(long)]
        rpc_url: Option<String>,
        /// Network hash the RPC endpoint serves, as 64 lowercase hex digits
        #[arg(long)]
        network_hash: Option<String>,
        /// Absolute HTTP(S) faucet URL
        #[arg(long)]
        faucet_url: Option<String>,
        /// Profile-specific signing key
        #[arg(long)]
        default_key: Option<String>,
        /// Also select the new profile as this layer's default
        #[arg(long = "use")]
        select: bool,
        /// Create the profile in global config
        #[arg(long)]
        global: bool,
        /// Create the profile in project config
        #[arg(long)]
        project: bool,
    },
    /// Remove an entire profile from one config layer
    #[command(
        long_about = "Remove an entire profile from one config layer, including its contract \
                      aliases and wallet bindings. Interactive use asks for confirmation; pass \
                      --force to confirm non-interactively. A matching default profile selection \
                      is cleared in the same atomic update."
    )]
    Rm {
        /// Profile to remove
        profile: String,
        /// Remove the profile without prompting; this cannot be undone
        #[arg(short = 'f', long)]
        force: bool,
        /// Remove the profile from global config
        #[arg(long)]
        global: bool,
        /// Remove the profile from project config
        #[arg(long)]
        project: bool,
    },
    /// Set one typed field in a profile layer
    #[command(
        long_about = "Set network, rpc-url, network-hash, faucet-url, or default-key on an existing \
                      profile in one config layer. The profile must already exist in that layer. \
                      Inside a project, pass --global or --project; outside a project, omitted \
                      scope means global."
    )]
    Set {
        /// Field to set: network, rpc-url, network-hash, faucet-url, or default-key
        field: String,
        /// New field value
        value: String,
        /// Profile to update
        #[arg(long)]
        profile: String,
        /// Update the global config layer
        #[arg(long)]
        global: bool,
        /// Update the project config layer
        #[arg(long)]
        project: bool,
    },
    /// Remove one typed field from a profile layer
    #[command(
        long_about = "Remove network, rpc-url, network-hash, faucet-url, or default-key from one config \
                      layer. Missing fields and missing target-layer profiles are no-ops. Removing \
                      a project override may reveal the inherited global value."
    )]
    Unset {
        /// Field to remove: network, rpc-url, network-hash, faucet-url, or default-key
        field: String,
        /// Profile to update
        #[arg(long)]
        profile: String,
        /// Update the global config layer
        #[arg(long)]
        global: bool,
        /// Update the project config layer
        #[arg(long)]
        project: bool,
    },
    /// Select the default profile in one config layer
    #[command(
        long_about = "Set the profile used when --profile and GROUNDHOG_PROFILE are omitted. \
                      A global selection requires a global profile. A project selection may use a \
                      profile from either layer. Explicit --profile and GROUNDHOG_PROFILE still \
                      take precedence."
    )]
    Use {
        /// Profile to select
        profile: String,
        /// Update the global config layer
        #[arg(long)]
        global: bool,
        /// Update the project config layer
        #[arg(long)]
        project: bool,
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
        /// Exact local wallet prefund in HOG; remote faucets control this amount
        #[arg(long)]
        prefund: Option<String>,
        /// Confirm a low local faucet balance without prompting
        #[arg(long)]
        yes: bool,
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
        /// Path to a .spec.xdr file
        source: std::path::PathBuf,
        /// Output file path (default: stdout)
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum ChainCommand {
    /// Dump a stopped persistent node into one compressed JSON archive
    Dump {
        /// Persistent directory passed to `groundhog node --data-dir`
        #[arg(long)]
        data_dir: PathBuf,
        /// JSON archive to create
        #[arg(long)]
        output: PathBuf,
        /// Replace an existing archive
        #[arg(long)]
        overwrite: bool,
    },
    /// Restore one compressed JSON archive into a stopped persistent node
    Restore {
        /// Persistent directory passed to `groundhog node --data-dir`
        #[arg(long)]
        data_dir: PathBuf,
        /// JSON archive to restore
        #[arg(long)]
        input: PathBuf,
        /// Replace populated persistent state
        #[arg(short = 'f', long)]
        force: bool,
    },
}

impl ChainCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Dump { .. } => "chain.dump",
            Self::Restore { .. } => "chain.restore",
        }
    }
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
        /// Wallet name, contract alias, dev-N account, or address (default: effective wallet)
        target: Option<String>,
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
        /// Trusted local .xdr sidecar used to decode matching events
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

impl ConfigCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Get { .. } => "config.get",
            Self::Profiles => "config.profiles",
            Self::New { .. } => "config.new",
            Self::Rm { .. } => "config.rm",
            Self::Set { .. } => "config.set",
            Self::Unset { .. } => "config.unset",
            Self::Use { .. } => "config.use",
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
            Self::Chain { command } => command.command_name(),
            Self::Config { command } => command.command_name(),
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
            Self::Export { .. } => "keys.export",
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

fn cli_command(bundle_identity: &groundhog_bundle_identity::BundleIdentity) -> clap::Command {
    Cli::command().version(bundle_identity.to_string())
}

fn parse_cli(bundle_identity: &groundhog_bundle_identity::BundleIdentity) -> Cli {
    let matches = match cli_command(bundle_identity).try_get_matches() {
        Ok(matches) => matches,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::DisplayVersion
                && let Some(reason) = bundle_identity.terminal_safe_mismatch_reason()
            {
                eprintln!("warning: bundle manifest mismatch: {reason}");
            }
            error.exit()
        }
    };
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

#[allow(clippy::too_many_lines)]
fn main() {
    let bundle_identity = groundhog_cli::bundle_identity();
    let cli = parse_cli(&bundle_identity);
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
        Command::Config { command } => std::env::current_dir()
            .map_err(groundhog_cli::CliError::Io)
            .and_then(|cwd| match command {
                ConfigCommand::Get { profile, sources } => groundhog_cli::config_cmd::get(
                    &cwd,
                    profile.as_deref(),
                    sources,
                    cmd_name,
                    &print,
                ),
                ConfigCommand::Profiles => {
                    groundhog_cli::config_cmd::profiles(&cwd, cmd_name, &print)
                }
                ConfigCommand::New {
                    profile,
                    network,
                    rpc_url,
                    network_hash,
                    faucet_url,
                    default_key,
                    select,
                    global,
                    project,
                } => groundhog_cli::config_cmd::new_profile(
                    &cwd,
                    &profile,
                    &groundhog_cli::config::NewProfileOptions {
                        network,
                        rpc_url,
                        network_hash,
                        faucet_url,
                        default_key,
                        select,
                    },
                    global,
                    project,
                    cmd_name,
                    &print,
                ),
                ConfigCommand::Rm {
                    profile,
                    force,
                    global,
                    project,
                } => groundhog_cli::config_cmd::remove_with_force(
                    &cwd, &profile, force, global, project, cmd_name, &print,
                ),
                ConfigCommand::Set {
                    field,
                    value,
                    profile,
                    global,
                    project,
                } => groundhog_cli::config_cmd::set(
                    &cwd, &profile, &field, &value, global, project, cmd_name, &print,
                ),
                ConfigCommand::Unset {
                    field,
                    profile,
                    global,
                    project,
                } => groundhog_cli::config_cmd::unset(
                    &cwd, &profile, &field, global, project, cmd_name, &print,
                ),
                ConfigCommand::Use {
                    profile,
                    global,
                    project,
                } => groundhog_cli::config_cmd::use_default(
                    &cwd, &profile, global, project, cmd_name, &print,
                ),
            }),
        Command::Build {
            package,
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
                    &bindings,
                    stack_size,
                    &cargo_args,
                    &artifacts,
                    profile.as_deref(),
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
            package,
            artifact,
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
                    package.as_deref(),
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
            package,
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
                    package.as_deref(),
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
            yes,
            profile,
            target_args,
        } => groundhog_cli::airdrop::run(
            recipient.as_deref(),
            amount.as_deref(),
            dry_run,
            yes,
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
                    target.as_deref(),
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
            package,
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
                    package.as_deref(),
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
            host,
            port,
            verbose,
            dev_account_count,
            dev_account_hog,
            hogd_dev_binary,
            contract_artifact_directories,
            data_dir,
            hogd_protocol,
            startup_timeout_secs,
            block_driver,
            block_interval_ms,
            rpc_max_connections,
            ramdb_bytes,
        } => {
            let mut options = groundhog_cli::node::LocalNodeOptions::default();
            options.host = Some(host);
            options.port = port;
            options.hogd_dev_binary = hogd_dev_binary;
            options.contract_artifact_directories = contract_artifact_directories;
            options.data_dir = data_dir;
            options.hogd_protocol = hogd_protocol;
            options.startup_timeout = startup_timeout_secs.map(std::time::Duration::from_secs);
            options.dev_account_count = dev_account_count;
            options.dev_account_tokens = dev_account_hog;
            options.block_driver = block_driver.map(Into::into);
            options.block_interval = block_interval_ms.map(std::time::Duration::from_millis);
            options.rpc_max_connections = rpc_max_connections;
            options.ramdb_bytes = ramdb_bytes;
            options.bundle_identity = Some(bundle_identity.clone());
            groundhog_cli::node::run_with_options(options, verbose, cmd_name, &print)
        }
        Command::Chain { command } => match command {
            ChainCommand::Dump {
                data_dir,
                output,
                overwrite,
            } => groundhog_cli::chain::dump(&data_dir, &output, overwrite, cmd_name, &print),
            ChainCommand::Restore {
                data_dir,
                input,
                force,
            } => groundhog_cli::chain::restore(&data_dir, &input, force, cmd_name, &print),
        },
        Command::Keys { command } => match command {
            KeysCommand::Generate { name, fund } => {
                groundhog_cli::keys::generate(&name, fund, cmd_name, &print)
            }
            KeysCommand::Ls => groundhog_cli::keys::list(cmd_name, &print),
            KeysCommand::Export { name } => groundhog_cli::keys::export(&name, cmd_name, &print),
            KeysCommand::Use { name } => groundhog_cli::keys::use_key(&name, cmd_name, &print),
            KeysCommand::Rm { name, force } => {
                groundhog_cli::keys::remove_with_force(&name, force, cmd_name, &print)
            }
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
                prefund,
                yes,
                profile,
                target_args,
            } => groundhog_cli::wallet::create(
                key.as_deref(),
                name.as_deref(),
                dry_run,
                prefund.as_deref(),
                yes,
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
                        target.as_deref(),
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
            clap_complete::generate(
                shell,
                &mut cli_command(&bundle_identity),
                "groundhog",
                &mut output,
            );

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
    fn config_get_flags_and_command_name_parse() {
        let cli = Cli::parse_from(["groundhog", "config", "get", "devnet", "--sources"]);

        assert_eq!(cli.command.command_name(), "config.get");
        match cli.command {
            Command::Config {
                command: ConfigCommand::Get { profile, sources },
            } => {
                assert_eq!(profile.as_deref(), Some("devnet"));
                assert!(sources);
            }
            _ => panic!("expected config get command"),
        }
    }

    #[test]
    fn config_profiles_command_name_parses() {
        let cli = Cli::parse_from(["groundhog", "config", "profiles"]);

        assert_eq!(cli.command.command_name(), "config.profiles");
        assert!(matches!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Profiles
            }
        ));
    }

    #[test]
    fn config_mutation_commands_and_flags_parse() {
        let new_profile = Cli::parse_from([
            "groundhog",
            "config",
            "new",
            "staging",
            "--network",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--faucet-url",
            "https://faucet.example",
            "--default-key",
            "alice",
            "--use",
            "--global",
        ]);
        assert_eq!(new_profile.command.command_name(), "config.new");
        assert!(matches!(
            new_profile.command,
            Command::Config {
                command: ConfigCommand::New {
                    profile,
                    network,
                    select: true,
                    global: true,
                    project: false,
                    ..
                }
            } if profile == "staging" && network == "devnet"
        ));

        let remove = Cli::parse_from([
            "groundhog",
            "config",
            "rm",
            "staging",
            "--project",
            "--force",
        ]);
        assert_eq!(remove.command.command_name(), "config.rm");
        assert!(matches!(
            remove.command,
            Command::Config {
                command: ConfigCommand::Rm {
                    profile,
                    force: true,
                    global: false,
                    project: true,
                }
            } if profile == "staging"
        ));

        let set = Cli::parse_from([
            "groundhog",
            "config",
            "set",
            "rpc-url",
            "https://rpc.example",
            "--profile",
            "devnet",
            "--project",
        ]);
        assert_eq!(set.command.command_name(), "config.set");
        assert!(matches!(
            set.command,
            Command::Config {
                command: ConfigCommand::Set {
                    profile,
                    project: true,
                    global: false,
                    ..
                }
            } if profile == "devnet"
        ));

        let unset = Cli::parse_from([
            "groundhog",
            "config",
            "unset",
            "faucet-url",
            "--profile",
            "devnet",
            "--global",
        ]);
        assert_eq!(unset.command.command_name(), "config.unset");

        let use_profile = Cli::parse_from(["groundhog", "config", "use", "devnet", "--global"]);
        assert_eq!(use_profile.command.command_name(), "config.use");
    }

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
    fn node_host_flag_parses() {
        let cli = Cli::parse_from(["groundhog", "node", "--host", "0.0.0.0"]);

        match cli.command {
            Command::Node { host, .. } => {
                assert_eq!(host, std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
            }
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_host_defaults_to_loopback() {
        let cli = Cli::parse_from(["groundhog", "node"]);

        match cli.command {
            Command::Node { host, .. } => {
                assert_eq!(host, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
            }
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_host_rejects_invalid_address() {
        let Err(error) = Cli::try_parse_from(["groundhog", "node", "--host", "not-an-address"])
        else {
            panic!("invalid host address should be rejected");
        };

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn node_dev_account_hog_flag_parses_exact_raw_units() {
        let cli = Cli::parse_from(["groundhog", "node", "--dev-account-hog", "25.0000001"]);

        match cli.command {
            Command::Node {
                dev_account_hog, ..
            } => assert_eq!(dev_account_hog, Some(250_000_001)),
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
    fn node_contract_artifact_directories_are_repeatable() {
        let cli = Cli::parse_from([
            "groundhog",
            "node",
            "--contract-artifacts-dir",
            "/tmp/contracts-a",
            "--contract-artifacts-dir",
            "/tmp/contracts-b",
        ]);

        match cli.command {
            Command::Node {
                contract_artifact_directories,
                ..
            } => assert_eq!(
                contract_artifact_directories,
                [
                    PathBuf::from("/tmp/contracts-a"),
                    PathBuf::from("/tmp/contracts-b"),
                ]
            ),
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
                assert_eq!(recipient.as_deref(), Some("alice"));
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
                assert_eq!(target.as_deref(), Some("alice"));
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
