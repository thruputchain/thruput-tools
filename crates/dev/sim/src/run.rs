use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClient, HttpClientBuilder};
use groundhog_json_rpc_types::{
    ContractAddress,
    block::ChainTip,
    events::{EventPage, EventQuery},
    hex::HexBytes,
    receipt::TransactionReceipt,
    runtime_manifest::RuntimeManifest,
    state::StorageItemRead,
    transaction::TxHash,
    view::{ViewRequest, ViewResponse},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::actor::{
    ActorError, ActorState, build_actor_states_from_accounts, required_actor_count,
};
use crate::assertions::{
    AssertionEvaluation, determine_outcome, evaluate_assertions, workload_event_rate,
};
use crate::compare::{ResultDiff, load_result};
use crate::contract::{ContractSetup, deploy_manifest_contracts};
use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent};
use crate::executor::{
    DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL, DEFAULT_RECEIPT_DRAIN_TIMEOUT, ScheduleExecution,
    SimRpcClient, StreamingScheduleExecution, drain_pending_receipt_polls, run_schedule_with_chaos,
    run_streaming_schedule_with_chaos,
};
use crate::manifest::{ClientSpec, ResolvedRunSpec, SimulationManifest, WorkloadKind};
use crate::metrics::{RunMetrics, RunMetricsBuilder};
use crate::nonce::NonceNamespace;
use crate::provision::{ActorProvisionInputs, provision_missing_actor_accounts};
use crate::resource::TargetResourceSampler;
use crate::result::{
    AssertionResult, BaselineDiff, ManifestHashes, MetricAvailability, ProducerIdentity,
    RuntimeIdentity, SIM_RESULT_SCHEMA_VERSION, SimMetricAvailability, SimResult, SimRunOutcome,
    TargetIdentity, WorkloadCounts,
};
use crate::runtime::{derive_accounts, load_runtime_manifest, preflight_chain_identity};
use crate::scheduler::{
    PhaseStart, ScheduledSimulationEvent, phase_starts, resource_sample_interval_ms,
    schedule_summary,
};
use crate::target::{
    ExistingTargetProvider, LocalTargetProvider, ResolvedTarget, TargetOverrides, TargetOwnership,
    TargetProvider, TargetProviderContext, TargetProviderKind,
};
use crate::util::{
    LoadedSimulationManifest, default_run_dir, json_string, load_simulation_manifest, run_id,
    workload_name, write_json_pretty, write_json_pretty_atomic,
};
use crate::validation::{validate_against_runtime, validate_manifest};

pub const DEFAULT_RPC_CLIENT_CONCURRENCY: u32 = 512;
const DEFAULT_RPC_REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RpcClientConfig {
    max_concurrent_requests: u32,
    pool_size: u32,
    request_timeout: Duration,
}

impl RpcClientConfig {
    fn into_manifest_spec(self) -> ClientSpec {
        ClientSpec {
            max_concurrent_requests: Some(self.max_concurrent_requests),
            pool_size: Some(self.pool_size),
        }
    }
}

#[derive(Clone)]
struct SimHttpClientPool {
    clients: Arc<Vec<HttpClient>>,
    next_client: Arc<AtomicUsize>,
    request_limit: Arc<Semaphore>,
}

impl SimHttpClientPool {
    fn new(rpc_url: &str, config: RpcClientConfig) -> Result<Self, SimError> {
        if config.pool_size == 0 || config.max_concurrent_requests == 0 {
            return Err(SimError::RpcClient(
                "simulator RPC client pool and request limits must be at least 1".into(),
            ));
        }
        let pool_size = usize::try_from(config.pool_size).unwrap_or(usize::MAX);
        let mut clients = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            clients.push(
                HttpClientBuilder::default()
                    .request_timeout(config.request_timeout)
                    .build(rpc_url)
                    .map_err(|error| SimError::RpcClient(error.to_string()))?,
            );
        }

        Ok(Self {
            clients: Arc::new(clients),
            next_client: Arc::new(AtomicUsize::new(0)),
            request_limit: Arc::new(Semaphore::new(
                usize::try_from(config.max_concurrent_requests).unwrap_or(usize::MAX),
            )),
        })
    }

    async fn acquire_client(&self) -> Result<(OwnedSemaphorePermit, HttpClient), String> {
        let permit = self
            .request_limit
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "simulator RPC client limiter closed".to_string())?;
        let index = self.next_client.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        Ok((permit, self.clients[index].clone()))
    }
}

