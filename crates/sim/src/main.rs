use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use groundhog_rpc_api_types::runtime_manifest::RuntimeManifest;
use groundhog_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_sim::event::{EventWriter, ResultEvent, WorkloadEvent};
use groundhog_sim::manifest::WorkloadKind;
use groundhog_sim::manifest::{ResolvedRunSpec, SimulationManifest};
use groundhog_sim::metrics::{RunMetrics, RunMetricsBuilder};
use groundhog_sim::nonce::NonceNamespace;
use groundhog_sim::result::{
    AssertionResult, AssertionStatus, ChainProgressSummary, ChainTipObservation, ManifestHashes,
    MetricAvailability, ProducerIdentity, RuntimeIdentity, SIM_RESULT_SCHEMA_VERSION,
    SimMetricAvailability, SimResult, SimRunOutcome, TargetIdentity, WorkloadCounts,
};
use groundhog_sim::runtime::{derive_accounts, load_runtime_manifest};
use groundhog_sim::scheduler::{
    PhaseStart, ScheduledActorTick, ScheduledSimulationEvent, generate_schedule, phase_starts,
    select_workload_for_tick,
};
use groundhog_sim::target::{TargetOwnership, TargetProviderKind};
use groundhog_sim::validation::{
    validate_against_runtime, validate_existing_target_flags, validate_manifest,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, mpsc};

const DEFAULT_GLOBAL_IN_FLIGHT_LIMIT: usize = 1_024;
const DEFAULT_PER_ACTOR_IN_FLIGHT_LIMIT: usize = 1;

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

async fn run_cli() -> Result<(), CliError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { scenario } => {
            let loaded = load_simulation_manifest(&scenario).await?;
            validate_manifest(&loaded.manifest)?;
            println!("valid: {}", scenario.display());
            Ok(())
        }
        Command::Run(args) => run_existing(args).await,
        Command::Compare { run_a, run_b, json } => compare_runs(&run_a, &run_b, json).await,
    }
}

#[allow(clippy::too_many_lines)]
async fn run_existing(args: RunArgs) -> Result<(), CliError> {
    let mut loaded_sim = load_simulation_manifest(&args.scenario).await?;
    if let Some(seed) = args.seed {
        loaded_sim.manifest.seed = seed;
    }
    validate_manifest(&loaded_sim.manifest)?;

    let runtime_manifest_path = args
        .runtime_manifest
        .clone()
        .or_else(|| loaded_sim.manifest.target.runtime_manifest.clone())
        .ok_or(CliError::MissingRuntimeManifest)?;
    let rpc_url = args
        .rpc_url
        .clone()
        .or_else(|| loaded_sim.manifest.target.rpc_url.clone())
        .ok_or(CliError::MissingRpcUrl)?;
    loaded_sim.manifest.target.rpc_url = Some(rpc_url.clone());
    loaded_sim.manifest.target.runtime_manifest = Some(runtime_manifest_path.clone());
    validate_existing_target_flags(&loaded_sim.manifest, Some(&rpc_url), true)?;

    let loaded_runtime = load_runtime_manifest(&runtime_manifest_path).await?;
    let accounts = derive_accounts(&loaded_runtime.manifest)?;
    validate_against_runtime(&loaded_sim.manifest, &loaded_runtime.manifest)?;
    validate_executable_workloads(&loaded_sim.manifest)?;

    let run_id = run_id()?;
    let nonce_namespace = match args.nonce_namespace {
        Some(value) => NonceNamespace::new(value)?,
        None => NonceNamespace::generate(
            &loaded_sim.manifest.seed,
            &loaded_sim.manifest.name,
            &run_id,
        ),
    };
    loaded_sim.manifest.resolved = Some(ResolvedRunSpec {
        rpc_url: rpc_url.clone(),
        runtime_manifest: runtime_manifest_path,
        nonce_namespace: nonce_namespace.get(),
    });
    loaded_sim.refresh_sha256()?;
    let schedule = generate_schedule(&loaded_sim.manifest)?;
    let phase_starts = phase_starts(&loaded_sim.manifest)?;
    let out_dir = args
        .out_dir
        .unwrap_or_else(|| default_run_dir(&run_id, &loaded_sim.manifest.name));

    let mut event_writer =
        prepare_result_directory(&out_dir, loaded_sim.bytes.as_slice(), &loaded_runtime.bytes)
            .await?;
    write_initial_events(
        &mut event_writer,
        &run_id,
        &rpc_url,
        accounts.len(),
        nonce_namespace,
        &loaded_runtime.sha256,
    )
    .await?;

    let client = HttpClientBuilder::default()
        .build(rpc_url.clone())
        .map_err(|error| CliError::RpcClient(error.to_string()))?;
    let scheduled_workloads = schedule
        .iter()
        .filter(|event| matches!(event, ScheduledSimulationEvent::ActorTick(_)))
        .count() as u64;
    let mut metrics = RunMetricsBuilder::default();
    run_schedule(
        client,
        &loaded_sim.manifest,
        &schedule,
        &phase_starts,
        &mut event_writer,
        &mut metrics,
    )
    .await?;
    let completed_workloads = metrics.completed_count();
    let failed_workloads = metrics.failed_count();
    let skipped_workloads = metrics.skipped_count();
    let workload_counts = metrics.workload_counts();
    let finished_metrics = metrics.finish();
    let assertion_results = evaluate_assertions(
        &loaded_sim.manifest,
        completed_workloads,
        failed_workloads,
        finished_metrics.chain_progress.as_ref(),
        &finished_metrics.workload_metrics,
    );
    for assertion in &assertion_results {
        event_writer
            .write(&ResultEvent::AssertionChecked(assertion.clone()))
            .await?;
    }
    let outcome = determine_outcome(&assertion_results, failed_workloads);
    event_writer
        .write(&ResultEvent::RunCompleted {
            outcome,
            scheduled_workloads,
            completed_workloads,
            failed_workloads,
            skipped_workloads,
        })
        .await?;
    let result = build_result(BuildResultInput {
        loaded_sim: &loaded_sim,
        runtime_manifest: &loaded_runtime.manifest,
        runtime_manifest_sha256: &loaded_runtime.sha256,
        rpc_url: &rpc_url,
        outcome,
        nonce_namespace,
        workload_counts,
        finished_metrics,
        assertion_results,
    });
    write_json_pretty_atomic(out_dir.join("result.json"), &result).await?;

    println!("completed existing target run");
    println!("run directory: {}", out_dir.display());
    if outcome == SimRunOutcome::Failed {
        return Err(CliError::RunFailed(out_dir.display().to_string()));
    }
    Ok(())
}

