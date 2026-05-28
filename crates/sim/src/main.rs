use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use groundhog_sim::compare::{ResultDiff, load_result};
use groundhog_sim::error::SimError;
use groundhog_sim::run::{RunOptions, run_target};
use groundhog_sim::sweep::{SweepEntryState, sweep_runs};
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
    /// Run a simulation against the configured target provider.
    Run(RunArgs),
    /// Compare two completed run directories or result.json files.
    Compare {
        run_a: PathBuf,
        run_b: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Reclaim orphaned simulator-owned local targets.
    Sweep(SweepArgs),
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
    /// Override the groundhog binary path for local target runs.
    #[arg(long)]
    groundhog_binary: Option<PathBuf>,
    /// Override the hogd-dev binary path for local target runs.
    #[arg(long)]
    hogd_dev_binary: Option<PathBuf>,
    /// Override the local target JSON-RPC port.
    #[arg(long)]
    port: Option<u16>,
    /// Override the local target startup timeout in seconds.
    #[arg(long)]
    startup_timeout_secs: Option<u64>,
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

#[derive(Debug, Parser)]
struct SweepArgs {
    /// Runs root to scan.
    #[arg(default_value = "hoglab-runs")]
    root: PathBuf,
    /// Validate candidates without sending signals.
    #[arg(long)]
    dry_run: bool,
    /// Emit a single JSON report.
    #[arg(long)]
    json: bool,
}

impl From<RunArgs> for RunOptions {
    fn from(args: RunArgs) -> Self {
        Self {
            scenario: args.scenario,
            rpc_url: args.rpc_url,
            runtime_manifest: args.runtime_manifest,
            groundhog_binary: args.groundhog_binary,
            hogd_dev_binary: args.hogd_dev_binary,
            port: args.port,
            startup_timeout_secs: args.startup_timeout_secs,
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
            let summary = run_target(args.into()).await?;
            println!("completed run");
            println!("run directory: {}", summary.out_dir.display());
            Ok(())
        }
        Command::Compare { run_a, run_b, json } => compare_runs(run_a, run_b, json).await,
        Command::Sweep(args) => sweep_targets_blocking(args).await,
    }
}

async fn compare_runs(run_a: PathBuf, run_b: PathBuf, json: bool) -> Result<(), SimError> {
    let a = load_result(&run_a).await?;
    let b = load_result(&run_b).await?;
    let diff = ResultDiff::from_results(&a, &b);

    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        println!("{}", diff.render_text());
    }
    Ok(())
}

fn sweep_targets(args: &SweepArgs) -> Result<(), SimError> {
    let report = sweep_runs(&args.root, args.dry_run)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let reclaimed = report
            .entries
            .iter()
            .filter(|entry| entry.state == SweepEntryState::Reclaimed)
            .count();
        let skipped = report
            .entries
            .iter()
            .filter(|entry| entry.state == SweepEntryState::Skipped)
            .count();
        let failed = report
            .entries
            .iter()
            .filter(|entry| entry.state == SweepEntryState::Failed)
            .count();
        println!(
            "sweep: root={} reclaimed={} skipped={} failed={}",
            report.root, reclaimed, skipped, failed
        );
        for entry in &report.entries {
            match entry.state {
                SweepEntryState::Reclaimed => println!(
                    "reclaimed: {} pid={} port={}",
                    entry.run_dir,
                    entry.pid.unwrap_or_default(),
                    entry.port.unwrap_or_default()
                ),
                SweepEntryState::Skipped => println!(
                    "skipped: {} {}",
                    entry.run_dir,
                    entry.reason.as_deref().unwrap_or("unknown reason")
                ),
                SweepEntryState::Failed => println!(
                    "failed: {} {}",
                    entry.run_dir,
                    entry.error.as_deref().unwrap_or("unknown error")
                ),
            }
        }
    }

    if report.has_failures() {
        return Err(SimError::Executor(
            "one or more validated local targets failed to terminate".into(),
        ));
    }
    Ok(())
}

async fn sweep_targets_blocking(args: SweepArgs) -> Result<(), SimError> {
    tokio::task::spawn_blocking(move || sweep_targets(&args))
        .await
        .map_err(|error| SimError::Executor(format!("sweep task panicked: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_local_launch_flags_parse() {
        let cli = Cli::parse_from([
            "hoglab",
            "run",
            "scenario.json",
            "--groundhog-binary",
            "/tmp/groundhog",
            "--hogd-dev-binary",
            "/tmp/hogd-dev",
            "--port",
            "0",
            "--startup-timeout-secs",
            "5",
        ]);

        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.groundhog_binary, Some(PathBuf::from("/tmp/groundhog")));
                assert_eq!(args.hogd_dev_binary, Some(PathBuf::from("/tmp/hogd-dev")));
                assert_eq!(args.port, Some(0));
                assert_eq!(args.startup_timeout_secs, Some(5));
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn sweep_flags_parse() {
        let cli = Cli::parse_from(["hoglab", "sweep", "/tmp/runs", "--dry-run", "--json"]);

        match cli.command {
            Command::Sweep(args) => {
                assert_eq!(args.root, PathBuf::from("/tmp/runs"));
                assert!(args.dry_run);
                assert!(args.json);
            }
            _ => panic!("expected sweep command"),
        }
    }
}
