use clap::{ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use serde_json::json;
use std::path::{Path, PathBuf};
use thruput_cli::result::CommandResult;
use thruput_cli::{CliError, Print};

#[derive(Parser)]
#[command(name = "thruput", about = "Thruput smart contract development tools")]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Parser)]
#[allow(clippy::struct_excessive_bools)]
#[command(next_help_heading = "Global options")]
struct GlobalArgs {
    /// Suppress status messages
    #[arg(long, short, global = true)]
    quiet: bool,
    /// Disable prompts and terminal UI while retaining ordinary output
    #[arg(long, global = true)]
    non_interactive: bool,
    /// Emit structured JSON output
    #[arg(long, global = true)]
    json: bool,
    /// Use plain human output without boxes, animation, or terminal controls
    #[arg(long, global = true)]
    no_ui: bool,
    /// Print each cargo command to stderr before running it
    #[arg(long, global = true)]
    show_cargo_commands: bool,
    /// Forbid managed toolchain downloads; use locally available binaries
    #[arg(long, global = true)]
    offline: bool,
}

/// Everything a subcommand needs beyond its own arguments.
///
/// Built once from the global flags and the parsed subcommand, then passed to
/// every `run` so the per-command signatures stay down to their own arguments.
struct Ctx {
    print: Print,
    /// Canonical command name for JSON envelopes, from [`Command::command_name`].
    cmd_name: &'static str,
    /// `--offline` or `THRUPUT_OFFLINE=1`.
    offline: bool,
    bundle_identity: thruput_bundle_identity::BundleIdentity,
}

/// Invocation directory the project-relative commands resolve against.
fn invocation_cwd() -> Result<PathBuf, CliError> {
    std::env::current_dir().map_err(CliError::Io)
}

#[derive(Parser, Debug, Clone)]
struct ArtifactArgs {
    /// Directory for cargo build artifacts
    #[arg(long)]
    target_dir: Option<PathBuf>,
}

impl ArtifactArgs {
    fn options(self, cwd: &Path) -> thruput_cli::artifacts::ArtifactOptions {
        thruput_cli::artifacts::ArtifactOptions::from_invocation_cwd(self.target_dir, cwd)
    }
}

#[derive(Parser, Debug, Clone, Default)]
struct RpcTargetArgs {
    /// Override the active profile's RPC URL
    #[arg(long)]
    rpc_url: Option<String>,
    /// Override local-dev runtime manifest path for local chain identity and faucet
    #[arg(long)]
    runtime_manifest: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, Copy, Default)]
struct ConfirmationArgs {
    /// Confirm without prompting; the protected action cannot be undone
    #[arg(short = 'f', long)]
    force: bool,
}

impl From<RpcTargetArgs> for thruput_cli::config::ProfileOverrides {
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

impl From<NodeBlockDriverArg> for thruput_cli::node::LocalNodeBlockDriverMode {
    fn from(value: NodeBlockDriverArg) -> Self {
        match value {
            NodeBlockDriverArg::Instant => Self::Instant,
            NodeBlockDriverArg::Interval => Self::Interval,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Create a new Thruput contract project
    New(NewArgs),
    /// Discover embedded project templates
    Templates {
        #[command(subcommand)]
        command: TemplatesCommand,
    },
    /// Serve the bundled developer documentation on localhost
    Docs(DocsArgs),
    /// Install, update, or read Thruput coding-agent guidance
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Set up profiles and a signing key, with optional interactive scaffolding
    #[command(long_about = "Initialize global profiles and check setup readiness. \
        Interactive sessions offer key generation and optional scaffolding.\n\n\
        Use --non-interactive to skip all questions and --generate-key to authorize \
        initial key creation. Existing keys and explicit selections are preserved. \
        Without a TTY, or with --quiet/--json, questions are skipped.\n\n\
        --json emits one setup result; use status --json for read-only diagnostics. \
        Exit 1 means setup is incomplete or failed. Scripts create projects with thruput new.")]
    Init(InitArgs),
    /// Show RV32, devnode, keys, and project status
    #[command(long_about = "Non-interactive diagnostic. Reports \
                      the RV32 toolchain, hog-devnode availability, keys, \
                      and project config. Use --json for \
                      scripting.\n\n\
                      Exit code 0 means everything looks good; \
                      exit code 1 means at least one warning-level \
                      finding exists.")]
    Status(StatusArgs),
    /// Manage persisted Thruput configuration
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
    /// Install and manage verified hog-devnode bundles
    Toolchain {
        #[command(subcommand)]
        command: ToolchainCommand,
    },
    /// Build contract artifact
    #[command(long_about = "Cross-compile the contract to \
                      riscv32im-unknown-none-elf via cargo, then lower the ELF \
                      to a Thruput RV32 artifact. Must be run inside a \
                      Thruput project.")]
    Build(BuildArgs),
    /// Build and test the contract under RV32 or local-node execution
    #[command(
        long_about = "Builds the contract to RV32, then runs test_rv32 on the host. With \
                       --integration, starts an ephemeral hog-devnode \
                       local node, deploys the RV32 artifact, and \
                       runs the integration test target with RPC environment \
                       variables. With --workspace, builds and tests every \
                       workspace member that declares a test_rv32 target and \
                       reports them together. With --json, emits a structured \
                       report with mode, artifact metadata, summary, per-test \
                       cases, and integration node metadata when applicable.",
        after_help = "Logging:\n  With --integration, local-node supervisor info logs \
                        are written to stderr;\n  add --verbose/-v to also forward \
                        hog-devnode info logs. RUST_LOG overrides\n  the default filter, \
                        for example \
                        RUST_LOG=thruput_node_supervisor=debug,hogd_dev=debug."
    )]
    Test(TestArgs),
    /// Deploy a contract
    Deploy(DeployArgs),
    /// Submit a transaction that calls a deployed contract method
    #[command(long_about = "Submit a signed transaction that calls a deployed \
                      contract method. Named methods use local or built \
                      contract-spec metadata when available. Without metadata, \
                      pass an explicit 0x-prefixed 8-byte entrypoint ID \
                      and one raw hex calldata argument.")]
    Call(CallArgs),
    /// Transfer native HOG from the effective profile wallet
    Transfer(TransferArgs),
    /// Request HOG funding for a wallet
    Airdrop(AirdropArgs),
    /// Read native HOG balance for an address, wallet, contract, or dev account
    Balance(BalanceArgs),
    /// Execute a sandboxed contract simulation against committed state
    #[command(long_about = "Execute a sandboxed contract simulation via \
                      simulateInvocation. Named methods use local or built \
                      contract-spec metadata when available. Without metadata, \
                      pass an explicit 0x-prefixed 8-byte entrypoint ID \
                      and raw calldata.")]
    View(ViewArgs),
    /// Read latest committed raw contract storage
    #[command(long_about = "Read one latest committed raw storage item via \
                      getStorage. This is a low-level state \
                      inspection command for address/key reads; use \
                      `thruput call` for signed transaction invocation \
                      and `thruput view` for sandboxed contract \
                      simulation.")]
    Storage(StorageArgs),
    /// Start a local development node
    #[command(
        long_about = "Start a local development node \
                      backed by a supervised hog-devnode process.",
        after_help = "Network exposure:\n  The public JSON-RPC listener binds to 127.0.0.1 by default. --host changes only\n  this public facade; hog-devnode's internal services stay on loopback. Binding to\n  0.0.0.0 or another non-loopback address exposes an unauthenticated development RPC\n  endpoint. Use it only on a trusted network or behind an appropriate firewall.\n\nProtocol override:\n  --hogd-protocol hogp/<hash> overrides the protocol token used for local\n  hog-devnode handshakes. This is useful when a local hog-devnode build is ahead of\n  the published Rust protocol crates; generated XDR clients still fail if the\n  wire shape is actually incompatible.\n\nLogging:\n  By default, thruput node prints its own lifecycle/status output and keeps forwarded\n  hog-devnode info logs quiet. Use --verbose for the normal debugging view, or RUST_LOG\n  for exact tracing filters. --verbose is also honored with --json; logs stay on stderr.\n\nEnvironment:\n  THRUPUT_DEVNODE_BINARY  Local binary override unless --hog-devnode-version is supplied\n  THRUPUT_OFFLINE=1  Forbids managed downloads\n  THRUPUT_TOOLCHAIN_BASE_URL  Overrides the public artifact source\n  RUST_LOG                  Overrides --verbose and controls supervisor/forwarded logs\n\nChild log bridge:\n  hog-devnode readiness is read from stdout. hog-devnode logs are requested as JSON on stderr,\n  parsed by node-supervisor, and re-emitted through tracing with hogd_timestamp_ms,\n  hogd_target, hogd_file, hogd_line, and hogd_fields metadata."
    )]
    Node(NodeArgs),
    /// Dump or restore persistent local-chain state
    Chain {
        #[command(subcommand)]
        command: ChainCommand,
    },
    /// Manage signing keys
    #[command(long_about = "Ed25519 or ML-DSA-65 signing keys stored in \
                       ~/.config/thruput/keys/. Keys are \
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
    Completion(CompletionArgs),
}

