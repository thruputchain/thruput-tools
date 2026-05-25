use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use groundhog_sim::compare::{ResultDiff, load_result};
use groundhog_sim::error::SimError;
use groundhog_sim::run::{ExistingRunOptions, run_existing_target};
use groundhog_sim::util::load_simulation_manifest;
use groundhog_sim::validation::validate_manifest;

#[derive(Debug, Parser)]
#[command(name = "hoglab")]
#[command(about = "Groundhog stateful simulation and soak-test harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a simulation manifest without sending traffic.
    Validate {
        /// Simulation manifest path.
        scenario: PathBuf,
    },
    /// Run a simulation against an existing target.
    Run(RunArgs),
    /// Compare two completed run directories or result.json files.
    Compare {
        run_a: PathBuf,
        run_b: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Parser)]
struct RunArgs {
    /// Simulation manifest path.
    scenario: PathBuf,
    /// Override the target RPC URL.
    #[arg(long)]
    rpc_url: Option<String>,
    /// Override the runtime manifest path.
    #[arg(long)]
    runtime_manifest: Option<PathBuf>,
    /// Override the output directory.
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// Override the simulation seed before nonce namespace generation.
    #[arg(long)]
    seed: Option<String>,
    /// Fixed high-32-bit nonce namespace. Requires a fresh target or empty replay cache.
    #[arg(long)]
    nonce_namespace: Option<u64>,
}

impl From<RunArgs> for ExistingRunOptions {
    fn from(args: RunArgs) -> Self {
        Self {
            scenario: args.scenario,
            rpc_url: args.rpc_url,
            runtime_manifest: args.runtime_manifest,
            out_dir: args.out_dir,
            seed: args.seed,
            nonce_namespace: args.nonce_namespace,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run_cli() -> Result<(), SimError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { scenario } => {
            let loaded = load_simulation_manifest(&scenario).await?;
            validate_manifest(&loaded.manifest)?;
            println!("valid: {}", scenario.display());
            Ok(())
        }
        Command::Run(args) => {
            let summary = run_existing_target(args.into()).await?;
            println!("completed existing target run");
            println!("run directory: {}", summary.out_dir.display());
            Ok(())
        }
        Command::Compare { run_a, run_b, json } => compare_runs(run_a, run_b, json).await,
    }
}

async fn compare_runs(run_a: PathBuf, run_b: PathBuf, json: bool) -> Result<(), SimError> {
    let a = load_result(&run_a).await?;
    let b = load_result(&run_b).await?;
    let diff = ResultDiff::from_results(&a, &b);

    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        println!("config match: {}", diff.same_config);
        println!("runtime identity match: {}", diff.runtime_identity_match);
        println!("producer identity match: {}", diff.producer_identity_match);
        println!(
            "storage-token availability: {} -> {}",
            diff.storage_tokens_a, diff.storage_tokens_b
        );
        println!(
            "NNInt compute availability: {} -> {}",
            diff.nnint_compute_a, diff.nnint_compute_b
        );
    }
    Ok(())
}