async fn prepare_result_directory(
    out_dir: &Path,
    simulation_manifest_bytes: &[u8],
    runtime_manifest_bytes: &[u8],
) -> Result<EventWriter, CliError> {
    tokio::fs::create_dir_all(out_dir.join("histograms")).await?;
    tokio::fs::create_dir_all(out_dir.join("logs")).await?;
    tokio::fs::write(out_dir.join("simulation.json"), simulation_manifest_bytes).await?;
    tokio::fs::write(
        out_dir.join("runtime-manifest.json"),
        runtime_manifest_bytes,
    )
    .await?;
    write_json_pretty(
        out_dir.join("target-ownership.json"),
        &TargetOwnership::existing(),
    )
    .await?;
    Ok(EventWriter::create(out_dir.join("result.jsonl")).await?)
}

async fn write_initial_events(
    event_writer: &mut EventWriter,
    run_id: &str,
    rpc_url: &str,
    actor_accounts: usize,
    nonce_namespace: NonceNamespace,
    runtime_manifest_sha256: &str,
) -> Result<(), CliError> {
    event_writer
        .write(&ResultEvent::RunStarted {
            run_id: run_id.into(),
            target_provider: TargetProviderKind::Existing,
            rpc_url: rpc_url.into(),
            actor_accounts,
            nonce_namespace: nonce_namespace.get(),
        })
        .await?;
    event_writer
        .write(&ResultEvent::TargetResolved {
            target_provider: TargetProviderKind::Existing,
            rpc_url: rpc_url.into(),
            runtime_manifest_sha256: runtime_manifest_sha256.into(),
        })
        .await?;
    Ok(())
}

struct BuildResultInput<'a> {
    loaded_sim: &'a LoadedSimulationManifest,
    runtime_manifest: &'a RuntimeManifest,
    runtime_manifest_sha256: &'a str,
    rpc_url: &'a str,
    outcome: SimRunOutcome,
    nonce_namespace: NonceNamespace,
    workload_counts: BTreeMap<WorkloadKind, WorkloadCounts>,
    finished_metrics: RunMetrics,
    assertion_results: Vec<AssertionResult>,
}