impl Command {
    /// Canonical command name for JSON envelopes.
    ///
    /// Used by both success and error JSON output so they
    /// always agree.
    fn command_name(&self) -> &'static str {
        match self {
            Self::New(..) => "new",
            Self::Templates {
                command: TemplatesCommand::List,
            } => "templates.list",
            Self::Templates {
                command: TemplatesCommand::Show { .. },
            } => "templates.show",
            Self::Init(..) => "init",
            Self::Status(..) => "status",
            Self::Build(..) => "build",
            Self::Test(..) => "test",
            Self::Deploy(..) => "deploy",
            Self::Call(..) => "call",
            Self::Transfer(..) => "transfer",
            Self::Airdrop(..) => "airdrop",
            Self::Balance(..) => "balance",
            Self::View(..) => "view",
            Self::Storage(..) => "storage",
            Self::Node(..) => "node",
            Self::Docs(args) => match args.command {
                Some(DocsCommand::Export { .. }) => "docs export",
                Some(DocsCommand::Status) => "docs status",
                Some(DocsCommand::Stop) => "docs stop",
                None => "docs",
            },
            Self::Skill { command } => command.command_name(),
            Self::Chain { command } => command.command_name(),
            Self::Config { command } => command.command_name(),
            Self::Toolchain { command } => command.command_name(),
            Self::Keys { command } => command.command_name(),
            Self::Wallet { command } => command.command_name(),
            Self::Codegen { command } => command.command_name(),
            Self::Tx { command } => command.command_name(),
            Self::Events { command } => command.command_name(),
            Self::Token { command } => command.command_name(),
            Self::Completion(..) => "completion",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        match self {
            Self::New(args) => args.run(ctx),
            Self::Templates {
                command: TemplatesCommand::List,
            } => thruput_cli::templates::list(ctx.cmd_name, &ctx.print),
            Self::Templates {
                command: TemplatesCommand::Show { id },
            } => thruput_cli::templates::show(&id, ctx.cmd_name, &ctx.print),
            Self::Docs(args) => args.run(ctx),
            Self::Skill { command } => command.run(ctx),
            Self::Init(args) => args.run(ctx),
            Self::Status(args) => args.run(ctx),
            Self::Config { command } => command.run(ctx),
            Self::Toolchain { command } => command.run(ctx),
            Self::Build(args) => args.run(ctx),
            Self::Test(args) => args.run(ctx),
            Self::Deploy(args) => args.run(ctx),
            Self::Call(args) => args.run(ctx),
            Self::Transfer(args) => args.run(ctx),
            Self::Airdrop(args) => args.run(ctx),
            Self::Balance(args) => args.run(ctx),
            Self::View(args) => args.run(ctx),
            Self::Storage(args) => args.run(ctx),
            Self::Node(args) => args.run(ctx),
            Self::Chain { command } => command.run(ctx),
            Self::Keys { command } => command.run(ctx),
            Self::Wallet { command } => command.run(ctx),
            Self::Codegen { command } => command.run(ctx),
            Self::Tx { command } => command.run(ctx),
            Self::Events { command } => command.run(ctx),
            Self::Token { command } => command.run(ctx),
            Self::Completion(args) => args.run(ctx),
        }
    }
}

#[derive(Args)]
struct NewArgs {
    /// Project name: lowercase letters, digits, and single hyphens, starting with a letter
    name: Option<String>,
    /// Project directory (relative, absolute, or ~/path); parent must exist, destination must be empty
    #[arg(long)]
    path: Option<PathBuf>,
    /// Named template from `templates list`; omit for a minimal contract
    #[arg(long)]
    template: Option<String>,
}

impl NewArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::new::run(
            self.name,
            self.path,
            self.template,
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Subcommand)]
enum TemplatesCommand {
    /// List available project templates
    List,
    /// Show contents, prerequisites, and next steps
    Show { id: String },
}

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
struct DocsArgs {
    #[command(subcommand)]
    command: Option<DocsCommand>,
    /// Start a background server and return once it is ready
    #[arg(long, conflicts_with = "foreground")]
    background: bool,
    /// Serve in the foreground (the default)
    #[arg(long)]
    foreground: bool,
    #[arg(long, hide = true, conflicts_with_all = ["background", "foreground", "dir", "port"])]
    managed: bool,
    /// Override the bundled docs with a trusted local snapshot directory
    #[arg(long, value_name = "DIRECTORY")]
    dir: Option<PathBuf>,
    /// Loopback port (0 chooses an available port)
    #[arg(long, default_value_t = 4321)]
    port: u16,
}

#[derive(Subcommand)]
enum DocsCommand {
    /// Export a portable Markdown set into a new directory without running a server
    Export {
        #[arg(long)]
        output: PathBuf,
        /// Use a trusted local snapshot instead of the bundled snapshot
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Inspect the current user's background docs server
    Status,
    /// Stop the current user's background docs server
    Stop,
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Install the latest maintained skill through npx skills (requires Node/npm)
    Install {
        #[arg(long)]
        global: bool,
        #[arg(long, action = ArgAction::Append)]
        agent: Vec<String>,
        /// Authorize npm package execution and skill installation without prompting
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Update only the installed Thruput skill in the selected scope
    Update {
        #[arg(long)]
        global: bool,
        /// Authorize npm package execution and updates without prompting
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Print an installed skill reference without network access
    Show {
        topic: Option<String>,
        /// Read only the user-wide installation
        #[arg(long)]
        global: bool,
    },
    /// List references in the installed skill
    List {
        /// Read only the user-wide installation
        #[arg(long)]
        global: bool,
    },
}

impl SkillCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Install { .. } => "skill install",
            Self::Update { .. } => "skill update",
            Self::Show { .. } => "skill show",
            Self::List { .. } => "skill list",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        use thruput_cli::skill::{self, InstallOptions};
        match self {
            Self::Show { topic, global } => {
                skill::show(topic.as_deref(), global, false, ctx.cmd_name, &ctx.print)
            }
            Self::List { global } => skill::show(None, global, true, ctx.cmd_name, &ctx.print),
            Self::Install { global, agent, yes } => skill::install(
                false,
                &InstallOptions {
                    global,
                    agents: agent,
                    yes,
                    offline: ctx.offline,
                },
                ctx.cmd_name,
                &ctx.print,
            ),
            Self::Update { global, yes } => skill::install(
                true,
                &InstallOptions {
                    global,
                    yes,
                    offline: ctx.offline,
                    ..Default::default()
                },
                ctx.cmd_name,
                &ctx.print,
            ),
        }
    }
}

impl DocsArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        use thruput_cli::docs::{
            self,
            service::{self, Action},
        };
        if let Some(command) = self.command {
            return match command {
                DocsCommand::Export { output, dir } => {
                    docs::export(dir.as_deref(), &output, ctx.cmd_name, &ctx.print)
                }
                DocsCommand::Status => {
                    service::run(Action::Status, None, self.port, ctx.cmd_name, &ctx.print)
                }
                DocsCommand::Stop => {
                    service::run(Action::Stop, None, self.port, ctx.cmd_name, &ctx.print)
                }
            };
        }
        if self.managed {
            return service::managed(&ctx.print);
        }
        if self.background {
            return service::run(
                Action::Start,
                self.dir.as_deref(),
                self.port,
                ctx.cmd_name,
                &ctx.print,
            );
        }
        thruput_cli::docs::run(self.dir.as_deref(), self.port, ctx.cmd_name, &ctx.print)
    }
}

