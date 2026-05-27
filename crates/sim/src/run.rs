use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use groundhog_rpc_api_types::runtime_manifest::RuntimeManifest;
use groundhog_rpc_client::HttpClientBuilder;

use crate::actor::{ActorState, build_actor_states};
use crate::assertions::{determine_outcome, evaluate_assertions};
use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent};
use crate::executor::{
    DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL, DEFAULT_RECEIPT_DRAIN_TIMEOUT, ScheduleExecution,
    SimRpcClient, drain_pending_receipt_polls, run_schedule,
};
use crate::manifest::{ResolvedRunSpec, SimulationManifest, WorkloadKind};
use crate::metrics::{RunMetrics, RunMetricsBuilder};
use crate::nonce::NonceNamespace;
use crate::result::{
    AssertionResult, ManifestHashes, MetricAvailability, ProducerIdentity, RuntimeIdentity,
    SIM_RESULT_SCHEMA_VERSION, SimMetricAvailability, SimResult, SimRunOutcome, TargetIdentity,
    WorkloadCounts,
};
use crate::runtime::{derive_accounts, load_runtime_manifest};
use crate::scheduler::{PhaseStart, ScheduledSimulationEvent, generate_schedule, phase_starts};
use crate::target::{
    ExistingTargetProvider, LocalTargetProvider, ResolvedTarget, TargetOverrides, TargetOwnership,
    TargetProvider, TargetProviderKind,
};
use crate::util::{
    LoadedSimulationManifest, default_run_dir, json_string, load_simulation_manifest, run_id,
    workload_name, write_json_pretty, write_json_pretty_atomic,
};
use crate::validation::{validate_against_runtime, validate_manifest};