impl SimRpcClient for SimHttpClientPool {
    #[allow(clippy::manual_async_fn)]
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
        let pool = self.clone();
        async move {
            let (_permit, client) = pool.acquire_client().await?;
            GroundhogApiClient::chain_tip(&client)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn send_raw_transaction(
        &self,
        tx_hex: String,
    ) -> impl Future<Output = Result<TxHash, String>> + Send {
        let pool = self.clone();
        async move {
            let (_permit, client) = pool.acquire_client().await?;
            GroundhogApiClient::send_raw_transaction(&client, tx_hex)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
        let pool = self.clone();
        async move {
            let (_permit, client) = pool.acquire_client().await?;
            GroundhogApiClient::get_transaction_receipt(&client, tx_hash)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn view(
        &self,
        request: ViewRequest,
    ) -> impl Future<Output = Result<ViewResponse, String>> + Send {
        let pool = self.clone();
        async move {
            let (_permit, client) = pool.acquire_client().await?;
            GroundhogApiClient::view(&client, request)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn storage_read(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> impl Future<Output = Result<StorageItemRead, String>> + Send {
        let pool = self.clone();
        async move {
            let (_permit, client) = pool.acquire_client().await?;
            GroundhogApiClient::get_storage_at(&client, address, key)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn events_query(
        &self,
        query: EventQuery,
    ) -> impl Future<Output = Result<EventPage, String>> + Send {
        let pool = self.clone();
        async move {
            let (_permit, client) = pool.acquire_client().await?;
            GroundhogApiClient::get_events(&client, query)
                .await
                .map_err(|error| error.to_string())
        }
    }
}

/// Options for running a simulation.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub scenario: PathBuf,
    pub rpc_url: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
    pub groundhog_binary: Option<PathBuf>,
    pub hogd_dev_binary: Option<PathBuf>,
    pub hogd_protocol: Option<String>,
    pub port: Option<u16>,
    pub startup_timeout_secs: Option<u64>,
    pub contract_artifact_root: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub seed: Option<String>,
    pub nonce_namespace: Option<u64>,
}

/// Options for running a simulation against an already-started target.
#[derive(Debug, Clone)]
pub struct ExistingRunOptions {
    pub scenario: PathBuf,
    pub rpc_url: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub seed: Option<String>,
    pub nonce_namespace: Option<u64>,
}

impl From<ExistingRunOptions> for RunOptions {
    fn from(args: ExistingRunOptions) -> Self {
        Self {
            scenario: args.scenario,
            rpc_url: args.rpc_url,
            runtime_manifest: args.runtime_manifest,
            groundhog_binary: None,
            hogd_dev_binary: None,
            hogd_protocol: None,
            port: None,
            startup_timeout_secs: None,
            contract_artifact_root: None,
            out_dir: args.out_dir,
            seed: args.seed,
            nonce_namespace: args.nonce_namespace,
        }
    }
}

/// Run a simulation against the provider selected by the manifest.
pub async fn run_target(args: RunOptions) -> Result<RunSummary, SimError> {
    let RunOptions {
        scenario,
        rpc_url,
        runtime_manifest,
        groundhog_binary,
        hogd_dev_binary,
        hogd_protocol,
        port,
        startup_timeout_secs,
        contract_artifact_root,
        out_dir,
        seed,
        nonce_namespace,
    } = args;
    let scenario_path = scenario.clone();
    let mut loaded_sim = load_simulation_manifest(&scenario).await?;
    if let Some(seed) = seed {
        loaded_sim.manifest.seed = seed;
    }
    validate_manifest(&loaded_sim.manifest)?;
    let overrides = TargetOverrides {
        rpc_url,
        runtime_manifest,
        groundhog_binary,
        hogd_dev_binary,
        hogd_protocol,
        port,
        startup_timeout_secs,
    };
    match loaded_sim.manifest.target.provider {
        TargetProviderKind::Existing => {
            let provider = ExistingTargetProvider;
            Box::pin(run_with_provider(
                loaded_sim,
                &scenario_path,
                &provider,
                overrides,
                out_dir,
                nonce_namespace,
                contract_artifact_root.as_deref(),
            ))
            .await
        }
        TargetProviderKind::Local => {
            let provider = LocalTargetProvider::new();
            Box::pin(run_with_provider(
                loaded_sim,
                &scenario_path,
                &provider,
                overrides,
                out_dir,
                nonce_namespace,
                contract_artifact_root.as_deref(),
            ))
            .await
        }
        TargetProviderKind::RemoteSsh => {
            Err(crate::validation::ValidationError::ProviderNotImplemented {
                provider: TargetProviderKind::RemoteSsh,
            }
            .into())
        }
    }
}

/// Summary of a completed simulation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub run_id: String,
    pub out_dir: PathBuf,
    pub outcome: SimRunOutcome,
    pub counts: RunSummaryCounts,
}

/// Aggregate workload counts for a completed simulation run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunSummaryCounts {
    pub scheduled: u64,
    pub completed: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl RunSummaryCounts {
    fn from_workloads(
        scheduled: u64,
        workload_counts: &BTreeMap<WorkloadKind, WorkloadCounts>,
    ) -> Self {
        let mut counts = Self {
            scheduled,
            ..Self::default()
        };
        for workload in workload_counts.values() {
            counts.completed += workload.completed;
            counts.failed += workload.failed;
            counts.skipped += workload.skipped;
        }
        counts
    }
}

/// Run a simulation against an already-started target.
pub async fn run_existing_target(args: ExistingRunOptions) -> Result<RunSummary, SimError> {
    let ExistingRunOptions {
        scenario,
        rpc_url,
        runtime_manifest,
        out_dir,
        seed,
        nonce_namespace,
    } = args;
    let scenario_path = scenario.clone();
    let mut loaded_sim = load_simulation_manifest(&scenario).await?;
    if let Some(seed) = seed {
        loaded_sim.manifest.seed = seed;
    }
    validate_manifest(&loaded_sim.manifest)?;
    let provider = ExistingTargetProvider;
    Box::pin(run_with_provider(
        loaded_sim,
        &scenario_path,
        &provider,
        TargetOverrides {
            rpc_url,
            runtime_manifest,
            ..TargetOverrides::default()
        },
        out_dir,
        nonce_namespace,
        None,
    ))
    .await
}

async fn run_with_provider(
    loaded_sim: LoadedSimulationManifest,
    scenario_path: &Path,
    provider: &dyn TargetProvider,
    overrides: TargetOverrides,
    out_dir: Option<PathBuf>,
    nonce_namespace: Option<u64>,
    contract_artifact_root: Option<&Path>,
) -> Result<RunSummary, SimError> {
    let run_id = run_id()?;
    let out_dir = out_dir.unwrap_or_else(|| default_run_dir(&run_id, &loaded_sim.manifest.name));
    let context = TargetProviderContext {
        logs_dir: out_dir.join("logs"),
        ownership_path: out_dir.join("target-ownership.json"),
    };
    let target = provider
        .acquire(&loaded_sim.manifest, overrides, context)
        .await?;
    let run_result = Box::pin(run_resolved_target(
        loaded_sim,
        scenario_path,
        &target,
        provider.chaos_controller(),
        provider.resource_sampler(),
        out_dir,
        run_id,
        nonce_namespace,
        contract_artifact_root,
    ))
    .await;
    let cleanup_result = provider.cleanup(&target).await;
    finish_target_run(run_result, cleanup_result)
}

fn finish_target_run<T>(
    run_result: Result<T, SimError>,
    cleanup_result: Result<(), SimError>,
) -> Result<T, SimError> {
    match (run_result, cleanup_result) {
        (Err(run_error), _) => Err(run_error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Ok(output), Ok(())) => Ok(output),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_resolved_target(
    mut loaded_sim: LoadedSimulationManifest,
    scenario_path: &Path,
    target: &ResolvedTarget,
    chaos_controller: Option<&dyn crate::target::ChaosController>,
    resource_sampler: Option<&dyn TargetResourceSampler>,
    out_dir: PathBuf,
    run_id: String,
    nonce_namespace: Option<u64>,
    contract_artifact_root: Option<&Path>,
) -> Result<RunSummary, SimError> {
    loaded_sim.manifest.target.rpc_url = Some(target.rpc_url.clone());
    loaded_sim.manifest.target.runtime_manifest = Some(target.runtime_manifest.clone());
    let rpc_client_config = effective_rpc_client_config(&loaded_sim.manifest);
    loaded_sim.manifest.client = Some(rpc_client_config.into_manifest_spec());
    loaded_sim.manifest.target.max_rpc_client_concurrency = None;

    let loaded_runtime = load_runtime_manifest(&target.runtime_manifest).await?;
    preflight_chain_identity(
        &loaded_runtime.manifest,
        &target.rpc_url,
        rpc_client_config.request_timeout,
    )
    .await?;
    let accounts = derive_accounts(&loaded_runtime.manifest)?;
    validate_executable_workloads(&loaded_sim.manifest)?;
    validate_against_runtime(&loaded_sim.manifest, &loaded_runtime.manifest)?;

    let nonce_namespace = match nonce_namespace {
        Some(value) => NonceNamespace::new(value)?,
        None => NonceNamespace::generate(
            &loaded_sim.manifest.seed,
            &loaded_sim.manifest.name,
            &run_id,
        ),
    };
    let required_actors = required_actor_count(&loaded_sim.manifest);
    let mut actor_states = build_actor_states_from_accounts(accounts, nonce_namespace)?;
    if actor_states.len() > required_actors {
        actor_states.truncate(required_actors);
    }
    loaded_sim.manifest.resolved = Some(ResolvedRunSpec {
        rpc_url: target.rpc_url.clone(),
        runtime_manifest: target.runtime_manifest.clone(),
        nonce_namespace: nonce_namespace.get(),
    });
    loaded_sim.refresh_hashes()?;
    let phase_starts = phase_starts(&loaded_sim.manifest)?;
    let schedule_summary = schedule_summary(&loaded_sim.manifest)?;
    let resource_sample_interval_ms = resource_sample_interval_ms(&loaded_sim.manifest)?;

    let mut event_writer = prepare_result_directory(
        &out_dir,
        loaded_sim.bytes.as_slice(),
        &loaded_runtime.bytes,
        &target.ownership,
    )
    .await?;
    write_initial_events(
        &mut event_writer,
        &run_id,
        target,
        required_actors,
        nonce_namespace,
        &loaded_runtime.sha256,
    )
    .await?;

    let client = SimHttpClientPool::new(&target.rpc_url, rpc_client_config)?;
    provision_missing_actor_accounts(
        client.clone(),
        ActorProvisionInputs {
            manifest: &loaded_sim.manifest,
            runtime_manifest: &loaded_runtime.manifest,
            runtime_manifest_path: &target.runtime_manifest,
            scenario_path,
            contract_artifact_root,
        },
        &mut actor_states,
        &mut event_writer,
    )
    .await?;
    if actor_states.len() < required_actors {
        return Err(ActorError::TooManyActors {
            requested: required_actors,
            available: actor_states.len(),
        }
        .into());
    }

    let contract_setup = deploy_manifest_contracts(
        client.clone(),
        &loaded_sim.manifest,
        &loaded_runtime.manifest,
        scenario_path,
        contract_artifact_root,
        &mut actor_states,
        &mut event_writer,
    )
    .await?;
    let mut result = execute_loaded_simulation(
        client,
        LoadedExecution {
            run_id: &run_id,
            loaded_sim: &loaded_sim,
            runtime_manifest: &loaded_runtime.manifest,
            runtime_manifest_sha256: &loaded_runtime.sha256,
            runtime_config_sha256: &loaded_runtime.config_sha256,
            target,
            chaos_controller,
            nonce_namespace,
            actor_states: &mut actor_states,
            scheduled_workloads: schedule_summary.scheduled_workloads,
            shutdown_simulation_time_ms: schedule_summary.shutdown_simulation_time_ms,
            resource_sample_interval_ms,
            resource_sampler,
            schedule: None,
            phase_starts: &phase_starts,
            event_writer: &mut event_writer,
            contract_setup,
        },
    )
    .await?;
    refresh_result_target_ownership(&mut result, &out_dir).await?;
    attach_baseline_diff(
        &mut result,
        scenario_path,
        loaded_sim.manifest.baseline.as_deref(),
    )
    .await?;
    let outcome = result.outcome;
    let counts = RunSummaryCounts::from_workloads(
        schedule_summary.scheduled_workloads,
        &result.workload_counts,
    );
    write_json_pretty_atomic(out_dir.join("result.json"), &result).await?;

    let summary = RunSummary {
        run_id,
        out_dir,
        outcome,
        counts,
    };
    if outcome == SimRunOutcome::Failed {
        return Err(SimError::RunFailed(summary.out_dir.display().to_string()));
    }
    Ok(summary)
}

fn effective_rpc_client_config(manifest: &SimulationManifest) -> RpcClientConfig {
    let max_concurrent_requests = manifest
        .client
        .as_ref()
        .and_then(|client| client.max_concurrent_requests)
        .or(manifest.target.max_rpc_client_concurrency)
        .unwrap_or(DEFAULT_RPC_CLIENT_CONCURRENCY);
    let pool_size = manifest
        .client
        .as_ref()
        .and_then(|client| client.pool_size)
        .unwrap_or_else(|| default_rpc_client_pool_size(manifest, max_concurrent_requests));

    RpcClientConfig {
        max_concurrent_requests,
        pool_size,
        request_timeout: DEFAULT_RPC_REQUEST_TIMEOUT,
    }
}

fn default_rpc_client_pool_size(
    manifest: &SimulationManifest,
    max_concurrent_requests: u32,
) -> u32 {
    manifest
        .load_shape
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or(1)
        .min(max_concurrent_requests)
        .max(1)
}

/// Inputs for executing an already-loaded simulation.
pub struct LoadedExecution<'a> {
    pub run_id: &'a str,
    pub loaded_sim: &'a LoadedSimulationManifest,
    pub runtime_manifest: &'a RuntimeManifest,
    pub runtime_manifest_sha256: &'a str,
    pub runtime_config_sha256: &'a str,
    pub target: &'a ResolvedTarget,
    pub chaos_controller: Option<&'a dyn crate::target::ChaosController>,
    pub nonce_namespace: NonceNamespace,
    pub actor_states: &'a mut [ActorState],
    pub scheduled_workloads: u64,
    pub shutdown_simulation_time_ms: u64,
    pub resource_sample_interval_ms: u64,
    pub resource_sampler: Option<&'a dyn TargetResourceSampler>,
    pub schedule: Option<&'a [ScheduledSimulationEvent]>,
    pub phase_starts: &'a [PhaseStart],
    pub event_writer: &'a mut EventWriter,
    pub contract_setup: ContractSetup,
}

/// Execute a loaded simulation against a supplied client.
#[allow(clippy::too_many_lines)]
pub async fn execute_loaded_simulation<C>(
    client: C,
    input: LoadedExecution<'_>,
) -> Result<SimResult, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    if input.loaded_sim.config_sha256.is_none() {
        return Err(SimError::Executor(
            "simulation configuration hash was not resolved".into(),
        ));
    }
    let mut metrics = RunMetricsBuilder::default();
    metrics.set_resource_sample_interval_ms(input.resource_sample_interval_ms);
    let mut chaos_events = Vec::new();
    let schedule_report = if input.contract_setup.failed {
        crate::executor::ScheduleRunReport {
            chaos_failed: false,
        }
    } else if let Some(schedule) = input.schedule {
        run_schedule_with_chaos(
            client.clone(),
            ScheduleExecution {
                manifest: &input.loaded_sim.manifest,
                runtime_manifest: input.runtime_manifest,
                schedule,
                phase_starts: input.phase_starts,
                actor_states: input.actor_states,
                event_writer: &mut *input.event_writer,
                metrics: &mut metrics,
                view_calls: &input.contract_setup.view_calls,
                contract_calls: &input.contract_setup.contract_calls,
                storage_reads: &input.contract_setup.storage_reads,
                event_queries: &input.contract_setup.event_queries,
                read_after_write: &input.contract_setup.read_after_write,
                resource_sampler: input.resource_sampler,
            },
            input.chaos_controller,
            &mut chaos_events,
        )
        .await?
    } else {
        run_streaming_schedule_with_chaos(
            client.clone(),
            StreamingScheduleExecution {
                manifest: &input.loaded_sim.manifest,
                runtime_manifest: input.runtime_manifest,
                phase_starts: input.phase_starts,
                actor_states: input.actor_states,
                event_writer: &mut *input.event_writer,
                metrics: &mut metrics,
                view_calls: &input.contract_setup.view_calls,
                contract_calls: &input.contract_setup.contract_calls,
                storage_reads: &input.contract_setup.storage_reads,
                event_queries: &input.contract_setup.event_queries,
                read_after_write: &input.contract_setup.read_after_write,
                resource_sampler: input.resource_sampler,
            },
            input.chaos_controller,
            &mut chaos_events,
        )
        .await?
    };
    let reconciled_receipt_polls = if schedule_report.chaos_failed {
        0
    } else {
        drain_pending_receipt_polls(
            client,
            input.actor_states,
            &mut *input.event_writer,
            &mut metrics,
            input.shutdown_simulation_time_ms,
            DEFAULT_RECEIPT_DRAIN_TIMEOUT,
            DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL,
        )
        .await?
    };
    let completed_workloads = metrics.completed_count();
    let failed_workloads = metrics.failed_count();
    let skipped_workloads = metrics.skipped_count();
    let backpressure_workloads = metrics.simulator_backpressure_count();
    let workload_counts = metrics.workload_counts();
    let pending_receipts = input
        .actor_states
        .iter()
        .map(ActorState::pending_receipt_count)
        .sum();
    metrics.record_pending_receipts_at_shutdown(pending_receipts);
    let finished_metrics = metrics.finish();
    let metric_availability = SimMetricAvailability::phase_one_defaults();
    let backpressure_rate = workload_event_rate(backpressure_workloads, input.scheduled_workloads);
    let simulator_overload_rate = workload_event_rate(
        finished_metrics.simulator_overload_count,
        input.scheduled_workloads,
    );
    let assertion_results = evaluate_assertions(
        &input.loaded_sim.manifest,
        AssertionEvaluation {
            completed_workloads,
            failed_workloads,
            scheduled_actor_workloads: input.scheduled_workloads,
            backpressure_workloads,
            simulator_overload_count: finished_metrics.simulator_overload_count,
            chain_progress: finished_metrics.chain_progress.as_ref(),
            resource_timeline: finished_metrics.resource_timeline.as_ref(),
            workload_metrics: &finished_metrics.workload_metrics,
        },
    );
    for assertion in &assertion_results {
        input
            .event_writer
            .write(&ResultEvent::AssertionChecked {
                assertion: assertion.clone(),
            })
            .await?;
    }
    input
        .event_writer
        .write(&ResultEvent::MetricAvailabilityRecorded {
            metric_availability: metric_availability.clone(),
        })
        .await?;
    let mut outcome = determine_outcome(&assertion_results, failed_workloads);
    if input.contract_setup.failed {
        outcome = SimRunOutcome::Failed;
    }
    if schedule_report.chaos_failed {
        outcome = SimRunOutcome::Failed;
    }
    input
        .event_writer
        .write(&ResultEvent::RunCompleted {
            outcome,
            scheduled_workloads: input.scheduled_workloads + reconciled_receipt_polls,
            completed_workloads,
            failed_workloads,
            skipped_workloads,
        })
        .await?;

    build_result(BuildResultInput {
        loaded_sim: input.loaded_sim,
        run_id: input.run_id,
        runtime_manifest: input.runtime_manifest,
        runtime_manifest_sha256: input.runtime_manifest_sha256,
        runtime_config_sha256: input.runtime_config_sha256,
        target: input.target,
        outcome,
        nonce_namespace: input.nonce_namespace,
        workload_counts,
        backpressure_rate,
        simulator_overload_rate,
        finished_metrics,
        metric_availability,
        assertion_results,
        chaos_events,
        contracts: input.contract_setup.records,
    })
}

async fn attach_baseline_diff(
    result: &mut SimResult,
    scenario_path: &Path,
    baseline: Option<&Path>,
) -> Result<(), SimError> {
    let Some(baseline) = baseline else {
        return Ok(());
    };

    let baseline_load_path = resolve_baseline_path(scenario_path, baseline);
    let baseline_result = load_result(&baseline_load_path).await?;
    result.baseline_diff = Some(BaselineDiff {
        path: baseline.display().to_string(),
        diff: ResultDiff::from_results(&baseline_result, result),
    });
    Ok(())
}

async fn refresh_result_target_ownership(
    result: &mut SimResult,
    out_dir: &Path,
) -> Result<(), SimError> {
    let ownership: TargetOwnership =
        serde_json::from_slice(&tokio::fs::read(out_dir.join("target-ownership.json")).await?)?;
    result.target_ownership = ownership;
    Ok(())
}

fn resolve_baseline_path(scenario_path: &Path, baseline: &Path) -> PathBuf {
    if baseline.is_absolute() {
        return baseline.to_path_buf();
    }

    scenario_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(baseline)
}

async fn prepare_result_directory(
    out_dir: &Path,
    simulation_manifest_bytes: &[u8],
    runtime_manifest_bytes: &[u8],
    target_ownership: &TargetOwnership,
) -> Result<EventWriter, SimError> {
    tokio::fs::create_dir_all(out_dir.join("histograms")).await?;
    tokio::fs::create_dir_all(out_dir.join("logs")).await?;
    tokio::fs::write(out_dir.join("simulation.json"), simulation_manifest_bytes).await?;
    tokio::fs::write(
        out_dir.join("runtime-manifest.json"),
        runtime_manifest_bytes,
    )
    .await?;
    write_json_pretty(out_dir.join("target-ownership.json"), target_ownership).await?;
    tokio::fs::write(out_dir.join("result.jsonl"), b"").await?;
    Ok(EventWriter::create(out_dir.join("result.jsonl")).await?)
}

async fn write_initial_events(
    event_writer: &mut EventWriter,
    run_id: &str,
    target: &ResolvedTarget,
    actor_accounts: usize,
    nonce_namespace: NonceNamespace,
    runtime_manifest_sha256: &str,
) -> Result<(), SimError> {
    event_writer
        .write(&ResultEvent::RunStarted {
            run_id: run_id.into(),
            target_provider: target.provider,
            rpc_url: target.rpc_url.clone(),
            actor_accounts,
            nonce_namespace: nonce_namespace.get(),
        })
        .await?;
    event_writer
        .write(&ResultEvent::TargetResolved {
            target_provider: target.provider,
            rpc_url: target.rpc_url.clone(),
            runtime_manifest_sha256: runtime_manifest_sha256.into(),
        })
        .await?;
    Ok(())
}

struct BuildResultInput<'a> {
    loaded_sim: &'a LoadedSimulationManifest,
    run_id: &'a str,
    runtime_manifest: &'a RuntimeManifest,
    runtime_manifest_sha256: &'a str,
    runtime_config_sha256: &'a str,
    target: &'a ResolvedTarget,
    outcome: SimRunOutcome,
    nonce_namespace: NonceNamespace,
    workload_counts: BTreeMap<WorkloadKind, WorkloadCounts>,
    backpressure_rate: f64,
    simulator_overload_rate: f64,
    finished_metrics: RunMetrics,
    metric_availability: SimMetricAvailability,
    assertion_results: Vec<AssertionResult>,
    chaos_events: Vec<crate::result::ChaosEventRecord>,
    contracts: Vec<crate::result::ContractDeploymentRecord>,
}

fn build_result(input: BuildResultInput<'_>) -> Result<SimResult, SimError> {
    let simulation_config_sha256 = input.loaded_sim.config_sha256.clone().ok_or_else(|| {
        SimError::Executor("simulation configuration hash was not resolved".into())
    })?;
    Ok(SimResult {
        schema_version: SIM_RESULT_SCHEMA_VERSION,
        run_id: input.run_id.into(),
        name: input.loaded_sim.manifest.name.clone(),
        outcome: input.outcome,
        target_provider: input.target.provider,
        target_ownership: input.target.ownership.clone(),
        target_identity: TargetIdentity {
            rpc_url: Some(input.target.rpc_url.clone()),
        },
        runtime_identity: RuntimeIdentity {
            network_hash: input.runtime_manifest.network_hash.to_string(),
            protocol: input.runtime_manifest.protocol.clone(),
            genesis_hash: json_string(&input.runtime_manifest.genesis_hash, "genesisHash")?,
            producer: ProducerIdentity {
                binary: input.runtime_manifest.producer.binary.clone(),
                version: input.runtime_manifest.producer.version.clone(),
                commit_hash: input.runtime_manifest.producer.commit_hash.clone(),
            },
        },
        nonce_namespace: input.nonce_namespace.get(),
        manifest_hashes: ManifestHashes {
            simulation_manifest_sha256: input.loaded_sim.sha256.clone(),
            simulation_config_sha256: Some(simulation_config_sha256),
            runtime_manifest_sha256: input.runtime_manifest_sha256.into(),
            runtime_config_sha256: Some(input.runtime_config_sha256.into()),
            bootstrap_manifest: MetricAvailability::Unsupported {
                reason: "existing target cannot expose simulator launcher input".into(),
            },
        },
        metric_availability: input.metric_availability,
        workload_counts: input.workload_counts,
        workload_metrics: input.finished_metrics.workload_metrics,
        backpressure_rate: input.backpressure_rate,
        simulator_overload_rate: input.simulator_overload_rate,
        simulator_overload_count: input.finished_metrics.simulator_overload_count,
        diagnostics: input.finished_metrics.diagnostics,
        resource_timeline: input.finished_metrics.resource_timeline.map_or_else(
            || MetricAvailability::Unavailable {
                reason: "no target resource samples were observed".into(),
            },
            |value| MetricAvailability::Observed { value },
        ),
        chain_progress: input.finished_metrics.chain_progress,
        assertion_results: input.assertion_results,
        chaos_events: input.chaos_events,
        contracts: input.contracts,
        baseline_diff: None,
    })
}

fn validate_executable_workloads(manifest: &SimulationManifest) -> Result<(), SimError> {
    let unsupported: Vec<_> = manifest
        .workloads
        .iter()
        .filter(|workload| {
            !matches!(
                workload.name,
                WorkloadKind::ChainTip
                    | WorkloadKind::WalletSubmitSmoke
                    | WorkloadKind::ReceiptPoll
                    | WorkloadKind::ContractCall
                    | WorkloadKind::EventsQuery
                    | WorkloadKind::ViewCall
                    | WorkloadKind::StorageRead
                    | WorkloadKind::ReadAfterWrite
            )
        })
        .map(|workload| workload_name(workload.name))
        .collect();
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(SimError::UnsupportedRuntimeWorkloads(
            unsupported.join(", "),
        ))
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::{BlockHeight, block::ChainTip, hex::Hex0x, transaction::TxHash};
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};
    use serde_json::json;
    use tempfile::{NamedTempFile, tempdir};

    use super::*;
    use crate::event::EventWriter;
    use crate::executor::SimRpcClient;
    use crate::manifest::ChaosEventSpec;
    use crate::runtime::SimAccount;
    use crate::scheduler::{generate_schedule, phase_starts};
    use crate::target::{ChaosController, ChaosEventOutcome, TargetProviderFuture};
    use crate::util::{sha256_hex, simulation_config_sha256};

    #[derive(Clone)]
    struct MockClient {
        tip: ChainTip,
    }

    impl SimRpcClient for MockClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl std::future::Future<Output = Result<ChainTip, String>> + Send {
            let tip = self.tip.clone();
            async move { Ok(tip) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl std::future::Future<Output = Result<TxHash, String>> + Send {
            async { Ok(TxHash(Hex0x(SHA256Hash::from([0x55; 32])))) }
        }
    }

    struct FailingChaosController;

    impl ChaosController for FailingChaosController {
        fn execute<'a>(
            &'a self,
            _event: &'a ChaosEventSpec,
        ) -> TargetProviderFuture<'a, Result<ChaosEventOutcome, SimError>> {
            Box::pin(async { Err(SimError::Executor("restart failed".into())) })
        }
    }

    #[test]
    fn effective_rpc_client_config_defaults_pool_to_peak_users() {
        let mut manifest = manifest();
        manifest.load_shape[0].users = 1_000;

        let config = effective_rpc_client_config(&manifest);

        assert_eq!(
            config,
            RpcClientConfig {
                max_concurrent_requests: DEFAULT_RPC_CLIENT_CONCURRENCY,
                pool_size: DEFAULT_RPC_CLIENT_CONCURRENCY,
                request_timeout: DEFAULT_RPC_REQUEST_TIMEOUT,
            }
        );
    }

    #[test]
    fn effective_rpc_client_config_uses_explicit_client_config() {
        let mut manifest = manifest();
        manifest.client = Some(ClientSpec {
            max_concurrent_requests: Some(128),
            pool_size: Some(32),
        });

        let config = effective_rpc_client_config(&manifest);

        assert_eq!(
            config,
            RpcClientConfig {
                max_concurrent_requests: 128,
                pool_size: 32,
                request_timeout: DEFAULT_RPC_REQUEST_TIMEOUT,
            }
        );
    }

    #[test]
    fn effective_rpc_client_config_accepts_legacy_target_concurrency() {
        let mut manifest = manifest();
        manifest.load_shape[0].users = 100;
        manifest.target.max_rpc_client_concurrency = Some(64);

        let config = effective_rpc_client_config(&manifest);

        assert_eq!(
            config,
            RpcClientConfig {
                max_concurrent_requests: 64,
                pool_size: 64,
                request_timeout: DEFAULT_RPC_REQUEST_TIMEOUT,
            }
        );
    }

    #[tokio::test]
    async fn execute_loaded_simulation_rejects_missing_config_hash_before_events() {
        let manifest = manifest();
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest should encode");
        let loaded_sim = LoadedSimulationManifest {
            config_sha256: None,
            manifest,
            sha256: sha256_hex(&bytes),
            bytes,
        };
        let runtime_manifest = runtime_manifest();
        let schedule = generate_schedule(&loaded_sim.manifest).expect("schedule should generate");
        let starts = phase_starts(&loaded_sim.manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut actors = actor_states();
        let target = resolved_target();

        let error = execute_loaded_simulation(
            MockClient {
                tip: ChainTip {
                    height: BlockHeight::from(42),
                    block_hash: Hex0x(SHA256Hash::from([0x42; 32])),
                },
            },
            LoadedExecution {
                run_id: "test-run",
                loaded_sim: &loaded_sim,
                runtime_manifest: &runtime_manifest,
                runtime_manifest_sha256: "runtime-sha",
                runtime_config_sha256: "runtime-config-sha",
                target: &target,
                chaos_controller: None,
                nonce_namespace: NonceNamespace::new(7).expect("nonce namespace should fit"),
                actor_states: &mut actors,
                scheduled_workloads: 1,
                shutdown_simulation_time_ms: 0,
                resource_sample_interval_ms: 1_000,
                resource_sampler: None,
                schedule: Some(&schedule),
                phase_starts: &starts,
                event_writer: &mut event_writer,
                contract_setup: empty_contract_setup(),
            },
        )
        .await
        .expect_err("missing stable hash should fail before execution");

        assert!(
            matches!(error, SimError::Executor(message) if message.contains("hash was not resolved"))
        );
        assert_eq!(
            tokio::fs::read(file.path())
                .await
                .expect("event file should read"),
            Vec::<u8>::new()
        );
    }

    #[tokio::test]
    async fn execute_loaded_simulation_records_chain_tip_result() {
        let manifest = manifest();
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest should encode");
        let loaded_sim = LoadedSimulationManifest {
            config_sha256: Some(
                simulation_config_sha256(&manifest).expect("config hash should encode"),
            ),
            manifest,
            sha256: sha256_hex(&bytes),
            bytes,
        };
        let runtime_manifest = runtime_manifest();
        let schedule = generate_schedule(&loaded_sim.manifest).expect("schedule should generate");
        let starts = phase_starts(&loaded_sim.manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut actors = actor_states();
        let target = resolved_target();

        let result = execute_loaded_simulation(
            MockClient {
                tip: ChainTip {
                    height: BlockHeight::from(42),
                    block_hash: Hex0x(SHA256Hash::from([0x42; 32])),
                },
            },
            LoadedExecution {
                run_id: "test-run",
                loaded_sim: &loaded_sim,
                runtime_manifest: &runtime_manifest,
                runtime_manifest_sha256: "runtime-sha",
                runtime_config_sha256: "runtime-config-sha",
                target: &target,
                chaos_controller: None,
                nonce_namespace: NonceNamespace::new(7).expect("nonce namespace should fit"),
                actor_states: &mut actors,
                scheduled_workloads: 1,
                shutdown_simulation_time_ms: schedule
                    .iter()
                    .map(ScheduledSimulationEvent::simulation_time_ms)
                    .max()
                    .unwrap_or_default(),
                resource_sample_interval_ms: resource_sample_interval_ms(&loaded_sim.manifest)
                    .expect("resource interval should resolve"),
                resource_sampler: None,
                schedule: Some(&schedule),
                phase_starts: &starts,
                event_writer: &mut event_writer,
                contract_setup: empty_contract_setup(),
            },
        )
        .await
        .expect("simulation should execute");

        assert_eq!(result.outcome, SimRunOutcome::Passed);
        assert_eq!(result.run_id, "test-run");
        assert_eq!(
            result.manifest_hashes.simulation_config_sha256.as_deref(),
            loaded_sim.config_sha256.as_deref()
        );
        assert_eq!(
            result.manifest_hashes.runtime_config_sha256.as_deref(),
            Some("runtime-config-sha")
        );
        assert_eq!(
            result.target_identity.rpc_url.as_deref(),
            Some("http://127.0.0.1:9545")
        );
        let metrics = result
            .workload_metrics
            .get(&WorkloadKind::ChainTip)
            .expect("chain-tip metrics should exist");
        assert_eq!(metrics.counts.completed, 1);
        assert_eq!(
            result
                .chain_progress
                .as_ref()
                .expect("chain progress should exist")
                .last_height,
            42
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"kind\":\"workload-completed\""));
        assert!(events.contains("\"kind\":\"metric-availability-recorded\""));
        assert!(events.contains("\"kind\":\"run-completed\""));
    }

    #[tokio::test]
    async fn failed_chaos_marks_result_failed_and_records_chaos_events() {
        let manifest = chaos_manifest();
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest should encode");
        let loaded_sim = LoadedSimulationManifest {
            config_sha256: Some(
                simulation_config_sha256(&manifest).expect("config hash should encode"),
            ),
            manifest,
            sha256: sha256_hex(&bytes),
            bytes,
        };
        let runtime_manifest = runtime_manifest();
        let schedule = generate_schedule(&loaded_sim.manifest).expect("schedule should generate");
        let starts = phase_starts(&loaded_sim.manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut actors = actor_states();
        let target = resolved_target();

        let result = execute_loaded_simulation(
            MockClient {
                tip: ChainTip {
                    height: BlockHeight::from(42),
                    block_hash: Hex0x(SHA256Hash::from([0x42; 32])),
                },
            },
            LoadedExecution {
                run_id: "test-run",
                loaded_sim: &loaded_sim,
                runtime_manifest: &runtime_manifest,
                runtime_manifest_sha256: "runtime-sha",
                runtime_config_sha256: "runtime-config-sha",
                target: &target,
                chaos_controller: Some(&FailingChaosController),
                nonce_namespace: NonceNamespace::new(7).expect("nonce namespace should fit"),
                actor_states: &mut actors,
                scheduled_workloads: 2,
                shutdown_simulation_time_ms: schedule
                    .iter()
                    .map(ScheduledSimulationEvent::simulation_time_ms)
                    .max()
                    .unwrap_or_default(),
                resource_sample_interval_ms: resource_sample_interval_ms(&loaded_sim.manifest)
                    .expect("resource interval should resolve"),
                resource_sampler: None,
                schedule: Some(&schedule),
                phase_starts: &starts,
                event_writer: &mut event_writer,
                contract_setup: empty_contract_setup(),
            },
        )
        .await
        .expect("failed chaos should still build a result");

        assert_eq!(result.outcome, SimRunOutcome::Failed);
        assert!(
            result
                .chaos_events
                .iter()
                .any(|event| event.state == crate::result::ChaosEventState::Failed)
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"kind\":\"chaos-transition\""));
        assert!(events.contains("\"outcome\":\"failed\""));
    }

    #[tokio::test]
    async fn prepare_result_directory_writes_existing_target_ownership() {
        let dir = tempdir().expect("temp dir should be created");
        let out_dir = dir.path().join("run");
        let mut writer = prepare_result_directory(
            &out_dir,
            b"{\"simulation\":true}",
            b"{\"runtime\":true}",
            &TargetOwnership::existing(),
        )
        .await
        .expect("result directory should prepare");
        writer
            .write(&ResultEvent::RunCompleted {
                outcome: SimRunOutcome::Passed,
                scheduled_workloads: 0,
                completed_workloads: 0,
                failed_workloads: 0,
                skipped_workloads: 0,
            })
            .await
            .expect("event should write");

        let ownership: TargetOwnership = serde_json::from_slice(
            &tokio::fs::read(out_dir.join("target-ownership.json"))
                .await
                .expect("ownership should read"),
        )
        .expect("ownership should parse");
        assert_eq!(ownership, TargetOwnership::existing());
        assert!(
            tokio::fs::try_exists(out_dir.join("result.jsonl"))
                .await
                .expect("result stream existence should be knowable")
        );
    }

    #[tokio::test]
    async fn prepare_result_directory_truncates_stale_result_stream() {
        let dir = tempdir().expect("temp dir should be created");
        let out_dir = dir.path().join("run");
        tokio::fs::create_dir_all(&out_dir)
            .await
            .expect("result dir should be created");
        tokio::fs::write(out_dir.join("result.jsonl"), b"{\"kind\":\"stale\"}\n")
            .await
            .expect("stale event should write");

        let mut writer = prepare_result_directory(
            &out_dir,
            b"{\"simulation\":true}",
            b"{\"runtime\":true}",
            &TargetOwnership::existing(),
        )
        .await
        .expect("result directory should prepare");
        writer
            .write(&ResultEvent::RunCompleted {
                outcome: SimRunOutcome::Passed,
                scheduled_workloads: 0,
                completed_workloads: 0,
                failed_workloads: 0,
                skipped_workloads: 0,
            })
            .await
            .expect("event should write");

        let contents = tokio::fs::read_to_string(out_dir.join("result.jsonl"))
            .await
            .expect("result stream should read");
        assert!(!contents.contains("stale"));
        assert!(contents.contains("\"kind\":\"run-completed\""));
    }

    #[test]
    fn validate_executable_workloads_accepts_read_after_write() {
        let manifest: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": crate::manifest::SIMULATION_SCHEMA_VERSION,
            "kind": "groundhog-simulation",
            "name": "read-after-write-runtime-gate-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "read-after-write", "weight": 1}],
            "contractArtifacts": [{"name": "caller-counter", "artifact": {"kind": "wasm", "path": "caller-counter.wasm"}}],
            "readAfterWrite": [{
                "name": "counter-roundtrip",
                "contract": "caller-counter",
                "write": {"entrypoint": "0x1111111111111111"},
                "read": {"entrypoint": "0x2222222222222222"}
            }]
        }))
        .expect("manifest fixture should parse");

        validate_executable_workloads(&manifest)
            .expect("read-after-write should be runtime-supported");
    }

    #[test]
    fn target_run_error_wins_over_cleanup_error() {
        let result = finish_target_run::<()>(
            Err(SimError::Executor("run failed".into())),
            Err(SimError::Executor("cleanup failed".into())),
        );

        assert!(matches!(
            result,
            Err(SimError::Executor(message)) if message == "run failed"
        ));
    }

    #[test]
    fn target_cleanup_error_is_reported_after_successful_run() {
        let result = finish_target_run(Ok(()), Err(SimError::Executor("cleanup failed".into())));

        assert!(matches!(
            result,
            Err(SimError::Executor(message)) if message == "cleanup failed"
        ));
    }

    #[tokio::test]
    async fn run_target_dispatches_local_validation() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("local.json");
        tokio::fs::write(&scenario, local_manifest_bytes())
            .await
            .expect("scenario should write");

        let error = Box::pin(run_target(RunOptions {
            scenario,
            rpc_url: Some("http://127.0.0.1:9545".into()),
            runtime_manifest: None,
            groundhog_binary: None,
            hogd_dev_binary: None,
            hogd_protocol: None,
            port: None,
            startup_timeout_secs: None,
            contract_artifact_root: None,
            out_dir: None,
            seed: None,
            nonce_namespace: None,
        }))
        .await
        .expect_err("local run should reject rpc url before launch");

        assert!(matches!(
            error,
            SimError::Validation(crate::validation::ValidationError::LocalRejectsRpcUrl)
        ));
    }

    #[tokio::test]
    async fn run_target_dispatches_local_provider() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("local.json");
        tokio::fs::write(&scenario, local_manifest_bytes())
            .await
            .expect("scenario should write");
        let missing = dir.path().join("missing-groundhog");

        let error = Box::pin(run_target(RunOptions {
            scenario,
            rpc_url: None,
            runtime_manifest: None,
            groundhog_binary: Some(missing.clone()),
            hogd_dev_binary: None,
            hogd_protocol: None,
            port: None,
            startup_timeout_secs: None,
            contract_artifact_root: None,
            out_dir: None,
            seed: None,
            nonce_namespace: None,
        }))
        .await
        .expect_err("local run should use the local provider");

        assert!(matches!(
            error,
            SimError::GroundhogBinaryNotFound { binary } if binary == missing.display().to_string()
        ));
    }