#[derive(Args)]
struct InitArgs {
    /// Create the initial key only when no keys or explicit key selections exist
    #[arg(long)]
    generate_key: bool,
}

impl InitArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::init::run(
            &invocation_cwd()?,
            &thruput_cli::init::InitOptions {
                generate_key: self.generate_key,
            },
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct StatusArgs {
    /// Config profile to use
    #[arg(long = "profile")]
    profile: Option<String>,
}

impl StatusArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::status::run(
            &invocation_cwd()?,
            self.profile.as_deref(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct BuildArgs {
    /// Show Cargo logs live instead of collecting diagnostics below the result
    #[arg(short, long)]
    verbose: bool,
    /// Package to build (required in workspaces)
    #[arg(short, long)]
    package: Option<String>,
    /// Generate typed bindings after build (e.g. `ts`)
    #[arg(long)]
    bindings: Vec<String>,
    /// Guest stack size (decimal, 0x hex, or a K/M/G suffix such as 32K;
    /// multiple of 16, below the guest data memory). RV32 only; sets the
    /// SDK linker script's `__stack_size`. The data memory itself is the
    /// package's `[package.metadata.thruput] initial_memory_bytes`.
    #[arg(long, value_name = "SIZE", value_parser = thruput_cli::build::parse_stack_size)]
    stack_size: Option<u32>,
    /// Config profile to use
    #[arg(long = "profile")]
    profile: Option<String>,
    #[command(flatten)]
    artifacts: ArtifactArgs,
    /// Extra arguments passed to cargo
    #[arg(last = true)]
    cargo_args: Vec<String>,
}

impl BuildArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        thruput_cli::build::run(
            self.package.as_deref(),
            &self.bindings,
            self.stack_size,
            &self.cargo_args,
            &self.artifacts.options(&cwd),
            self.profile.as_deref(),
            &ctx.print.clone().with_verbose_build(self.verbose),
        )
    }
}

#[derive(Args)]
#[allow(clippy::struct_excessive_bools)]
struct TestArgs {
    /// Package to test (required in workspaces unless --workspace is set)
    #[arg(short, long, conflicts_with = "workspace")]
    package: Option<String>,
    /// Test every workspace member that declares a `test_rv32` target
    ///
    /// Members run one at a time in name order. A failing package does
    /// not stop the rest unless --fail-fast is set. Not yet supported
    /// with --integration.
    #[arg(long)]
    workspace: bool,
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
    /// Override the hog-devnode handshake protocol token for the ephemeral integration node
    ///
    /// (Only used with --integration)
    /// Falls back to the `THRUPUT_HOGD_PROTOCOL` env var.
    #[arg(long = "hogd-protocol", requires = "integration")]
    hogd_protocol: Option<String>,
    /// Use one exact managed hog-devnode version for the integration node
    #[arg(long, requires = "integration")]
    hog_devnode_version: Option<String>,
    /// Override a genesis network parameter on the integration node (repeatable;
    /// decimal, hex, or K/M/G values). Mirrors `thruput node --network-config`,
    /// and replaces the same parameter from the profile's `network_config` table.
    #[arg(long = "network-config", value_name = "NAME=VALUE", requires = "integration",
          value_parser = thruput_cli::node::parse_network_config_override)]
    network_config: Vec<(thruput_node_supervisor::ConfigParamName, u32)>,
    /// Pass arguments to the integration hog-devnode (repeatable). Each value
    /// is split with shell quoting rules: `--node-arg "--ramdb-bytes 4294967296"`.
    /// Appended last, so they override flags thruput sets.
    #[arg(
        long = "node-arg",
        value_name = "ARGS",
        requires = "integration",
        allow_hyphen_values = true,
        value_parser = parse_node_arg
    )]
    node_arg: Vec<NodeArgWords>,
    /// Config profile to use
    #[arg(long = "profile")]
    profile: Option<String>,
    #[command(flatten)]
    artifacts: ArtifactArgs,
    /// Extra arguments forwarded to the test harness (after --)
    #[arg(last = true)]
    cargo_args: Vec<String>,
}

/// One `--node-arg` value after shell-style splitting.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeArgWords(Vec<String>);

fn parse_node_arg(value: &str) -> Result<NodeArgWords, String> {
    thruput_cli::test_cmd::parse_node_arg(value).map(NodeArgWords)
}

impl TestArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        let artifacts = self.artifacts.options(&cwd);
        let options = thruput_cli::test_cmd::TestOptions {
            match_test: self.match_test,
            test_threads: self.test_threads,
            skip: self.skip,
            rerun: self.rerun,
            watch: self.watch,
            workspace: self.workspace,
            harness: thruput_cli::test_cmd::TestHarnessOptions {
                fail_fast: self.fail_fast,
                show_output: self.show_output,
                no_capture: self.no_capture,
            },
        };
        if self.integration {
            thruput_cli::test_cmd::run_integration_with_options(
                self.package.as_deref(),
                &options,
                &thruput_cli::test_cmd::IntegrationTestOptions {
                    hogd_protocol: self.hogd_protocol,
                    hog_devnode_version: self.hog_devnode_version,
                    network_config_overrides: self.network_config,
                    node_args: self
                        .node_arg
                        .into_iter()
                        .flat_map(|words| words.0)
                        .collect(),
                    offline: ctx.offline,
                    verbose: self.verbose,
                },
                &self.cargo_args,
                &artifacts,
                self.profile.as_deref(),
                ctx.cmd_name,
                &ctx.print,
            )
        } else {
            thruput_cli::test_cmd::run_with_options(
                self.package.as_deref(),
                &options,
                &self.cargo_args,
                &artifacts,
                self.profile.as_deref(),
                ctx.cmd_name,
                &ctx.print,
            )
        }
    }
}

#[derive(Args)]
struct DeployArgs {
    /// Inspect deployment storage funding without signing or submitting
    #[arg(long, conflicts_with_all = ["dry_run", "fund"])]
    estimate: bool,
    /// Exact HOG or "minimum" for deployment collateral; zero skips funding
    #[arg(long, value_name = "HOG|minimum")]
    fund: Option<String>,
    /// Initialize the new contract atomically using a method name or 0x entrypoint
    #[arg(long, value_name = "METHOD")]
    init: Option<String>,
    /// Initializer argument; repeat for each argument (explicit entrypoints take raw hex)
    #[arg(
        long,
        value_name = "VALUE",
        requires = "init",
        allow_hyphen_values = true
    )]
    init_arg: Vec<String>,
    /// Gas budget for the initializer (default: 25000000)
    #[arg(long, requires = "init", value_parser = clap::value_parser!(u64).range(1..))]
    init_gas_limit: Option<u64>,
    /// Package to deploy (required at a workspace root)
    #[arg(short, long, conflicts_with = "artifact")]
    package: Option<String>,
    /// Path to compiled .rv32a (default: auto-detect)
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Name to record the deployed address under in thruput.toml
    /// (defaults to the package name; with --artifact there is no package,
    /// so without this no alias is recorded)
    #[arg(long, value_parser = thruput_cli::config::parse_contract_alias)]
    alias: Option<String>,
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
    #[command(flatten)]
    confirmation: ConfirmationArgs,
}