fn build_result(input: BuildResultInput<'_>) -> SimResult {
    SimResult {
        schema_version: SIM_RESULT_SCHEMA_VERSION,
        name: input.loaded_sim.manifest.name.clone(),
        outcome: input.outcome,
        target_provider: TargetProviderKind::Existing,
        target_ownership: TargetOwnership::existing(),
        target_identity: TargetIdentity {
            rpc_url: Some(input.rpc_url.into()),
        },
        runtime_identity: RuntimeIdentity {
            network_id: input.runtime_manifest.network_id.clone(),
            protocol: input.runtime_manifest.protocol.clone(),
            genesis_hash: json_string(&input.runtime_manifest.genesis_hash),
            producer: ProducerIdentity {
                binary: input.runtime_manifest.producer.binary.clone(),
                version: input.runtime_manifest.producer.version.clone(),
                commit_hash: input.runtime_manifest.producer.commit_hash.clone(),
            },
        },
        nonce_namespace: input.nonce_namespace.get(),
        manifest_hashes: ManifestHashes {
            simulation_manifest_sha256: input.loaded_sim.sha256.clone(),
            runtime_manifest_sha256: input.runtime_manifest_sha256.into(),
            bootstrap_manifest: MetricAvailability::Unsupported {
                reason: "existing target cannot expose simulator launcher input".into(),
            },
        },
        metric_availability: SimMetricAvailability::phase_one_defaults(),
        workload_counts: input.workload_counts,
        workload_metrics: input.finished_metrics.workload_metrics,
        chain_progress: input.finished_metrics.chain_progress,
        assertion_results: input.assertion_results,
        chaos_events: Vec::new(),
    }
}

async fn run_schedule<C>(
    client: C,
    manifest: &SimulationManifest,
    schedule: &[ScheduledSimulationEvent],
    phase_starts: &[PhaseStart],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), CliError>
where
    C: GroundhogApiClient + Clone + Send + Sync + 'static,
{
    let run_start = Instant::now();
    let mut next_phase = phase_starts.iter().peekable();
    let (completion_tx, mut completion_rx) =
        mpsc::channel::<Result<WorkloadCompletion, CliError>>(DEFAULT_GLOBAL_IN_FLIGHT_LIMIT);
    let global_in_flight = Arc::new(Semaphore::new(DEFAULT_GLOBAL_IN_FLIGHT_LIMIT));
    let actor_in_flight = actor_semaphores(phase_starts);
    let mut in_flight = 0_u64;

    for event in schedule {
        in_flight -= drain_completed_workloads(&mut completion_rx, event_writer, metrics).await?;
        while let Some(phase) =
            next_phase.next_if(|phase| phase.simulation_time_ms <= event.simulation_time_ms())
        {
            sleep_until_sim_time(run_start, phase.simulation_time_ms).await;
            write_phase_started(event_writer, phase).await?;
        }
        match event {
            ScheduledSimulationEvent::ActorTick(tick) => {
                sleep_until_sim_time(run_start, tick.simulation_time_ms).await;
                let workload = select_workload_for_tick(manifest, tick)?;
                if let Some(skipped) = dispatch_workload(
                    client.clone(),
                    tick.clone(),
                    workload,
                    &completion_tx,
                    global_in_flight.clone(),
                    actor_in_flight
                        .get(tick.actor_id as usize)
                        .ok_or(CliError::Executor("actor semaphore missing".into()))?
                        .clone(),
                ) {
                    record_completion(skipped, event_writer, metrics).await?;
                } else {
                    in_flight += 1;
                }
            }
            ScheduledSimulationEvent::Chaos(_) => {}
        }
    }
    for phase in next_phase {
        sleep_until_sim_time(run_start, phase.simulation_time_ms).await;
        write_phase_started(event_writer, phase).await?;
    }
    drop(completion_tx);
    while in_flight > 0 {
        let completion = completion_rx
            .recv()
            .await
            .ok_or(CliError::Executor("completion channel closed early".into()))??;
        record_completion(completion, event_writer, metrics).await?;
        in_flight -= 1;
    }
    Ok(())
}

fn actor_semaphores(phase_starts: &[PhaseStart]) -> Vec<Arc<Semaphore>> {
    let max_users = phase_starts
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or_default();
    (0..max_users)
        .map(|_| Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_IN_FLIGHT_LIMIT)))
        .collect()
}

