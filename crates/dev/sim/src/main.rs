use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use groundhog_sim::catalog::{
    ScenarioCatalogEntry, ScenarioContractRef, default_catalog_path, load_catalog,
    resolve_scenario_path,
};
use groundhog_sim::compare::{ResultDiff, load_result};
use groundhog_sim::error::SimError;
use groundhog_sim::manifest::SimulationManifest;
use groundhog_sim::result::{SimResult, SimRunOutcome};
use groundhog_sim::run::{RunOptions, RunSummary, run_target};
use groundhog_sim::sweep::{SweepEntryState, sweep_runs};
use groundhog_sim::util::load_simulation_manifest;
use groundhog_sim::validation::validate_manifest;
use serde::Serialize;

const HOGSIM_COMMAND_JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Parser)]
#[command(name = "hogsim")]
#[command(version, about = "Groundhog stateful simulation and soak-test harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Validate a simulation manifest without sending traffic.
    Validate {
        /// Simulation manifest path.
        scenario: PathBuf,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
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
    /// List curated simulation scenarios.
    Catalog(CatalogArgs),
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
    /// Override the hog-devnode binary path for local target runs.
    #[arg(long = "hog-devnode-binary", value_name = "GROUNDHOG_DEVNODE_BINARY")]
    hogd_dev_binary: Option<PathBuf>,
    /// Override the hog-devnode x23 intro protocol token for local target runs.
    #[arg(long)]
    hogd_protocol: Option<String>,
    /// Override the local target JSON-RPC port.
    #[arg(long)]
    port: Option<u16>,
    /// Override the local target startup timeout in seconds.
    #[arg(long)]
    startup_timeout_secs: Option<u64>,
    /// Root directory used to resolve simulation contract artifact paths.
    #[arg(long)]
    contract_artifact_root: Option<PathBuf>,
    /// Override the output directory.
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// Override the simulation seed before nonce namespace generation.
    #[arg(long)]
    seed: Option<String>,
    /// Fixed high-32-bit nonce namespace. Requires a fresh target or empty replay cache.
    #[arg(long)]
    nonce_namespace: Option<u64>,
    /// Emit structured JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct SweepArgs {
    /// Runs root to scan.
    #[arg(default_value = "hogsim-runs")]
    root: PathBuf,
    /// Validate candidates without sending signals.
    #[arg(long)]
    dry_run: bool,
    /// Emit a single JSON report.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct CatalogArgs {
    /// Scenario catalog path.
    #[arg(long)]
    catalog: Option<PathBuf>,
    /// Only list scenarios carrying this tag.
    #[arg(long)]
    tag: Option<String>,
    /// Emit structured JSON output.
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
            hogd_protocol: args.hogd_protocol,
            port: args.port,
            startup_timeout_secs: args.startup_timeout_secs,
            contract_artifact_root: args.contract_artifact_root,
            out_dir: args.out_dir,
            seed: args.seed,
            nonce_namespace: args.nonce_namespace,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match Box::pin(run_cli()).await {
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
        Command::Validate { scenario, json } => {
            let loaded = load_simulation_manifest(&scenario).await?;
            validate_manifest(&loaded.manifest)?;
            let report = ValidateReport::new(&scenario, &loaded.manifest);
            if json {
                print_json_command("validate", true, &report)?;
            } else {
                println!("valid: {}", scenario.display());
            }
            Ok(())
        }
        Command::Run(args) => {
            let json = args.json;
            if json {
                match Box::pin(run_target(args.into())).await {
                    Ok(summary) => print_json_command("run", true, &RunReport::from(summary))?,
                    Err(SimError::RunFailed(out_dir)) => {
                        let report = RunReport::from_failed_result(PathBuf::from(&out_dir)).await?;
                        print_json_command("run", false, &report)?;
                        return Err(SimError::RunFailed(out_dir));
                    }
                    Err(error) => return Err(error),
                }
            } else {
                let summary = Box::pin(run_target(args.into())).await?;
                println!("completed run");
                println!("run directory: {}", summary.out_dir.display());
            }
            Ok(())
        }
        Command::Compare { run_a, run_b, json } => compare_runs(run_a, run_b, json).await,
        Command::Sweep(args) => sweep_targets_blocking(args).await,
        Command::Catalog(args) => list_catalog(args).await,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HogsimCommandResult<'a, T: Serialize> {
    ok: bool,
    command: &'a str,
    warnings: Vec<String>,
    data: T,
}

fn print_json_command<T: Serialize>(
    command: &'static str,
    ok: bool,
    data: &T,
) -> Result<(), SimError> {
    let result = HogsimCommandResult {
        ok,
        command,
        warnings: Vec::new(),
        data,
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidateReport {
    schema_version: u32,
    status: &'static str,
    scenario: String,
    name: String,
    target_provider: String,
    load_phases: usize,
    workloads: usize,
    contract_artifacts: usize,
    assertions: AssertionReport,
    chaos_events: usize,
}

impl ValidateReport {
    fn new(scenario: &std::path::Path, manifest: &SimulationManifest) -> Self {
        Self {
            schema_version: HOGSIM_COMMAND_JSON_SCHEMA_VERSION,
            status: "valid",
            scenario: scenario.display().to_string(),
            name: manifest.name.clone(),
            target_provider: serde_json::to_value(manifest.target.provider)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{:?}", manifest.target.provider)),
            load_phases: manifest.load_shape.len(),
            workloads: manifest.workloads.len(),
            contract_artifacts: manifest.contract_artifacts.len(),
            assertions: AssertionReport::from_manifest(manifest),
            chaos_events: manifest.chaos.len(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssertionReport {
    configured: usize,
}

impl AssertionReport {
    fn from_manifest(manifest: &SimulationManifest) -> Self {
        let assertions = &manifest.assertions;
        let configured = [
            assertions.max_error_rate.is_some(),
            assertions.max_chain_stall.is_some(),
            assertions.p99_submit_latency_ms.is_some(),
            assertions.max_receipt_timeout_rate.is_some(),
            assertions.max_backpressure_rate.is_some(),
            assertions.max_simulator_overload_rate.is_some(),
            assertions.max_rss_growth_bytes_per_minute.is_some(),
        ]
        .into_iter()
        .filter(|configured| *configured)
        .count();
        Self { configured }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunReport {
    schema_version: u32,
    status: &'static str,
    outcome: SimRunOutcome,
    run_id: String,
    out_dir: String,
    counts: RunReportCounts,
    result_json: String,
    result_jsonl: String,
    simulation_json: String,
}

impl From<RunSummary> for RunReport {
    fn from(summary: RunSummary) -> Self {
        Self {
            schema_version: HOGSIM_COMMAND_JSON_SCHEMA_VERSION,
            status: "completed",
            outcome: summary.outcome,
            run_id: summary.run_id,
            counts: RunReportCounts {
                scheduled: summary.counts.scheduled,
                completed: summary.counts.completed,
                failed: summary.counts.failed,
                skipped: summary.counts.skipped,
            },
            result_json: summary.out_dir.join("result.json").display().to_string(),
            result_jsonl: summary.out_dir.join("result.jsonl").display().to_string(),
            simulation_json: summary
                .out_dir
                .join("simulation.json")
                .display()
                .to_string(),
            out_dir: summary.out_dir.display().to_string(),
        }
    }
}

impl RunReport {
    async fn from_failed_result(out_dir: PathBuf) -> Result<Self, SimError> {
        let result = load_result(&out_dir).await?;
        Ok(Self::from_result(&out_dir, &result))
    }

    fn from_result(out_dir: &Path, result: &SimResult) -> Self {
        let counts = RunReportCounts::from_result(result);
        Self {
            schema_version: HOGSIM_COMMAND_JSON_SCHEMA_VERSION,
            status: "completed",
            outcome: result.outcome,
            run_id: result.run_id.clone(),
            counts,
            result_json: out_dir.join("result.json").display().to_string(),
            result_jsonl: out_dir.join("result.jsonl").display().to_string(),
            simulation_json: out_dir.join("simulation.json").display().to_string(),
            out_dir: out_dir.display().to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunReportCounts {
    scheduled: u64,
    completed: u64,
    failed: u64,
    skipped: u64,
}

impl RunReportCounts {
    fn from_result(result: &SimResult) -> Self {
        let mut counts = Self {
            scheduled: 0,
            completed: 0,
            failed: 0,
            skipped: 0,
        };
        for workload in result.workload_counts.values() {
            counts.completed += workload.completed;
            counts.failed += workload.failed;
            counts.skipped += workload.skipped;
        }
        counts.scheduled = counts.completed + counts.failed + counts.skipped;
        counts
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

async fn list_catalog(args: CatalogArgs) -> Result<(), SimError> {
    let path = args.catalog.unwrap_or_else(default_catalog_path);
    let catalog = load_catalog(&path).await?;
    let entries = catalog.filter_by_tag(args.tag.as_deref());
    let rows = catalog_list_entries(&path, &entries);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print_catalog_table(&rows);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogListEntry<'a> {
    id: &'a str,
    title: &'a str,
    summary: &'a str,
    provider: &'a str,
    tier: &'a str,
    tags: &'a [String],
    path: String,
    resolved_path: String,
    requires_contracts: bool,
    contracts: &'a [ScenarioContractRef],
    runner: &'a str,
}

fn catalog_list_entries<'a>(
    catalog_path: &std::path::Path,
    entries: &[&'a ScenarioCatalogEntry],
) -> Vec<CatalogListEntry<'a>> {
    entries
        .iter()
        .map(|entry| CatalogListEntry {
            id: &entry.id,
            title: &entry.title,
            summary: &entry.summary,
            provider: &entry.provider,
            tier: &entry.tier,
            tags: &entry.tags,
            path: entry.path.display().to_string(),
            resolved_path: resolve_scenario_path(catalog_path, &entry.path)
                .display()
                .to_string(),
            requires_contracts: entry.requires_contracts,
            contracts: &entry.contracts,
            runner: &entry.runner,
        })
        .collect()
}

fn print_catalog_table(entries: &[CatalogListEntry<'_>]) {
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("{}", entry.id);
        println!("  title: {}", entry.title);
        println!("  summary: {}", entry.summary);
        println!("  provider: {}  tier: {}", entry.provider, entry.tier);
        println!("  tags: {}", entry.tags.join(","));
        println!("  scenario: {}", entry.resolved_path);
        println!("  runner: {}", entry.runner);
        if entry.requires_contracts {
            let contracts = entry
                .contracts
                .iter()
                .map(|contract| format!("{} ({})", contract.name, contract.artifact))
                .collect::<Vec<_>>()
                .join(", ");
            println!("  contracts: {contracts}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_local_launch_flags_parse() {
        let cli = Cli::parse_from([
            "hogsim",
            "run",
            "scenario.json",
            "--groundhog-binary",
            "/tmp/groundhog",
            "--hog-devnode-binary",
            "/tmp/hog-devnode",
            "--hogd-protocol",
            "hogp/test",
            "--port",
            "0",
            "--startup-timeout-secs",
            "5",
            "--contract-artifact-root",
            "/tmp/contracts",
            "--json",
        ]);

        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.groundhog_binary, Some(PathBuf::from("/tmp/groundhog")));
                assert_eq!(
                    args.hogd_dev_binary,
                    Some(PathBuf::from("/tmp/hog-devnode"))
                );
                assert_eq!(args.hogd_protocol, Some("hogp/test".into()));
                assert_eq!(args.port, Some(0));
                assert_eq!(args.startup_timeout_secs, Some(5));
                assert_eq!(
                    args.contract_artifact_root,
                    Some(PathBuf::from("/tmp/contracts"))
                );
                assert!(args.json);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn validate_json_flag_parse() {
        let cli = Cli::parse_from(["hogsim", "validate", "scenario.json", "--json"]);

        match cli.command {
            Command::Validate { scenario, json } => {
                assert_eq!(scenario, PathBuf::from("scenario.json"));
                assert!(json);
            }
            _ => panic!("expected validate command"),
        }
    }

    #[test]
    fn validate_report_uses_json_envelope_and_schema_version() {
        let manifest: SimulationManifest = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "local-smoke",
            "seed": "seed",
            "target": { "provider": "local" },
            "loadShape": [
                { "duration": "1s", "users": 1, "ratePerUserHz": 1.0 }
            ],
            "workloads": [{ "name": "chain-tip", "weight": 1 }],
            "assertions": { "maxErrorRate": 0.0 }
        }))
        .expect("bug: manifest fixture should parse");
        let report = ValidateReport::new(std::path::Path::new("scenario.json"), &manifest);
        let envelope = HogsimCommandResult {
            ok: true,
            command: "validate",
            warnings: Vec::new(),
            data: report,
        };
        let json = serde_json::to_value(envelope).expect("bug: report should serialize");

        assert_eq!(json["ok"], true);
        assert_eq!(json["command"], "validate");
        assert_eq!(json["data"]["schemaVersion"], 1);
        assert_eq!(json["data"]["status"], "valid");
        assert_eq!(json["data"]["targetProvider"], "local");
        assert_eq!(json["data"]["assertions"]["configured"], 1);
    }

    #[test]
    fn run_report_uses_json_envelope_and_artifact_paths() {
        let report = RunReport::from(RunSummary {
            run_id: "run-1".into(),
            out_dir: PathBuf::from("hogsim-runs/run-1-local-smoke"),
            outcome: SimRunOutcome::Passed,
            counts: groundhog_sim::run::RunSummaryCounts {
                scheduled: 5,
                completed: 4,
                failed: 0,
                skipped: 1,
            },
        });
        let envelope = HogsimCommandResult {
            ok: true,
            command: "run",
            warnings: Vec::new(),
            data: report,
        };
        let json = serde_json::to_value(envelope).expect("bug: report should serialize");

        assert_eq!(json["ok"], true);
        assert_eq!(json["command"], "run");
        assert_eq!(json["data"]["schemaVersion"], 1);
        assert_eq!(json["data"]["status"], "completed");
        assert_eq!(json["data"]["outcome"], "passed");
        assert_eq!(json["data"]["runId"], "run-1");
        assert_eq!(json["data"]["counts"]["scheduled"], 5);
        assert_eq!(json["data"]["counts"]["completed"], 4);
        assert_eq!(json["data"]["counts"]["failed"], 0);
        assert_eq!(json["data"]["counts"]["skipped"], 1);
        assert_eq!(
            json["data"]["resultJson"],
            "hogsim-runs/run-1-local-smoke/result.json"
        );
        assert_eq!(
            json["data"]["resultJsonl"],
            "hogsim-runs/run-1-local-smoke/result.jsonl"
        );
    }

    #[test]
    fn sweep_flags_parse() {
        let cli = Cli::parse_from(["hogsim", "sweep", "/tmp/runs", "--dry-run", "--json"]);

        match cli.command {
            Command::Sweep(args) => {
                assert_eq!(args.root, PathBuf::from("/tmp/runs"));
                assert!(args.dry_run);
                assert!(args.json);
            }
            _ => panic!("expected sweep command"),
        }
    }

    #[test]
    fn catalog_flags_parse() {
        let cli = Cli::parse_from([
            "hogsim",
            "catalog",
            "--catalog",
            "examples/sim/catalog.json",
            "--tag",
            "contract",
            "--json",
        ]);

        match cli.command {
            Command::Catalog(args) => {
                assert_eq!(
                    args.catalog,
                    Some(PathBuf::from("examples/sim/catalog.json"))
                );
                assert_eq!(args.tag.as_deref(), Some("contract"));
                assert!(args.json);
            }
            _ => panic!("expected catalog command"),
        }
    }

    #[test]
    fn catalog_output_rows_include_resolved_paths() {
        let entry = ScenarioCatalogEntry {
            id: "local-fibonacci-view-smoke".into(),
            path: PathBuf::from("local-fibonacci-view-smoke.json"),
            title: "Fibonacci".into(),
            summary: "Deploys and views fibonacci.".into(),
            provider: "local".into(),
            tier: "smoke".into(),
            tags: vec!["local".into(), "contract".into()],
            requires_contracts: true,
            contracts: vec![ScenarioContractRef {
                name: "fibonacci".into(),
                source: "groundhog-examples/fibonacci".into(),
                artifact: "fibonacci.wasm".into(),
            }],
            runner: "scripts/smoke-sim-scenarios.sh".into(),
        };
        let rows =
            catalog_list_entries(std::path::Path::new("examples/sim/catalog.json"), &[&entry]);

        assert_eq!(rows[0].path, "local-fibonacci-view-smoke.json");
        assert_eq!(
            rows[0].resolved_path,
            "examples/sim/local-fibonacci-view-smoke.json"
        );
        let json = serde_json::to_value(&rows).expect("rows should serialize");
        assert_eq!(
            json[0]["resolvedPath"],
            "examples/sim/local-fibonacci-view-smoke.json"
        );
    }
}