impl DeployArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        thruput_cli::deploy::run(
            self.artifact.as_deref(),
            &thruput_cli::deploy::DeploymentOptions {
                estimate: self.estimate,
                fund: self.fund.as_deref(),
                init_method: self.init.as_deref(),
                init_args: &self.init_arg,
                init_gas_limit: self.init_gas_limit,
            },
            self.package.as_deref(),
            self.alias.as_deref(),
            self.dry_run,
            self.confirmation.force,
            self.wallet.as_deref(),
            &self.artifacts.options(&cwd),
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct CallArgs {
    /// Contract alias, C StrKey, or direct base58 address
    #[allow(clippy::doc_markdown)]
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
    /// Gas budget for the transaction and its inner contract invocation
    #[arg(long, default_value_t = thruput_cli::call::DEFAULT_CALL_GAS_LIMIT,
          value_parser = clap::value_parser!(u64).range(1..))]
    gas_limit: u64,
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
    #[command(flatten)]
    confirmation: ConfirmationArgs,
}

impl CallArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        let run = thruput_cli::call::run;
        run(
            &self.contract,
            &self.method,
            &self.args,
            self.package.as_deref(),
            self.dry_run,
            self.gas_limit,
            self.confirmation.force,
            self.wallet.as_deref(),
            &self.artifacts.options(&cwd),
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct TransferArgs {
    /// Wallet name, contract alias, stored key, or direct address
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
    #[command(flatten)]
    confirmation: ConfirmationArgs,
}

impl TransferArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::transfer::run(
            &self.recipient,
            &self.amount,
            self.from.as_deref(),
            self.dry_run,
            self.confirmation.force,
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct AirdropArgs {
    /// Wallet name, key name, contract alias, or direct address (default: effective wallet)
    recipient: Option<String>,
    /// Amount in HOG, with up to 7 decimal places
    #[arg(long)]
    amount: Option<String>,
    /// Build and sign the airdrop transaction without submitting it
    #[arg(long)]
    dry_run: bool,
    #[command(flatten)]
    confirmation: ConfirmationArgs,
    /// Config profile to use
    #[arg(long = "profile")]
    profile: Option<String>,
    #[command(flatten)]
    target_args: RpcTargetArgs,
}

impl AirdropArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::airdrop::run(
            self.recipient.as_deref(),
            self.amount.as_deref(),
            self.dry_run,
            self.confirmation.force,
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct BalanceArgs {
    /// Wallet name, contract alias, stored key, or address (default: effective wallet)
    target: Option<String>,
    /// Config profile to use
    #[arg(long = "profile")]
    profile: Option<String>,
    #[command(flatten)]
    target_args: RpcTargetArgs,
}

impl BalanceArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::balance::run(
            self.target.as_deref(),
            &invocation_cwd()?,
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct ViewArgs {
    /// Contract alias, C StrKey, or direct base58 address
    #[allow(clippy::doc_markdown)]
    contract: String,
    /// Method name or 0x-prefixed 8-byte entrypoint ID
    method: String,
    /// Method arguments. With an explicit entrypoint, pass one raw hex calldata argument.
    args: Vec<String>,
    /// Raw calldata for explicit-entrypoint calls
    #[arg(long)]
    calldata: Option<String>,
    /// Simulated caller: saved wallet, key, contract alias, stored key, or direct address
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
}

impl ViewArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        let run = thruput_cli::view::run;
        run(
            &self.contract,
            &self.method,
            &self.args,
            self.calldata.as_deref(),
            self.from.as_deref(),
            self.package.as_deref(),
            &self.artifacts.options(&cwd),
            &cwd,
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct StorageArgs {
    /// Contract alias, C StrKey, or direct base58 address
    #[allow(clippy::doc_markdown)]
    address: String,
    /// 32-byte storage key as 0x-prefixed hex
    key: String,
    /// Config profile to use
    #[arg(long = "profile")]
    profile: Option<String>,
    #[command(flatten)]
    target_args: RpcTargetArgs,
}

impl StorageArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        thruput_cli::storage::run(
            &self.address,
            &self.key,
            &invocation_cwd()?,
            self.profile.as_deref(),
            &self.target_args.into(),
            ctx.cmd_name,
            &ctx.print,
        )
    }
}

#[derive(Args)]
struct NodeArgs {
    /// Host/interface for the public JSON-RPC listener
    #[arg(long, default_value = "127.0.0.1", value_name = "ADDRESS")]
    host: std::net::IpAddr,
    /// Port to listen on
    #[arg(long, default_value = "9545")]
    port: u16,
    /// Show forwarded hog-devnode info logs in addition to supervisor status
    #[arg(long, short = 'v')]
    verbose: bool,
    /// Stored key to fund on a fresh chain; defaults to the selected project or global key
    #[arg(long)]
    key: Option<String>,
    /// Initial HOG for the selected key (default: 100; up to 7 decimals)
    #[arg(long, value_name = "HOG", value_parser = thruput_cli::node::parse_initial_account_hog)]
    initial_account_hog: Option<u64>,
    /// JSON array of public key allocations for fixture-owned accounts
    #[arg(long, value_name = "FILE", conflicts_with_all = ["key", "initial_account_hog"])]
    bootstrap_accounts: Option<PathBuf>,
    /// Explicit hog-devnode binary path
    #[arg(long = "hog-devnode-binary", value_name = "THRUPUT_DEVNODE_BINARY")]
    hogd_dev_binary: Option<std::path::PathBuf>,
    /// Contract artifact directory passed to hog-devnode; may be repeated
    #[arg(long = "contract-artifacts-dir", value_name = "DIR")]
    contract_artifact_directories: Vec<PathBuf>,
    /// Use one exact managed hog-devnode version for this invocation
    #[arg(long, conflicts_with = "hogd_dev_binary")]
    hog_devnode_version: Option<String>,
    /// Directory for persistent hog-devnode state; omit for an ephemeral node
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Override the hog-devnode handshake protocol token
    #[arg(long)]
    hogd_protocol: Option<String>,
    /// Initialize a new chain: base58 encoding of exactly 32 bytes
    /// (default: all zeros for new chains). Omit on restart; initialized directories reject it.
    #[arg(long, value_name = "BASE58", value_parser = thruput_cli::config::parse_network_hash)]
    init_network_hash: Option<thruput_json_rpc_types::NetworkHashText>,
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

    /// Concurrent entries per JSON-RPC array (1..=1024, default 128)
    #[arg(long)]
    rpc_batch_concurrency: Option<thruput_node_supervisor::RpcBatchConcurrency>,
    /// RAM database capacity for hog-devnode (decimal, 0x hex, or a K/M/G
    /// suffix such as 2G); omit for the devnode default
    #[arg(long, value_name = "SIZE", value_parser = thruput_cli::byte_size::parse_byte_size)]
    ramdb_bytes: Option<u64>,
    /// Override a genesis network parameter (repeatable; decimal, hex, or K/M/G values).
    /// Replaces the same parameter from the active profile's `network_config` table.
    /// Existing data directories must use the same overrides they were created with.
    #[arg(long = "network-config", value_name = "NAME=VALUE",
          value_parser = thruput_cli::node::parse_network_config_override)]
    network_config: Vec<(thruput_node_supervisor::ConfigParamName, u32)>,
}

impl NodeArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        // LocalNodeOptions is #[non_exhaustive], so it is filled field by field
        // rather than with a struct literal.
        let mut options = thruput_cli::node::LocalNodeOptions::default();
        options.host = Some(self.host);
        options.port = self.port;
        options.hogd_dev_binary = self.hogd_dev_binary;
        options.contract_artifact_directories = self.contract_artifact_directories;
        options.hogd_dev_version = self.hog_devnode_version;
        options.offline = ctx.offline;
        options.data_dir = self.data_dir;
        options.hogd_protocol = self.hogd_protocol;
        options.init_network_hash = self.init_network_hash;
        options.startup_timeout = self
            .startup_timeout_secs
            .map(std::time::Duration::from_secs);
        if let Some(path) = self.bootstrap_accounts {
            options.bootstrap_accounts = thruput_cli::node::load_bootstrap_allocations(&path)?;
        } else {
            (options.selected_key, options.bootstrap_accounts) =
                thruput_cli::node::stored_key_allocation(
                    self.key.as_deref(),
                    self.initial_account_hog
                        .unwrap_or(thruput_node_supervisor::config::DEFAULT_LOCAL_ACCOUNT_TOKENS),
                )?;
        }
        options.block_driver = self.block_driver.map(Into::into);
        options.block_interval = self.block_interval_ms.map(std::time::Duration::from_millis);
        options.rpc_max_connections = self.rpc_max_connections;
        options.rpc_batch_concurrency = self.rpc_batch_concurrency;
        options.ramdb_bytes = self.ramdb_bytes;
        options.network_config_overrides = thruput_cli::node::with_profile_network_config(
            &invocation_cwd()?,
            None,
            self.network_config,
        )?;
        options.bundle_identity = Some(ctx.bundle_identity.clone());
        thruput_cli::node::run_with_options(options, self.verbose, ctx.cmd_name, &ctx.print)
    }
}