fn dispatch_workload<C>(
    client: C,
    tick: ScheduledActorTick,
    workload: WorkloadKind,
    completion_tx: &mpsc::Sender<Result<WorkloadCompletion, CliError>>,
    global_in_flight: Arc<Semaphore>,
    actor_in_flight: Arc<Semaphore>,
) -> Option<WorkloadCompletion>
where
    C: GroundhogApiClient + Clone + Send + Sync + 'static,
{
    let global_permit = match global_in_flight.try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            return Some(backpressure_completion(
                &tick,
                workload,
                "global",
                &error.to_string(),
            ));
        }
    };
    let actor_permit = match actor_in_flight.try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            drop(global_permit);
            return Some(backpressure_completion(
                &tick,
                workload,
                "actor",
                &error.to_string(),
            ));
        }
    };
    let completion_tx = completion_tx.clone();

    tokio::spawn(async move {
        let _permits = (global_permit, actor_permit);
        let completion = match workload {
            WorkloadKind::ChainTip => execute_chain_tip(client, &tick).await,
            _ => Err(CliError::UnsupportedRuntimeWorkloads(workload_name(
                workload,
            ))),
        };
        let _ = completion_tx.send(completion).await;
    });

    None
}

async fn drain_completed_workloads(
    completion_rx: &mut mpsc::Receiver<Result<WorkloadCompletion, CliError>>,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<u64, CliError> {
    let mut drained = 0_u64;
    while let Ok(completion) = completion_rx.try_recv() {
        record_completion(completion?, event_writer, metrics).await?;
        drained += 1;
    }
    Ok(drained)
}

async fn record_completion(
    completion: WorkloadCompletion,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), CliError> {
    if let Some(error_class) = completion.error_class {
        if completion.skipped {
            metrics.record_skipped(completion.workload, error_class);
        } else {
            metrics.record_failure(completion.workload, error_class, completion.latency_ms);
        }
    } else {
        metrics.record_success(completion.workload, completion.latency_ms);
    }
    if let Some(observation) = completion.chain_tip {
        metrics.record_chain_tip(observation);
    }
    event_writer.write(&completion.event).await?;
    Ok(())
}

fn backpressure_completion(
    tick: &ScheduledActorTick,
    workload: WorkloadKind,
    scope: &str,
    error: &str,
) -> WorkloadCompletion {
    let now = unix_ms()
        .map(|millis| millis.to_string())
        .unwrap_or_else(|_| "0".into());
    WorkloadCompletion {
        event: ResultEvent::WorkloadSkipped(WorkloadEvent {
            actor_id: tick.actor_id,
            workload,
            scheduled_time_ms: tick.simulation_time_ms,
            started_at_unix_ms: now.clone(),
            completed_at_unix_ms: now,
            latency_ms: 0,
            rpc_method: rpc_method(workload),
            chain_height: None,
            chain_block_hash: None,
            error_class: Some("simulator-backpressure".into()),
            error: Some(format!("{scope} in-flight limit reached: {error}")),
        }),
        workload,
        latency_ms: 0,
        error_class: Some("simulator-backpressure".into()),
        skipped: true,
        chain_tip: None,
    }
}

async fn write_phase_started(
    event_writer: &mut EventWriter,
    phase: &PhaseStart,
) -> Result<(), CliError> {
    event_writer
        .write(&ResultEvent::PhaseStarted {
            phase_index: phase.phase_index,
            simulation_time_ms: phase.simulation_time_ms,
            duration_ms: phase.duration_ms,
            users: phase.users,
            rate_per_user_millihz: phase.rate_per_user_millihz,
        })
        .await?;
    Ok(())
}

async fn execute_chain_tip<C>(
    client: C,
    tick: &ScheduledActorTick,
) -> Result<WorkloadCompletion, CliError>
where
    C: GroundhogApiClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let response = client.chain_tip().await;
    let latency_ms = duration_ms(started.elapsed());
    let completed_at_unix_ms = unix_ms()?.to_string();

    match response {
        Ok(tip) => {
            let height = *tip.height;
            let block_hash = json_string(&tip.block_hash);
            let chain_tip = ChainTipObservation {
                simulation_time_ms: tick.simulation_time_ms,
                height,
                block_hash: block_hash.clone(),
            };
            Ok(WorkloadCompletion {
                workload: WorkloadKind::ChainTip,
                latency_ms,
                error_class: None,
                chain_tip: Some(chain_tip),
                event: ResultEvent::WorkloadCompleted(WorkloadEvent {
                    actor_id: tick.actor_id,
                    workload: WorkloadKind::ChainTip,
                    scheduled_time_ms: tick.simulation_time_ms,
                    started_at_unix_ms,
                    completed_at_unix_ms,
                    latency_ms,
                    rpc_method: "groundhog_chainTip",
                    chain_height: Some(height),
                    chain_block_hash: Some(block_hash),
                    error_class: None,
                    error: None,
                }),
                skipped: false,
            })
        }
        Err(error) => {
            let error_string = error.to_string();
            Ok(WorkloadCompletion {
                workload: WorkloadKind::ChainTip,
                latency_ms,
                error_class: Some("rpc-error".into()),
                chain_tip: None,
                event: ResultEvent::WorkloadFailed(WorkloadEvent {
                    actor_id: tick.actor_id,
                    workload: WorkloadKind::ChainTip,
                    scheduled_time_ms: tick.simulation_time_ms,
                    started_at_unix_ms,
                    completed_at_unix_ms,
                    latency_ms,
                    rpc_method: "groundhog_chainTip",
                    chain_height: None,
                    chain_block_hash: None,
                    error_class: Some("rpc-error".into()),
                    error: Some(error_string),
                }),
                skipped: false,
            })
        }
    }
}