/// Options for running a simulation.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub scenario: PathBuf,
    pub rpc_url: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
    pub groundhog_binary: Option<PathBuf>,
    pub hogd_dev_binary: Option<PathBuf>,
    pub port: Option<u16>,
    pub startup_timeout_secs: Option<u64>,
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
            port: None,
            startup_timeout_secs: None,
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
        port,
        startup_timeout_secs,
        out_dir,
        seed,
        nonce_namespace,
    } = args;
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
        port,
        startup_timeout_secs,
    };
    match loaded_sim.manifest.target.provider {
        TargetProviderKind::Existing => {
            let provider = ExistingTargetProvider;
            run_with_provider(loaded_sim, &provider, overrides, out_dir, nonce_namespace).await
        }
        TargetProviderKind::Local => {
            let provider = LocalTargetProvider::new();
            run_with_provider(loaded_sim, &provider, overrides, out_dir, nonce_namespace).await
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
    pub out_dir: PathBuf,
    pub outcome: SimRunOutcome,
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
    let mut loaded_sim = load_simulation_manifest(&scenario).await?;
    if let Some(seed) = seed {
        loaded_sim.manifest.seed = seed;
    }
    validate_manifest(&loaded_sim.manifest)?;
    let provider = ExistingTargetProvider;
    run_with_provider(
        loaded_sim,
        &provider,
        TargetOverrides {
            rpc_url,
            runtime_manifest,
            ..TargetOverrides::default()
        },
        out_dir,
        nonce_namespace,
    )
    .await
}

async fn run_with_provider(
    loaded_sim: LoadedSimulationManifest,
    provider: &dyn TargetProvider,
    overrides: TargetOverrides,
    out_dir: Option<PathBuf>,
    nonce_namespace: Option<u64>,
) -> Result<RunSummary, SimError> {
    let target = provider.acquire(&loaded_sim.manifest, overrides).await?;
    let run_result = run_resolved_target(loaded_sim, &target, out_dir, nonce_namespace).await;
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

#[allow(clippy::too_many_lines)]
async fn run_resolved_target(
    mut loaded_sim: LoadedSimulationManifest,
    target: &ResolvedTarget,
    out_dir: Option<PathBuf>,
    nonce_namespace: Option<u64>,
) -> Result<RunSummary, SimError> {
    loaded_sim.manifest.target.rpc_url = Some(target.rpc_url.clone());
    loaded_sim.manifest.target.runtime_manifest = Some(target.runtime_manifest.clone());

    let loaded_runtime = load_runtime_manifest(&target.runtime_manifest).await?;
    let accounts = derive_accounts(&loaded_runtime.manifest)?;
    validate_executable_workloads(&loaded_sim.manifest)?;
    validate_against_runtime(&loaded_sim.manifest, &loaded_runtime.manifest)?;

    let run_id = run_id()?;
    let nonce_namespace = match nonce_namespace {
        Some(value) => NonceNamespace::new(value)?,
        None => NonceNamespace::generate(
            &loaded_sim.manifest.seed,
            &loaded_sim.manifest.name,
            &run_id,
        ),
    };
    let mut actor_states = build_actor_states(&loaded_sim.manifest, accounts, nonce_namespace)?;
    loaded_sim.manifest.resolved = Some(ResolvedRunSpec {
        rpc_url: target.rpc_url.clone(),
        runtime_manifest: target.runtime_manifest.clone(),
        nonce_namespace: nonce_namespace.get(),
    });
    loaded_sim.refresh_sha256()?;
    let schedule = generate_schedule(&loaded_sim.manifest)?;
    let phase_starts = phase_starts(&loaded_sim.manifest)?;
    let out_dir = out_dir.unwrap_or_else(|| default_run_dir(&run_id, &loaded_sim.manifest.name));

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
        actor_states.len(),
        nonce_namespace,
        &loaded_runtime.sha256,
    )
    .await?;

    let client = HttpClientBuilder::default()
        .build(target.rpc_url.clone())
        .map_err(|error| SimError::RpcClient(error.to_string()))?;
    let scheduled_workloads = schedule
        .iter()
        .filter(|event| matches!(event, ScheduledSimulationEvent::ActorTick(_)))
        .count() as u64;
    let result = execute_loaded_simulation(
        client,
        LoadedExecution {
            run_id: &run_id,
            loaded_sim: &loaded_sim,
            runtime_manifest: &loaded_runtime.manifest,
            runtime_manifest_sha256: &loaded_runtime.sha256,
            target,
            nonce_namespace,
            actor_states: &mut actor_states,
            scheduled_workloads,
            schedule: &schedule,
            phase_starts: &phase_starts,
            event_writer: &mut event_writer,
        },
    )
    .await?;
    let outcome = result.outcome;
    write_json_pretty_atomic(out_dir.join("result.json"), &result).await?;

    let summary = RunSummary { out_dir, outcome };
    if outcome == SimRunOutcome::Failed {
        return Err(SimError::RunFailed(summary.out_dir.display().to_string()));
    }
    Ok(summary)
}

/// Inputs for executing an already-loaded simulation.
#[derive(Debug)]
pub struct LoadedExecution<'a> {
    pub run_id: &'a str,
    pub loaded_sim: &'a LoadedSimulationManifest,
    pub runtime_manifest: &'a RuntimeManifest,
    pub runtime_manifest_sha256: &'a str,
    pub target: &'a ResolvedTarget,
    pub nonce_namespace: NonceNamespace,
    pub actor_states: &'a mut [ActorState],
    pub scheduled_workloads: u64,
    pub schedule: &'a [ScheduledSimulationEvent],
    pub phase_starts: &'a [PhaseStart],
    pub event_writer: &'a mut EventWriter,
}