#[derive(Args)]
struct CompletionArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    shell: Shell,
}

impl CompletionArgs {
    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let mut output = Vec::new();
        let shell_name = self.shell.to_string();
        clap_complete::generate(
            self.shell,
            &mut cli_command(&ctx.bundle_identity),
            "thruput",
            &mut output,
        );

        if ctx.print.is_json() {
            ctx.print.json(&CommandResult::new(
                ctx.cmd_name,
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
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Generate a new keypair
    Generate {
        /// Key name (default: "default")
        name: Option<String>,
        /// Signature scheme of the new key
        #[arg(long, value_enum, default_value_t = thruput_cli::keys::KeyScheme::Ed25519)]
        scheme: thruput_cli::keys::KeyScheme,
        /// Fund from testnet faucet (not yet implemented)
        #[arg(long, hide = true)]
        fund: bool,
    },
    /// Import a secret seed from hidden terminal input or piped stdin (S StrKey or base58)
    #[allow(clippy::doc_markdown)]
    Import {
        /// New key name; existing keys are never overwritten
        name: String,
    },
    /// List stored keys
    Ls,
    /// Export a private key for client use
    #[command(
        long_about = "Export a CLI-managed private key for use with a client signer.\n\nThe 32-byte private seed (Ed25519 seed or ML-DSA-65 FIPS 204 seed) is written to stdout as unprefixed, case-sensitive base58 by default; --format strkey exports an S StrKey for Ed25519 keys. Treat it as a secret: do not expose it in logs, CI output, or committed files."
    )]
    Export {
        /// Key name to export
        name: String,
        /// Private seed encoding
        #[arg(long, value_enum, default_value_t = thruput_cli::keys::KeyExportFormat::Base58)]
        format: thruput_cli::keys::KeyExportFormat,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
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
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
}

impl KeysCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Generate { .. } => "keys.generate",
            Self::Import { .. } => "keys.import",
            Self::Ls => "keys.ls",
            Self::Export { .. } => "keys.export",
            Self::Use { .. } => "keys.use",
            Self::Rm { .. } => "keys.rm",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let (cmd, print) = (ctx.cmd_name, &ctx.print);
        match self {
            Self::Generate { name, scheme, fund } => {
                thruput_cli::keys::generate(&name, scheme, fund, cmd, print)
            }
            Self::Import { name } => thruput_cli::keys::import(&name, cmd, print),
            Self::Ls => thruput_cli::keys::list(cmd, print),
            Self::Export {
                name,
                format,
                confirmation,
            } => {
                thruput_cli::keys::export_with_format(&name, confirmation.force, format, cmd, print)
            }
            Self::Use { name } => thruput_cli::keys::use_key(&name, cmd, print),
            Self::Rm { name, confirmation } => {
                thruput_cli::keys::remove(&name, confirmation.force, cmd, print)
            }
        }
    }
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
        /// Network hash the RPC endpoint serves, as base58 encoding of exactly 32 bytes
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
        #[command(flatten)]
        confirmation: ConfirmationArgs,
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
        long_about = "Set the profile used when --profile and THRUPUT_PROFILE are omitted. \
                      A global selection requires a global profile. A project selection may use a \
                      profile from either layer. Explicit --profile and THRUPUT_PROFILE still \
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

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        let (cmd, print) = (ctx.cmd_name, &ctx.print);
        match self {
            Self::Get { profile, sources } => {
                thruput_cli::config_cmd::get(&cwd, profile.as_deref(), sources, cmd, print)
            }
            Self::Profiles => thruput_cli::config_cmd::profiles(&cwd, cmd, print),
            Self::New {
                profile,
                network,
                rpc_url,
                network_hash,
                faucet_url,
                default_key,
                select,
                global,
                project,
            } => thruput_cli::config_cmd::new_profile(
                &cwd,
                &profile,
                &thruput_cli::config::NewProfileOptions {
                    network,
                    rpc_url,
                    network_hash,
                    faucet_url,
                    default_key,
                    select,
                },
                global,
                project,
                cmd,
                print,
            ),
            Self::Rm {
                profile,
                confirmation,
                global,
                project,
            } => thruput_cli::config_cmd::remove(
                &cwd,
                &profile,
                confirmation.force,
                global,
                project,
                cmd,
                print,
            ),
            Self::Set {
                field,
                value,
                profile,
                global,
                project,
            } => thruput_cli::config_cmd::set(
                &cwd, &profile, &field, &value, global, project, cmd, print,
            ),
            Self::Unset {
                field,
                profile,
                global,
                project,
            } => {
                thruput_cli::config_cmd::unset(&cwd, &profile, &field, global, project, cmd, print)
            }
            Self::Use {
                profile,
                global,
                project,
            } => thruput_cli::config_cmd::use_default(&cwd, &profile, global, project, cmd, print),
        }
    }
}

#[derive(Subcommand)]
enum ToolchainCommand {
    /// Install an exact catalog version for this host
    Install {
        /// Exact hog-devnode version; defaults to the active selection
        version: Option<String>,
    },
    /// List catalog versions and local installation state
    List,
    /// Select an exact catalog version globally
    Use {
        /// Exact hog-devnode version
        version: String,
    },
    /// Remove one version for this host under the installation lock
    Clean {
        /// Exact hog-devnode version; defaults to the active selection
        version: Option<String>,
    },
    /// Print the active installed hog-devnode path without downloading
    Which,
}

impl ToolchainCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Install { .. } => "toolchain.install",
            Self::List => "toolchain.list",
            Self::Use { .. } => "toolchain.use",
            Self::Clean { .. } => "toolchain.clean",
            Self::Which => "toolchain.which",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let (cmd, print) = (ctx.cmd_name, &ctx.print);
        match self {
            Self::Install { version } => {
                thruput_cli::toolchain_cmd::install(version.as_deref(), ctx.offline, cmd, print)
            }
            Self::List => thruput_cli::toolchain_cmd::list(cmd, print),
            Self::Use { version } => thruput_cli::toolchain_cmd::use_version(&version, cmd, print),
            Self::Clean { version } => {
                thruput_cli::toolchain_cmd::clean(version.as_deref(), cmd, print)
            }
            Self::Which => thruput_cli::toolchain_cmd::which(cmd, print),
        }
    }
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
    /// Save an empty native wallet for a stored signing key
    Create {
        /// Existing key name that should own the new wallet
        #[arg(long)]
        key: Option<String>,
        /// Saved wallet name; defaults to the resolved key name
        #[arg(long)]
        name: Option<String>,
        /// Derive the native wallet without saving it or contacting the network
        #[arg(long)]
        dry_run: bool,
        /// Removed: use thruput airdrop after creating the wallet
        #[arg(long)]
        prefund: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
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
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
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

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let (cmd, print) = (ctx.cmd_name, &ctx.print);
        match self {
            Self::Ls { profile } => thruput_cli::wallet::list(profile.as_deref(), cmd, print),
            Self::Use { name, profile } => {
                thruput_cli::wallet::use_wallet(&name, profile.as_deref(), cmd, print)
            }
            Self::Create {
                key,
                name,
                dry_run,
                prefund,
                confirmation,
                profile,
                target_args,
            } => thruput_cli::wallet::create(
                key.as_deref(),
                name.as_deref(),
                dry_run,
                prefund.as_deref(),
                confirmation.force,
                profile.as_deref(),
                &target_args.into(),
                cmd,
                print,
            ),
            Self::Rm {
                name,
                profile,
                confirmation,
            } => thruput_cli::wallet::remove(
                &name,
                profile.as_deref(),
                confirmation.force,
                cmd,
                print,
            ),
        }
    }
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