struct WorkloadCompletion {
    event: ResultEvent,
    workload: WorkloadKind,
    latency_ms: u64,
    error_class: Option<String>,
    skipped: bool,
    chain_tip: Option<ChainTipObservation>,
}

async fn sleep_until_sim_time(run_start: Instant, simulation_time_ms: u64) {
    let target = Duration::from_millis(simulation_time_ms);
    let elapsed = run_start.elapsed();
    if let Some(remaining) = target.checked_sub(elapsed) {
        tokio::time::sleep(remaining).await;
    }
}

async fn compare_runs(run_a: &Path, run_b: &Path, json: bool) -> Result<(), CliError> {
    let a = load_result(run_a).await?;
    let b = load_result(run_b).await?;
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

async fn load_result(path: &Path) -> Result<SimResult, CliError> {
    let path = if path.is_dir() {
        path.join("result.json")
    } else {
        path.to_path_buf()
    };
    let bytes = tokio::fs::read(&path).await?;
    let result: SimResult = serde_json::from_slice(&bytes)?;
    if result.schema_version != SIM_RESULT_SCHEMA_VERSION {
        return Err(CliError::UnsupportedResultSchema {
            actual: result.schema_version,
            path: path.display().to_string(),
        });
    }
    Ok(result)
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(rename_all = "camelCase")]
struct ResultDiff {
    same_config: bool,
    simulation_manifest_match: bool,
    runtime_manifest_match: bool,
    runtime_identity_match: bool,
    producer_identity_match: bool,
    storage_tokens_a: String,
    storage_tokens_b: String,
    nnint_compute_a: String,
    nnint_compute_b: String,
}

impl ResultDiff {
    fn from_results(a: &SimResult, b: &SimResult) -> Self {
        let simulation_manifest_match = a.manifest_hashes.simulation_manifest_sha256
            == b.manifest_hashes.simulation_manifest_sha256;
        let runtime_manifest_match =
            a.manifest_hashes.runtime_manifest_sha256 == b.manifest_hashes.runtime_manifest_sha256;
        let runtime_identity_match = a.runtime_identity.network_id == b.runtime_identity.network_id
            && a.runtime_identity.protocol == b.runtime_identity.protocol
            && a.runtime_identity.genesis_hash == b.runtime_identity.genesis_hash;
        let producer_identity_match = a.runtime_identity.producer == b.runtime_identity.producer;

        Self {
            same_config: simulation_manifest_match
                && runtime_manifest_match
                && runtime_identity_match
                && producer_identity_match,
            simulation_manifest_match,
            runtime_manifest_match,
            runtime_identity_match,
            producer_identity_match,
            storage_tokens_a: availability_status(&a.metric_availability.storage_tokens_consumed),
            storage_tokens_b: availability_status(&b.metric_availability.storage_tokens_consumed),
            nnint_compute_a: availability_status(&a.metric_availability.nnint_compute_used),
            nnint_compute_b: availability_status(&b.metric_availability.nnint_compute_used),
        }
    }
}

fn availability_status<T>(availability: &MetricAvailability<T>) -> String {
    match availability {
        MetricAvailability::Observed { .. } => "observed",
        MetricAvailability::Unsupported { .. } => "unsupported",
        MetricAvailability::Unavailable { .. } => "unavailable",
    }
    .into()
}

fn determine_outcome(
    assertion_results: &[AssertionResult],
    failed_workloads: u64,
) -> SimRunOutcome {
    if failed_workloads > 0
        || assertion_results
            .iter()
            .any(|assertion| assertion.status == AssertionStatus::Failed)
    {
        SimRunOutcome::Failed
    } else {
        SimRunOutcome::Passed
    }
}

#[allow(clippy::cast_precision_loss)]
fn evaluate_assertions(
    manifest: &SimulationManifest,
    completed_workloads: u64,
    failed_workloads: u64,
    chain_progress: Option<&ChainProgressSummary>,
    workload_metrics: &BTreeMap<WorkloadKind, groundhog_sim::result::WorkloadMetrics>,
) -> Vec<AssertionResult> {
    let mut results = Vec::new();

    if let Some(max_error_rate) = manifest.assertions.max_error_rate {
        let total = completed_workloads + failed_workloads;
        let observed = if total == 0 {
            0.0
        } else {
            failed_workloads as f64 / total as f64
        };
        results.push(AssertionResult {
            name: "maxErrorRate".into(),
            status: if observed <= max_error_rate {
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed
            },
            observed: Some(format!("{observed:.6}")),
            threshold: Some(format!("{max_error_rate:.6}")),
            reason: None,
        });
    }

    if let Some(max_chain_stall) = &manifest.assertions.max_chain_stall {
        match (
            max_chain_stall.parse_millis(),
            max_chain_stall_ms(chain_progress),
        ) {
            (Ok(threshold_ms), Some(observed_ms)) => {
                results.push(AssertionResult {
                    name: "maxChainStall".into(),
                    status: if observed_ms <= threshold_ms {
                        AssertionStatus::Passed
                    } else {
                        AssertionStatus::Failed
                    },
                    observed: Some(format!("{observed_ms}ms")),
                    threshold: Some(format!("{threshold_ms}ms")),
                    reason: None,
                });
            }
            (Ok(threshold_ms), None) => {
                results.push(AssertionResult {
                    name: "maxChainStall".into(),
                    status: AssertionStatus::Unsupported,
                    observed: None,
                    threshold: Some(format!("{threshold_ms}ms")),
                    reason: Some("no chain-tip observations were recorded".into()),
                });
            }
            (Err(error), _) => {
                results.push(AssertionResult {
                    name: "maxChainStall".into(),
                    status: AssertionStatus::Failed,
                    observed: None,
                    threshold: None,
                    reason: Some(error.to_string()),
                });
            }
        }
    }

    if let Some(threshold) = manifest.assertions.p99_submit_latency_ms {
        results.push(AssertionResult {
            name: "p99SubmitLatencyMs".into(),
            status: AssertionStatus::Unsupported,
            observed: workload_metrics
                .get(&WorkloadKind::WalletSubmitSmoke)
                .and_then(|metrics| metrics.latency_ms.as_ref())
                .map(|latency| format!("{}ms", latency.p99_ms)),
            threshold: Some(format!("{threshold}ms")),
            reason: Some("wallet-submit-smoke execution is not implemented yet".into()),
        });
    }

    if let Some(max_timeout_rate) = manifest.assertions.max_receipt_timeout_rate {
        results.push(AssertionResult {
            name: "maxReceiptTimeoutRate".into(),
            status: AssertionStatus::Unsupported,
            observed: None,
            threshold: Some(format!("{max_timeout_rate:.6}")),
            reason: Some("receipt-poll execution is not implemented yet".into()),
        });
    }

    results
}

fn max_chain_stall_ms(chain_progress: Option<&ChainProgressSummary>) -> Option<u64> {
    let observations = &chain_progress?.observations;
    if observations.is_empty() {
        return None;
    }
    let mut observations: Vec<_> = observations.iter().collect();
    observations.sort_by_key(|observation| observation.simulation_time_ms);

    let first = observations
        .first()
        .expect("bug: observations was checked non-empty");
    let mut last_progress_time = first.simulation_time_ms;
    let mut last_height = first.height;
    let mut max_stall = 0_u64;

    for observation in observations.iter().skip(1) {
        if observation.height > last_height {
            max_stall = max_stall.max(observation.simulation_time_ms - last_progress_time);
            last_progress_time = observation.simulation_time_ms;
            last_height = observation.height;
        }
    }
    let last_time = observations
        .last()
        .expect("bug: observations was checked non-empty")
        .simulation_time_ms;
    Some(max_stall.max(last_time - last_progress_time))
}

fn validate_executable_workloads(manifest: &SimulationManifest) -> Result<(), CliError> {
    let unsupported: Vec<_> = manifest
        .workloads
        .iter()
        .filter(|workload| workload.name != WorkloadKind::ChainTip)
        .map(|workload| workload_name(workload.name))
        .collect();
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(CliError::UnsupportedRuntimeWorkloads(
            unsupported.join(", "),
        ))
    }
}