    #[tokio::test]
    async fn attach_baseline_diff_none_leaves_result_unchanged() {
        let mut result = result_fixture(1);

        attach_baseline_diff(&mut result, Path::new("scenario.json"), None)
            .await
            .expect("missing baseline should be a no-op");

        assert!(result.baseline_diff.is_none());
    }

    #[tokio::test]
    async fn attach_baseline_diff_resolves_relative_path_and_preserves_manifest_path() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("scenarios").join("scenario.json");
        let baseline_path = dir
            .path()
            .join("scenarios")
            .join("baselines")
            .join("baseline-result.json");
        tokio::fs::create_dir_all(baseline_path.parent().expect("baseline should have parent"))
            .await
            .expect("baseline dir should create");
        let mut baseline = result_fixture(1);
        baseline.manifest_hashes.simulation_manifest_sha256 = "baseline-sim".into();
        write_json_pretty(baseline_path, &baseline)
            .await
            .expect("baseline should write");
        let mut current = result_fixture(3);

        attach_baseline_diff(
            &mut current,
            &scenario,
            Some(Path::new("baselines/baseline-result.json")),
        )
        .await
        .expect("baseline diff should attach");

        let baseline_diff = current
            .baseline_diff
            .as_ref()
            .expect("baseline diff should exist");
        assert_eq!(baseline_diff.path, "baselines/baseline-result.json");
        assert!(baseline_diff.diff.same_config);
        assert!(!baseline_diff.diff.simulation_manifest_match);
        assert_eq!(baseline_diff.diff.simulation_config_match, Some(true));
        assert_eq!(
            baseline_diff
                .diff
                .workload_count_deltas
                .get(&WorkloadKind::ChainTip)
                .expect("chain-tip delta should exist")
                .completed,
            2
        );
    }

    #[tokio::test]
    async fn attach_baseline_diff_accepts_run_directory_baseline() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("scenario.json");
        let run_dir = dir.path().join("baseline-run");
        tokio::fs::create_dir_all(&run_dir)
            .await
            .expect("baseline run dir should create");
        write_json_pretty(run_dir.join("result.json"), &result_fixture(2))
            .await
            .expect("baseline should write");
        let mut current = result_fixture(5);

        attach_baseline_diff(&mut current, &scenario, Some(Path::new("baseline-run")))
            .await
            .expect("run directory baseline should load");

        assert_eq!(
            current
                .baseline_diff
                .as_ref()
                .expect("baseline diff should exist")
                .diff
                .workload_count_deltas
                .get(&WorkloadKind::ChainTip)
                .expect("chain-tip delta should exist")
                .completed,
            3
        );
    }

    #[tokio::test]
    async fn attach_baseline_diff_rejects_unsupported_result_schema() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("scenario.json");
        let baseline_path = dir.path().join("baseline.json");
        let mut baseline = serde_json::to_value(result_fixture(1)).expect("result serializes");
        baseline["schemaVersion"] = json!(SIM_RESULT_SCHEMA_VERSION + 1);
        write_json_pretty(baseline_path, &baseline)
            .await
            .expect("baseline should write");
        let mut current = result_fixture(1);

        let error = attach_baseline_diff(&mut current, &scenario, Some(Path::new("baseline.json")))
            .await
            .expect_err("unsupported baseline schema should fail");

        assert!(matches!(
            error,
            SimError::UnsupportedResultSchema {
                actual,
                ..
            } if actual == SIM_RESULT_SCHEMA_VERSION + 1
        ));
    }

    #[tokio::test]
    async fn attach_baseline_diff_resolves_absolute_path_without_scenario_dir() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("scenarios").join("scenario.json");
        let baseline_path = dir.path().join("absolute-baseline.json");
        write_json_pretty(baseline_path.clone(), &result_fixture(1))
            .await
            .expect("baseline should write");
        let mut current = result_fixture(4);

        attach_baseline_diff(&mut current, &scenario, Some(&baseline_path))
            .await
            .expect("absolute baseline should load regardless of scenario dir");

        let baseline_diff = current
            .baseline_diff
            .as_ref()
            .expect("baseline diff should exist");
        assert_eq!(baseline_diff.path, baseline_path.display().to_string());
        assert_eq!(
            baseline_diff
                .diff
                .workload_count_deltas
                .get(&WorkloadKind::ChainTip)
                .expect("chain-tip delta should exist")
                .completed,
            3
        );
    }

    #[tokio::test]
    async fn attach_baseline_diff_errors_when_baseline_file_missing() {
        let dir = tempdir().expect("temp dir should be created");
        let scenario = dir.path().join("scenario.json");
        let mut current = result_fixture(1);

        let error = attach_baseline_diff(
            &mut current,
            &scenario,
            Some(Path::new("does-not-exist.json")),
        )
        .await
        .expect_err("missing baseline file should fail the run");

        assert!(matches!(error, SimError::Io(_)));
        assert!(current.baseline_diff.is_none());
    }

    #[tokio::test]
    async fn refresh_result_target_ownership_uses_latest_artifact() {
        let dir = tempdir().expect("temp dir should be created");
        let out_dir = dir.path().join("run");
        tokio::fs::create_dir_all(&out_dir)
            .await
            .expect("run dir should create");
        let latest = TargetOwnership {
            provider: TargetProviderKind::Local,
            started_by_sim: true,
            cleanup_allowed: true,
            process: Some(crate::target::OwnedProcess {
                pid: 42,
                pgid: 42,
                port: 9545,
                start_time: "Wed May 27 12:00:00 2026".into(),
                command: "groundhog node".into(),
            }),
        };
        write_json_pretty(out_dir.join("target-ownership.json"), &latest)
            .await
            .expect("ownership should write");
        let mut result = result_fixture(1);

        refresh_result_target_ownership(&mut result, &out_dir)
            .await
            .expect("ownership should refresh");

        assert_eq!(result.target_ownership, latest);
    }

    fn manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": crate::manifest::SIMULATION_SCHEMA_VERSION,
            "kind": "groundhog-simulation",
            "name": "orchestration-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest fixture should parse")
    }

    fn chaos_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "orchestration-chaos-test",
            "seed": "seed",
            "target": {"provider": "local"},
            "loadShape": [{"duration": "2ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}],
            "chaos": [{"at": "1ms", "kind": "restart-target"}]
        }))
        .expect("manifest fixture should parse")
    }

    fn local_manifest_bytes() -> Vec<u8> {
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": crate::manifest::SIMULATION_SCHEMA_VERSION,
            "kind": "groundhog-simulation",
            "name": "local-test",
            "seed": "seed",
            "target": {"provider": "local"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest should encode")
    }

    fn resolved_target() -> ResolvedTarget {
        ResolvedTarget {
            provider: crate::target::TargetProviderKind::Existing,
            capabilities: crate::target::TargetCapabilities::for_provider(
                crate::target::TargetProviderKind::Existing,
            ),
            ownership: TargetOwnership::existing(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: PathBuf::from("/tmp/runtime-manifest.json"),
        }
    }

    fn empty_contract_setup() -> ContractSetup {
        ContractSetup {
            records: Vec::new(),
            view_calls: Vec::new(),
            contract_calls: Vec::new(),
            storage_reads: Vec::new(),
            event_queries: Vec::new(),
            read_after_write: Vec::new(),
            failed: false,
        }
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x42),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000"
            },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": "groundhog-dev-accounts"
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x51),
                    "deployedCodeHash": prefixed_hex(0x52),
                    "artifactHint": "./dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
                "storageCostModel": {
                    "hash": prefixed_hex(0x53),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234"
                }
            }
        }))
        .expect("runtime manifest fixture should parse")
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }

    fn actor_states() -> Vec<ActorState> {
        vec![ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("run-test-seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x66; 32])).0,
            },
            NonceNamespace::new(7).expect("namespace should fit"),
        )]
    }

    fn result_fixture(completed: u64) -> SimResult {
        SimResult {
            schema_version: SIM_RESULT_SCHEMA_VERSION,
            run_id: "result-fixture-run".into(),
            name: "result-fixture".into(),
            outcome: SimRunOutcome::Passed,
            target_provider: TargetProviderKind::Existing,
            target_ownership: TargetOwnership::existing(),
            target_identity: TargetIdentity {
                rpc_url: Some("http://127.0.0.1:9545".into()),
            },
            runtime_identity: RuntimeIdentity {
                network_hash: "00".repeat(32),
                protocol: "hogp/test".into(),
                genesis_hash: prefixed_hex(0x42),
                producer: ProducerIdentity {
                    binary: "hog-devnode".into(),
                    version: "0.1.0".into(),
                    commit_hash: None,
                },
            },
            nonce_namespace: 1,
            manifest_hashes: ManifestHashes {
                simulation_manifest_sha256: "sim".into(),
                simulation_config_sha256: Some("sim-config".into()),
                runtime_manifest_sha256: "runtime".into(),
                runtime_config_sha256: Some("runtime-config".into()),
                bootstrap_manifest: MetricAvailability::Unsupported {
                    reason: "existing target".into(),
                },
            },
            metric_availability: SimMetricAvailability::phase_one_defaults(),
            workload_counts: BTreeMap::from([(
                WorkloadKind::ChainTip,
                WorkloadCounts {
                    completed,
                    failed: 0,
                    skipped: 0,
                },
            )]),
            workload_metrics: BTreeMap::new(),
            backpressure_rate: 0.0,
            simulator_overload_rate: 0.0,
            simulator_overload_count: 0,
            diagnostics: crate::result::RunDiagnostics::default(),
            resource_timeline: MetricAvailability::Unavailable {
                reason: "test fixture".into(),
            },
            chain_progress: None,
            assertion_results: Vec::new(),
            chaos_events: Vec::new(),
            contracts: Vec::new(),
            baseline_diff: None,
        }
    }
}