impl CodegenCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Ts { .. } => "codegen.ts",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        match self {
            Self::Ts { source, output } => {
                thruput_cli::codegen_cmd::run(&source, output.as_deref(), ctx.cmd_name, &ctx.print)
            }
        }
    }
}

#[derive(Subcommand)]
enum ChainCommand {
    /// Dump a stopped persistent node into one compressed JSON archive
    Dump {
        /// Persistent directory passed to `thruput node --data-dir`
        #[arg(long)]
        data_dir: PathBuf,
        /// JSON archive to create
        #[arg(long)]
        output: PathBuf,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Restore one compressed JSON archive into a stopped persistent node
    Restore {
        /// Persistent directory passed to `thruput node --data-dir`
        #[arg(long)]
        data_dir: PathBuf,
        /// JSON archive to restore
        #[arg(long)]
        input: PathBuf,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
}

impl ChainCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Dump { .. } => "chain.dump",
            Self::Restore { .. } => "chain.restore",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let (cmd, print) = (ctx.cmd_name, &ctx.print);
        match self {
            Self::Dump {
                data_dir,
                output,
                confirmation,
            } => thruput_cli::chain::dump(&data_dir, &output, confirmation.force, cmd, print),
            Self::Restore {
                data_dir,
                input,
                confirmation,
            } => thruput_cli::chain::restore(&data_dir, &input, confirmation.force, cmd, print),
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
                       Receipts are not fetched; inspect selected hashes with `thruput tx show`."
    )]
    List {
        /// Wallet name, contract alias, stored key, or address (default: effective wallet)
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

impl TxCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Show { .. } => "tx.show",
            Self::List { .. } => "tx.list",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        let cwd = invocation_cwd()?;
        let (cmd, print) = (ctx.cmd_name, &ctx.print);
        match self {
            Self::Show {
                tx_hash,
                profile,
                target_args,
            } => thruput_cli::tx::show(
                &tx_hash,
                &cwd,
                profile.as_deref(),
                &target_args.into(),
                cmd,
                print,
            ),
            Self::List {
                target,
                limit,
                cursor,
                profile,
                target_args,
            } => {
                let list = thruput_cli::tx::list;
                list(
                    target.as_deref(),
                    limit.as_deref(),
                    cursor.as_deref(),
                    &cwd,
                    profile.as_deref(),
                    &target_args.into(),
                    cmd,
                    print,
                )
            }
        }
    }
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
        /// Wallet name, contract alias, stored key, or direct address
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

impl EventsCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::List { .. } => "events.list",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        match self {
            Self::List {
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
            } => {
                let list = thruput_cli::events::list;
                list(
                    &contract,
                    &topic0,
                    &topic1,
                    &topic2,
                    limit.as_deref(),
                    cursor.as_deref(),
                    spec.as_deref(),
                    event.as_deref(),
                    &invocation_cwd()?,
                    profile.as_deref(),
                    &target_args.into(),
                    ctx.cmd_name,
                    &ctx.print,
                )
            }
        }
    }
}

#[derive(Subcommand)]
enum TokenCommand {
    /// List one page of canonical ERC20 transfer history
    #[command(
        long_about = "List exactly one newest-first page of canonical ERC20 TransferEvent, MintEvent, and BurnEvent records.\n\n\
                      Participant filters are applied by the server. --account matches transfers \
                      where the address is either sender or recipient, including mints and burns. \
                      --from excludes mints; --to excludes burns. A zero address is a real participant, \
                      not a supply-change marker. JSON schema version 2 uses null for the absent \
                      sender of a mint or recipient of a burn. Amounts remain unformatted \
                      base-unit decimal strings; token metadata is not fetched."
    )]
    Transfers {
        /// Token contract alias, wallet name, stored key, or direct address
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

impl TokenCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Transfers { .. } => "token.transfers",
        }
    }

    fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        match self {
            Self::Transfers {
                token,
                from,
                to,
                account,
                limit,
                cursor,
                profile,
                target_args,
            } => thruput_cli::token::transfers(
                &token,
                from.as_deref(),
                to.as_deref(),
                account.as_deref(),
                limit.as_deref(),
                cursor.as_deref(),
                &invocation_cwd()?,
                profile.as_deref(),
                &target_args.into(),
                ctx.cmd_name,
                &ctx.print,
            ),
        }
    }
}

fn cli_command(bundle_identity: &thruput_bundle_identity::BundleIdentity) -> clap::Command {
    Cli::command()
        .styles(Print::command_styles())
        .version(bundle_identity.to_string())
}

fn parse_cli(bundle_identity: &thruput_bundle_identity::BundleIdentity) -> Cli {
    // Clap can print help/errors before Print exists. Honor the global plain-output
    // switch here too, but do not inspect arguments forwarded after `--`.
    let no_ui = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--no-ui" || arg == "--non-interactive");
    let command = cli_command(bundle_identity).color(if no_ui {
        clap::ColorChoice::Never
    } else {
        clap::ColorChoice::Auto
    });
    let matches = match command.try_get_matches() {
        Ok(matches) => matches,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::DisplayVersion
                && let Some(reason) = bundle_identity.terminal_safe_mismatch_reason()
            {
                Print::new(false, false)
                    .with_no_ui(no_ui)
                    .warn(format_args!("bundle manifest mismatch: {reason}"));
            }
            error.exit()
        }
    };
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