fn workload_name(workload: WorkloadKind) -> String {
    serde_json::to_value(workload)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{workload:?}"))
}

fn rpc_method(workload: WorkloadKind) -> &'static str {
    match workload {
        WorkloadKind::ChainTip => "groundhog_chainTip",
        WorkloadKind::WalletSubmitSmoke => "groundhog_sendRawTransaction",
        WorkloadKind::ReceiptPoll => "groundhog_getTransactionReceipt",
        WorkloadKind::StatusPoll => "groundhog_getTransactionStatus",
        WorkloadKind::EventsQuery => "groundhog_getEvents",
        WorkloadKind::StorageRead => "groundhog_getStorageAt",
        WorkloadKind::ViewCall => "groundhog_view",
    }
}

fn json_string<T: Serialize + std::fmt::Debug>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{value:?}"))
}

fn unix_ms() -> Result<u128, CliError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::ClockBeforeEpoch)?
        .as_millis())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

struct LoadedSimulationManifest {
    manifest: SimulationManifest,
    sha256: String,
    bytes: Vec<u8>,
}

impl LoadedSimulationManifest {
    fn refresh_sha256(&mut self) -> Result<(), CliError> {
        self.bytes = serde_json::to_vec_pretty(&self.manifest)?;
        self.sha256 = sha256_hex(&self.bytes);
        Ok(())
    }
}