/// Execute a loaded simulation against a supplied client.
pub async fn execute_loaded_simulation<C>(
    client: C,
    input: LoadedExecution<'_>,
) -> Result<SimResult, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let mut metrics = RunMetricsBuilder::default();
    run_schedule(
        client.clone(),
        ScheduleExecution {
            manifest: &input.loaded_sim.manifest,
            runtime_manifest: input.runtime_manifest,
            schedule: input.schedule,
            phase_starts: input.phase_starts,
            actor_states: input.actor_states,
            event_writer: &mut *input.event_writer,
            metrics: &mut metrics,
        },
    )
    .await?;
    let shutdown_simulation_time_ms = input
        .schedule
        .iter()
        .map(ScheduledSimulationEvent::simulation_time_ms)
        .max()
        .unwrap_or_default();
    let reconciled_receipt_polls = drain_pending_receipt_polls(
        client,
        input.actor_states,
        &mut *input.event_writer,
        &mut metrics,
        shutdown_simulation_time_ms,
        DEFAULT_RECEIPT_DRAIN_TIMEOUT,
        DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL,
    )
    .await?;
    let completed_workloads = metrics.completed_count();
    let failed_workloads = metrics.failed_count();
    let skipped_workloads = metrics.skipped_count();
    let workload_counts = metrics.workload_counts();
    let pending_receipts = input
        .actor_states
        .iter()
        .map(ActorState::pending_receipt_count)
        .sum();
    metrics.record_pending_receipts_at_shutdown(pending_receipts);
    let finished_metrics = metrics.finish();
    let metric_availability = SimMetricAvailability::phase_one_defaults();
    let assertion_results = evaluate_assertions(
        &input.loaded_sim.manifest,
        completed_workloads,
        failed_workloads,
        finished_metrics.chain_progress.as_ref(),
        &finished_metrics.workload_metrics,
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
    let outcome = determine_outcome(&assertion_results, failed_workloads);
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
        target: input.target,
        outcome,
        nonce_namespace: input.nonce_namespace,
        workload_counts,
        finished_metrics,
        metric_availability,
        assertion_results,
    })
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
    target: &'a ResolvedTarget,
    outcome: SimRunOutcome,
    nonce_namespace: NonceNamespace,
    workload_counts: BTreeMap<WorkloadKind, WorkloadCounts>,
    finished_metrics: RunMetrics,
    metric_availability: SimMetricAvailability,
    assertion_results: Vec<AssertionResult>,
}

fn build_result(input: BuildResultInput<'_>) -> Result<SimResult, SimError> {
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
            network_id: input.runtime_manifest.network_id.clone(),
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
            runtime_manifest_sha256: input.runtime_manifest_sha256.into(),
            bootstrap_manifest: MetricAvailability::Unsupported {
                reason: "existing target cannot expose simulator launcher input".into(),
            },
        },
        metric_availability: input.metric_availability,
        workload_counts: input.workload_counts,
        workload_metrics: input.finished_metrics.workload_metrics,
        chain_progress: input.finished_metrics.chain_progress,
        assertion_results: input.assertion_results,
        chaos_events: Vec::new(),
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
    use groundhog_rpc_api_types::{BlockHeight, block::ChainTip, hex::Hex0x, transaction::TxHash};
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};
    use serde_json::json;
    use tempfile::{NamedTempFile, tempdir};

    use super::*;
    use crate::event::EventWriter;
    use crate::executor::SimRpcClient;
    use crate::runtime::SimAccount;
    use crate::scheduler::{generate_schedule, phase_starts};
    use crate::util::sha256_hex;

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

    #[tokio::test]
    async fn execute_loaded_simulation_records_chain_tip_result() {
        let manifest = manifest();
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest should encode");
        let loaded_sim = LoadedSimulationManifest {
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
                target: &target,
                nonce_namespace: NonceNamespace::new(7).expect("nonce namespace should fit"),
                actor_states: &mut actors,
                scheduled_workloads: 1,
                schedule: &schedule,
                phase_starts: &starts,
                event_writer: &mut event_writer,
            },
        )
        .await
        .expect("simulation should execute");

        assert_eq!(result.outcome, SimRunOutcome::Passed);
        assert_eq!(result.run_id, "test-run");
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

        let error = run_target(RunOptions {
            scenario,
            rpc_url: Some("http://127.0.0.1:9545".into()),
            runtime_manifest: None,
            groundhog_binary: None,
            hogd_dev_binary: None,
            port: None,
            startup_timeout_secs: None,
            out_dir: None,
            seed: None,
            nonce_namespace: None,
        })
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

        let error = run_target(RunOptions {
            scenario,
            rpc_url: None,
            runtime_manifest: None,
            groundhog_binary: Some(missing.clone()),
            hogd_dev_binary: None,
            port: None,
            startup_timeout_secs: None,
            out_dir: None,
            seed: None,
            nonce_namespace: None,
        })
        .await
        .expect_err("local run should use the local provider");

        assert!(matches!(
            error,
            SimError::GroundhogBinaryNotFound { binary } if binary == missing.display().to_string()
        ));
    }

    fn manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "orchestration-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest fixture should parse")
    }

    fn local_manifest_bytes() -> Vec<u8> {
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
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

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x42),
            "producer": {
                "binary": "hogd-dev",
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
}