fn main() {
    let bundle_identity = thruput_cli::bundle_identity();
    let cli = parse_cli(&bundle_identity);
    let ctx = Ctx {
        print: Print::new(cli.global.quiet, cli.global.json)
            .with_no_ui(cli.global.no_ui)
            .with_non_interactive(cli.global.non_interactive)
            .with_command_trace(cli.global.show_cargo_commands),
        cmd_name: cli.command.command_name(),
        offline: cli.global.offline
            || std::env::var("THRUPUT_OFFLINE").is_ok_and(|value| value == "1"),
        bundle_identity,
    };

    if let Err(e) = cli.command.run(&ctx) {
        if matches!(
            e,
            CliError::StatusHasWarnings | CliError::CommandReportedFailure
        ) {
            std::process::exit(1);
        }
        if let Err(print_error) = ctx.print.command_error(ctx.cmd_name, &e) {
            eprintln!("error: {print_error}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_node_supervisor::ConfigParamName;

    #[test]
    fn config_get_flags_and_command_name_parse() {
        let cli = Cli::parse_from(["thruput", "config", "get", "devnet", "--sources"]);

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
    fn deploy_alias_flag_parses_and_defaults_to_none() {
        let with_alias = Cli::parse_from([
            "thruput",
            "deploy",
            "--package",
            "my-counter",
            "--alias",
            "counter-v2",
        ]);
        match with_alias.command {
            Command::Deploy(DeployArgs { package, alias, .. }) => {
                assert_eq!(package.as_deref(), Some("my-counter"));
                assert_eq!(alias.as_deref(), Some("counter-v2"));
            }
            _ => panic!("expected deploy command"),
        }

        // Without --alias the package name is what deploy records, so the flag
        // stays absent rather than being defaulted here.
        let without_alias = Cli::parse_from(["thruput", "deploy", "--package", "my-counter"]);
        match without_alias.command {
            Command::Deploy(DeployArgs { alias, .. }) => assert_eq!(alias, None),
            _ => panic!("expected deploy command"),
        }
    }

    #[test]
    fn deploy_rejects_an_empty_alias_before_spending_a_deploy() {
        // An empty or whitespace-only alias is rejected by
        // save_profile_contract_alias, but that runs only after the deploy is
        // submitted -- the contract would be on chain with the CLI reporting an
        // alias it never wrote. The parser rejects it first.
        for empty in ["", "   ", "\t"] {
            let result = Cli::try_parse_from([
                "thruput",
                "deploy",
                "--package",
                "counter",
                "--alias",
                empty,
            ]);
            let Err(error) = result else {
                panic!("--alias {empty:?} should be rejected");
            };
            assert!(
                error.to_string().contains("contract alias cannot be empty"),
                "error should explain the rejection, got: {error}"
            );
        }

        // Surrounding whitespace is trimmed, so what is reported matches the
        // key that gets written.
        let Ok(padded) = Cli::try_parse_from([
            "thruput",
            "deploy",
            "--package",
            "counter",
            "--alias",
            "  token  ",
        ]) else {
            panic!("a padded alias should parse");
        };
        match padded.command {
            Command::Deploy(DeployArgs { alias, .. }) => {
                assert_eq!(alias.as_deref(), Some("token"));
            }
            _ => panic!("expected deploy command"),
        }
    }

    #[test]
    fn deploy_artifact_accepts_an_alias_and_does_not_demand_one() {
        // `--artifact` names a bare binary and carries no package, so there is
        // nothing to derive an alias from. Deploy used to fall back to the
        // project directory name, which matches the package name only by luck
        // in a single-package project and disagrees in any workspace. The flag
        // is how you name it; it stays optional so that dry runs and one-off
        // artifact deploys, which record nothing, need not invent one.
        let Ok(alone) = Cli::try_parse_from([
            "thruput",
            "deploy",
            "--artifact",
            "target/thruput/token.rv32a",
        ]) else {
            panic!("--artifact alone should still parse");
        };
        match alone.command {
            Command::Deploy(DeployArgs { alias, .. }) => assert_eq!(alias, None),
            _ => panic!("expected deploy command"),
        }

        let Ok(paired) = Cli::try_parse_from([
            "thruput",
            "deploy",
            "--artifact",
            "target/thruput/token.rv32a",
            "--alias",
            "token",
        ]) else {
            panic!("--artifact with --alias should parse");
        };
        match paired.command {
            Command::Deploy(DeployArgs {
                artifact, alias, ..
            }) => {
                assert_eq!(artifact, Some(PathBuf::from("target/thruput/token.rv32a")));
                assert_eq!(alias.as_deref(), Some("token"));
            }
            _ => panic!("expected deploy command"),
        }
    }

    #[test]
    fn config_profiles_command_name_parses() {
        let cli = Cli::parse_from(["thruput", "config", "profiles"]);

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
            "thruput",
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

        let remove =
            Cli::parse_from(["thruput", "config", "rm", "staging", "--project", "--force"]);
        assert_eq!(remove.command.command_name(), "config.rm");
        assert!(matches!(
            remove.command,
            Command::Config {
                command: ConfigCommand::Rm {
                    profile,
                    confirmation: ConfirmationArgs { force: true },
                    global: false,
                    project: true,
                }
            } if profile == "staging"
        ));

        let set = Cli::parse_from([
            "thruput",
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
            "thruput",
            "config",
            "unset",
            "faucet-url",
            "--profile",
            "devnet",
            "--global",
        ]);
        assert_eq!(unset.command.command_name(), "config.unset");

        let use_profile = Cli::parse_from(["thruput", "config", "use", "devnet", "--global"]);
        assert_eq!(use_profile.command.command_name(), "config.use");
    }

    #[test]
    fn node_key_flag_parses() {
        let cli = Cli::parse_from(["thruput", "node", "--key", "alice"]);

        match cli.command {
            Command::Node(NodeArgs { key, .. }) => assert_eq!(key.as_deref(), Some("alice")),
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_host_flag_parses() {
        let cli = Cli::parse_from(["thruput", "node", "--host", "0.0.0.0"]);

        match cli.command {
            Command::Node(NodeArgs { host, .. }) => {
                assert_eq!(host, std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
            }
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_host_defaults_to_loopback() {
        let cli = Cli::parse_from(["thruput", "node"]);

        match cli.command {
            Command::Node(NodeArgs { host, .. }) => {
                assert_eq!(host, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
            }
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_host_rejects_invalid_address() {
        let Err(error) = Cli::try_parse_from(["thruput", "node", "--host", "not-an-address"])
        else {
            panic!("invalid host address should be rejected");
        };

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn node_init_network_hash_preserves_canonical_base58() {
        for input in [
            thruput_json_rpc_types::NetworkHashText::from([0xab; 32]).to_string(),
            "1".repeat(32),
        ] {
            let cli =
                Cli::try_parse_from(["thruput", "node", "--init-network-hash", &input]).unwrap();
            let Command::Node(args) = cli.command else {
                panic!("expected node")
            };
            assert_eq!(args.init_network_hash.unwrap().as_str(), input);
        }
        let cli = Cli::try_parse_from(["thruput", "node"]).unwrap();
        let Command::Node(args) = cli.command else {
            panic!("expected node")
        };
        assert!(args.init_network_hash.is_none());
    }

    #[test]
    fn node_init_network_hash_rejects_invalid_inputs() {
        for input in [
            "",
            "ab",
            &"a".repeat(63),
            &"a".repeat(65),
            &"AB".repeat(32),
            &"gg".repeat(32),
            &format!("0X{}", "ab".repeat(32)),
            &"é".repeat(32),
        ] {
            let error = Cli::try_parse_from(["thruput", "node", "--init-network-hash", input])
                .err()
                .unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{input}"
            );
            assert!(
                error
                    .to_string()
                    .contains("base58 encoding of exactly 32 bytes")
            );
        }
    }

    #[test]
    fn node_rejects_the_old_network_hash_flag() {
        let error = Cli::try_parse_from(["thruput", "node", "--network-hash", &"ab".repeat(32)])
            .err()
            .unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn node_initial_account_hog_flag_parses_exact_raw_units() {
        let cli = Cli::parse_from(["thruput", "node", "--initial-account-hog", "25.0000001"]);

        match cli.command {
            Command::Node(NodeArgs {
                initial_account_hog,
                ..
            }) => assert_eq!(initial_account_hog, Some(250_000_001)),
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_data_dir_flag_parses() {
        let cli = Cli::parse_from(["thruput", "node", "--data-dir", "/tmp/thruput"]);

        match cli.command {
            Command::Node(NodeArgs { data_dir, .. }) => {
                assert_eq!(data_dir, Some(PathBuf::from("/tmp/thruput")));
            }
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_contract_artifact_directories_are_repeatable() {
        let cli = Cli::parse_from([
            "thruput",
            "node",
            "--contract-artifacts-dir",
            "/tmp/contracts-a",
            "--contract-artifacts-dir",
            "/tmp/contracts-b",
        ]);

        match cli.command {
            Command::Node(NodeArgs {
                contract_artifact_directories,
                ..
            }) => assert_eq!(
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
    fn call_gas_limit_accepts_override_and_preserves_default() {
        for (extra, expected) in [
            (vec![], 25_000_000),
            (vec!["--gas-limit", "200000000"], 200_000_000),
        ] {
            let mut args = vec!["thruput", "call", "counter", "increment"];
            args.extend(extra);
            match Cli::parse_from(args).command {
                Command::Call(CallArgs { gas_limit, .. }) => assert_eq!(gas_limit, expected),
                _ => panic!("expected call command"),
            }
        }
        assert!(
            Cli::try_parse_from([
                "thruput",
                "call",
                "counter",
                "increment",
                "--gas-limit",
                "0"
            ])
            .is_err()
        );
    }

    #[test]
    fn node_network_config_flags_parse() {
        let cli = Cli::try_parse_from([
            "thruput",
            "node",
            "--network-config",
            "max_rv32_memory_bytes=16M",
            "--network-config",
            "max_bytestring_len=32K",
        ])
        .unwrap();
        match cli.command {
            Command::Node(NodeArgs { network_config, .. }) => assert_eq!(
                network_config,
                [
                    (ConfigParamName::MaxRv32MemoryBytes, 16_777_216),
                    (ConfigParamName::MaxBytestringLen, 32_768),
                ]
            ),
            _ => panic!("expected node"),
        }
        // Past u32, and a name no parameter carries.
        for invalid in ["max_rv32_memory_bytes=4G", "max_rv32_memmory_bytes=16M"] {
            assert!(
                Cli::try_parse_from(["thruput", "node", "--network-config", invalid]).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn node_block_driver_flags_parse() {
        let cli = Cli::parse_from([
            "thruput",
            "node",
            "--block-driver",
            "interval",
            "--block-interval-ms",
            "250",
            "--rpc-max-connections",
            "512",
            "--rpc-batch-concurrency",
            "17",
        ]);

        match cli.command {
            Command::Node(NodeArgs {
                block_driver,
                block_interval_ms,
                rpc_max_connections,
                rpc_batch_concurrency,
                ..
            }) => {
                assert!(matches!(block_driver, Some(NodeBlockDriverArg::Interval)));
                assert_eq!(block_interval_ms, Some(250));
                assert_eq!(rpc_max_connections, Some(512));
                assert_eq!(rpc_batch_concurrency.unwrap().get(), 17);
            }
            _ => panic!("expected node command"),
        }
    }

    #[test]
    fn node_rpc_batch_concurrency_rejects_invalid_values() {
        for value in ["0", "1025"] {
            assert!(
                Cli::try_parse_from(["thruput", "node", "--rpc-batch-concurrency", value]).is_err()
            );
        }
    }

    #[test]
    fn node_profile_flag_is_not_supported() {
        let result = Cli::try_parse_from(["thruput", "node", "--profile", "local"]);
        let Err(err) = result else {
            panic!("node --profile should be rejected");
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn wallet_create_key_flag_parses() {
        let cli = Cli::parse_from(["thruput", "wallet", "create", "--key", "alice"]);

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
        let cli = Cli::parse_from(["thruput", "wallet", "ls"]);

        match cli.command {
            Command::Wallet {
                command: WalletCommand::Ls { profile },
            } => assert_eq!(profile, None),
            _ => panic!("expected wallet ls command"),
        }
    }

    #[test]
    fn wallet_use_flags_parse() {
        let cli = Cli::parse_from(["thruput", "wallet", "use", "trading", "--profile", "devnet"]);

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
        let cli = Cli::parse_from(["thruput", "wallet", "create", "--dry-run"]);

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
            "thruput", "wallet", "create", "--key", "alice", "--name", "trading",
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
            "thruput",
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
        let cli = Cli::parse_from(["thruput", "wallet", "rm", "alice", "--force"]);

        match cli.command {
            Command::Wallet {
                command:
                    WalletCommand::Rm {
                        name,
                        profile,
                        confirmation,
                    },
            } => {
                assert_eq!(name, "alice");
                assert_eq!(profile, None);
                assert!(confirmation.force);
            }
            _ => panic!("expected wallet rm command"),
        }
    }

    #[test]
    fn airdrop_flags_parse() {
        let cli = Cli::parse_from([
            "thruput",
            "airdrop",
            "alice",
            "--amount",
            "1.5",
            "--dry-run",
        ]);

        match cli.command {
            Command::Airdrop(AirdropArgs {
                recipient,
                amount,
                dry_run,
                profile,
                ..
            }) => {
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
            "thruput",
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
            Command::Transfer(TransferArgs {
                recipient,
                amount,
                dry_run,
                from,
                profile,
                target_args,
                confirmation,
            }) => {
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
                assert!(!confirmation.force);
            }
            _ => panic!("expected transfer command"),
        }
    }

    #[test]
    fn transaction_commands_parse_command_local_force() {
        for args in [
            vec!["thruput", "deploy", "--force"],
            vec!["thruput", "call", "contract", "method", "--force"],
            vec!["thruput", "transfer", "recipient", "1", "--force"],
            vec!["thruput", "airdrop", "recipient", "--force"],
            vec!["thruput", "wallet", "create", "--force"],
        ] {
            let cli = Cli::try_parse_from(args).expect("transaction command should accept --force");
            let force = match cli.command {
                Command::Deploy(DeployArgs { confirmation, .. })
                | Command::Call(CallArgs { confirmation, .. })
                | Command::Transfer(TransferArgs { confirmation, .. })
                | Command::Airdrop(AirdropArgs { confirmation, .. })
                | Command::Wallet {
                    command: WalletCommand::Create { confirmation, .. },
                } => confirmation.force,
                _ => panic!("expected transaction command"),
            };
            assert!(force);
        }
    }

    #[test]
    fn airdrop_and_wallet_create_reject_yes() {
        for args in [
            vec!["thruput", "airdrop", "recipient", "--yes"],
            vec!["thruput", "wallet", "create", "--yes"],
        ] {
            let Err(error) = Cli::try_parse_from(args) else {
                panic!("--yes should be removed");
            };
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn tx_show_flags_and_command_name_parse() {
        let cli = Cli::parse_from([
            "thruput",
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
            "thruput",
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
            "thruput",
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
            "thruput",
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
    fn test_workspace_flag_parses() {
        let cli = Cli::parse_from(["thruput", "test", "--workspace"]);

        match cli.command {
            Command::Test(TestArgs {
                workspace, package, ..
            }) => {
                assert!(workspace);
                assert!(package.is_none());
            }
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn test_workspace_flag_conflicts_with_package() {
        let result = Cli::try_parse_from(["thruput", "test", "--workspace", "-p", "demo"]);

        match result {
            Err(err) => assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict),
            Ok(_) => panic!("--workspace with -p should be rejected"),
        }
    }

    #[test]
    fn test_integration_verbose_flag_parses() {
        let cli = Cli::parse_from(["thruput", "test", "--integration", "-v"]);

        match cli.command {
            Command::Test(TestArgs {
                integration,
                verbose,
                ..
            }) => {
                assert!(integration);
                assert!(verbose);
            }
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn test_integration_network_config_flags_parse() {
        let cli = Cli::try_parse_from([
            "thruput",
            "test",
            "--integration",
            "--network-config",
            "max_rv32_memory_bytes=16M",
            "--network-config",
            "max_bytestring_len=32K",
        ])
        .unwrap();
        match cli.command {
            Command::Test(TestArgs { network_config, .. }) => assert_eq!(
                network_config,
                [
                    (ConfigParamName::MaxRv32MemoryBytes, 16_777_216),
                    (ConfigParamName::MaxBytestringLen, 32_768),
                ]
            ),
            _ => panic!("expected test command"),
        }
        // The override only reaches a node, so it needs --integration.
        assert!(
            Cli::try_parse_from([
                "thruput",
                "test",
                "--network-config",
                "max_rv32_memory_bytes=16M",
            ])
            .is_err()
        );
    }

    #[test]
    fn test_integration_node_arg_flags_parse() {
        let cli = Cli::try_parse_from([
            "thruput",
            "test",
            "--integration",
            "--node-arg",
            "--ramdb-bytes 4294967296",
            "--node-arg",
            "--contract-artifacts-dir '/tmp/my contracts'",
            "--",
            "--nocapture",
        ])
        .unwrap();
        match cli.command {
            Command::Test(TestArgs {
                node_arg,
                cargo_args,
                ..
            }) => {
                assert_eq!(
                    node_arg,
                    [
                        NodeArgWords(vec!["--ramdb-bytes".into(), "4294967296".into()]),
                        NodeArgWords(vec![
                            "--contract-artifacts-dir".into(),
                            "/tmp/my contracts".into()
                        ]),
                    ]
                );
                assert_eq!(cargo_args, ["--nocapture"]);
            }
            _ => panic!("expected test command"),
        }
        // The arguments only reach a node, so they need --integration.
        assert!(Cli::try_parse_from(["thruput", "test", "--node-arg", "--ramdb-bytes"]).is_err());
        // Unbalanced quotes are a parse error.
        assert!(
            Cli::try_parse_from(["thruput", "test", "--integration", "--node-arg", "'--x"])
                .is_err()
        );
    }
}