async fn load_simulation_manifest(path: &Path) -> Result<LoadedSimulationManifest, CliError> {
    let bytes = tokio::fs::read(path).await?;
    let manifest = serde_json::from_slice(&bytes)?;
    let sha256 = sha256_hex(&bytes);
    Ok(LoadedSimulationManifest {
        manifest,
        sha256,
        bytes,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(Sha256::digest(bytes)))
}

async fn write_json_pretty(path: PathBuf, value: &impl Serialize) -> Result<(), CliError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

async fn write_json_pretty_atomic(path: PathBuf, value: &impl Serialize) -> Result<(), CliError> {
    let tmp = path.with_extension("json.tmp");
    write_json_pretty(tmp.clone(), value).await?;
    tokio::fs::rename(tmp, path).await?;
    Ok(())
}

fn run_id() -> Result<String, CliError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::ClockBeforeEpoch)?;
    Ok(format!("{}-{:09}", now.as_secs(), now.subsec_nanos()))
}

fn default_run_dir(run_id: &str, name: &str) -> PathBuf {
    PathBuf::from("hoglab-runs").join(format!("{run_id}-{name}"))
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{0}")]
    Validation(#[from] groundhog_sim::validation::ValidationError),
    #[error("{0}")]
    RuntimeManifest(#[from] groundhog_sim::runtime::RuntimeManifestError),
    #[error("{0}")]
    Nonce(#[from] groundhog_sim::nonce::NonceError),
    #[error("{0}")]
    Schedule(#[from] groundhog_sim::scheduler::ScheduleError),
    #[error("{0}")]
    EventWrite(#[from] groundhog_sim::event::EventWriteError),
    #[error("failed filesystem operation: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse or write JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "Phase 1 execution currently supports only chain-tip workloads; unsupported workloads: {0}"
    )]
    UnsupportedRuntimeWorkloads(String),
    #[error("failed to build JSON-RPC client: {0}")]
    RpcClient(String),
    #[error("executor error: {0}")]
    Executor(String),
    #[error("run completed with failed workloads; result directory: {0}")]
    RunFailed(String),
    #[error("existing target requires an RPC URL")]
    MissingRpcUrl,
    #[error("existing target requires a runtime manifest path")]
    MissingRuntimeManifest,
    #[error("system clock is before Unix epoch")]
    ClockBeforeEpoch,
    #[error("unsupported result schemaVersion {actual} in {path}")]
    UnsupportedResultSchema { actual: u32, path: String },
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn evaluates_max_error_rate_assertion() {
        let manifest = manifest_with_assertions(&json!({"maxErrorRate": 0.25}));
        let results = evaluate_assertions(&manifest, 3, 1, None, &BTreeMap::new());

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "maxErrorRate");
        assert_eq!(results[0].status, AssertionStatus::Passed);
    }

    #[test]
    fn workload_failures_fail_run_even_when_assertions_pass() {
        let assertion_results = vec![AssertionResult {
            name: "maxChainStall".into(),
            status: AssertionStatus::Passed,
            observed: Some("0ms".into()),
            threshold: Some("1000ms".into()),
            reason: None,
        }];

        assert_eq!(
            determine_outcome(&assertion_results, 1),
            SimRunOutcome::Failed
        );
    }

    #[test]
    fn fails_max_chain_stall_assertion() {
        let manifest = manifest_with_assertions(&json!({"maxChainStall": "1s"}));
        let chain_progress = ChainProgressSummary {
            first_height: 1,
            last_height: 1,
            max_height: 1,
            total_observations: 2,
            observations: vec![
                ChainTipObservation {
                    simulation_time_ms: 0,
                    height: 1,
                    block_hash: "0x01".into(),
                },
                ChainTipObservation {
                    simulation_time_ms: 2_000,
                    height: 1,
                    block_hash: "0x01".into(),
                },
            ],
        };
        let results = evaluate_assertions(&manifest, 2, 0, Some(&chain_progress), &BTreeMap::new());

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "maxChainStall");
        assert_eq!(results[0].status, AssertionStatus::Failed);
    }

    #[test]
    fn max_chain_stall_handles_out_of_order_observations() {
        let chain_progress = ChainProgressSummary {
            first_height: 1,
            last_height: 1,
            max_height: 2,
            total_observations: 3,
            observations: vec![
                ChainTipObservation {
                    simulation_time_ms: 2_000,
                    height: 2,
                    block_hash: "0x02".into(),
                },
                ChainTipObservation {
                    simulation_time_ms: 0,
                    height: 1,
                    block_hash: "0x01".into(),
                },
                ChainTipObservation {
                    simulation_time_ms: 1_000,
                    height: 1,
                    block_hash: "0x01".into(),
                },
            ],
        };

        assert_eq!(max_chain_stall_ms(Some(&chain_progress)), Some(2_000));
    }

    #[tokio::test]
    async fn load_result_rejects_future_schema_version() {
        let file = NamedTempFile::new().expect("temp file should be created");
        tokio::fs::write(
            file.path(),
            serde_json::to_vec(&minimal_result_json(99)).unwrap(),
        )
        .await
        .expect("result should write");

        let err = load_result(file.path())
            .await
            .expect_err("future schema should reject");

        assert!(matches!(
            err,
            CliError::UnsupportedResultSchema { actual: 99, .. }
        ));
    }

    fn manifest_with_assertions(assertions: &serde_json::Value) -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "assertion-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}],
            "assertions": assertions
        }))
        .expect("manifest fixture should parse")
    }

    fn minimal_result_json(schema_version: u32) -> serde_json::Value {
        json!({
            "schemaVersion": schema_version,
            "name": "result-test",
            "outcome": "passed",
            "targetProvider": "existing",
            "targetOwnership": {
                "provider": "existing",
                "startedBySim": false,
                "cleanupAllowed": false
            },
            "nonceNamespace": 1,
            "manifestHashes": {
                "simulationManifestSha256": "sim",
                "runtimeManifestSha256": "runtime",
                "bootstrapManifest": {
                    "status": "unsupported",
                    "reason": "existing target"
                }
            },
            "metricAvailability": {
                "storageTokensConsumed": {
                    "status": "unsupported",
                    "reason": "not surfaced"
                },
                "nnintComputeUsed": {
                    "status": "unsupported",
                    "reason": "not surfaced"
                }
            }
        })
    }
}
