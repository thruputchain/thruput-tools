use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use groundhog_rpc_api_types::runtime_manifest::RuntimeManifest;
use groundhog_rpc_client::HttpClientBuilder;

use crate::actor::{ActorState, build_actor_states};
use crate::assertions::{determine_outcome, evaluate_assertions};
use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent};
use crate::executor::{SimRpcClient, run_schedule};
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
use crate::target::{TargetOwnership, TargetProviderKind};
use crate::util::{
    LoadedSimulationManifest, default_run_dir, json_string, load_simulation_manifest, run_id,
    workload_name, write_json_pretty, write_json_pretty_atomic,
};
use crate::validation::{
    validate_against_runtime, validate_existing_target_flags, validate_manifest,
};

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

/// Summary of a completed simulation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub out_dir: PathBuf,
    pub outcome: SimRunOutcome,
}

/// Run a simulation against an already-started target.
#[allow(clippy::too_many_lines)]
pub async fn run_existing_target(args: ExistingRunOptions) -> Result<RunSummary, SimError> {
    let mut loaded_sim = load_simulation_manifest(&args.scenario).await?;
    if let Some(seed) = args.seed {
        loaded_sim.manifest.seed = seed;
    }
    validate_manifest(&loaded_sim.manifest)?;
    validate_existing_target_flags(
        &loaded_sim.manifest,
        args.rpc_url.as_deref(),
        args.runtime_manifest.is_some(),
    )?;

    let runtime_manifest_path = args
        .runtime_manifest
        .clone()
        .or_else(|| loaded_sim.manifest.target.runtime_manifest.clone())
        .ok_or(SimError::MissingRuntimeManifest)?;
    let rpc_url = args
        .rpc_url
        .clone()
        .or_else(|| loaded_sim.manifest.target.rpc_url.clone())
        .ok_or(SimError::MissingRpcUrl)?;
    loaded_sim.manifest.target.rpc_url = Some(rpc_url.clone());
    loaded_sim.manifest.target.runtime_manifest = Some(runtime_manifest_path.clone());

    let loaded_runtime = load_runtime_manifest(&runtime_manifest_path).await?;
    let accounts = derive_accounts(&loaded_runtime.manifest)?;
    validate_executable_workloads(&loaded_sim.manifest)?;
    validate_against_runtime(&loaded_sim.manifest, &loaded_runtime.manifest)?;

    let run_id = run_id()?;
    let nonce_namespace = match args.nonce_namespace {
        Some(value) => NonceNamespace::new(value)?,
        None => NonceNamespace::generate(
            &loaded_sim.manifest.seed,
            &loaded_sim.manifest.name,
            &run_id,
        ),
    };
    let mut actor_states = build_actor_states(&loaded_sim.manifest, accounts, nonce_namespace)?;
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
        actor_states.len(),
        nonce_namespace,
        &loaded_runtime.sha256,
    )
    .await?;

    let client = HttpClientBuilder::default()
        .build(rpc_url.clone())
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
            rpc_url: &rpc_url,
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
    pub rpc_url: &'a str,
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
        client,
        &input.loaded_sim.manifest,
        input.schedule,
        input.phase_starts,
        input.actor_states,
        &mut *input.event_writer,
        &mut metrics,
    )
    .await?;
    let completed_workloads = metrics.completed_count();
    let failed_workloads = metrics.failed_count();
    let skipped_workloads = metrics.skipped_count();
    let workload_counts = metrics.workload_counts();
    let finished_metrics = metrics.finish();
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
            .write(&ResultEvent::AssertionChecked(assertion.clone()))
            .await?;
    }
    let outcome = determine_outcome(&assertion_results, failed_workloads);
    input
        .event_writer
        .write(&ResultEvent::RunCompleted {
            outcome,
            scheduled_workloads: input.scheduled_workloads,
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
        rpc_url: input.rpc_url,
        outcome,
        nonce_namespace: input.nonce_namespace,
        workload_counts,
        finished_metrics,
        assertion_results,
    })
}

async fn prepare_result_directory(
    out_dir: &Path,
    simulation_manifest_bytes: &[u8],
    runtime_manifest_bytes: &[u8],
) -> Result<EventWriter, SimError> {
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
) -> Result<(), SimError> {
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
    run_id: &'a str,
    runtime_manifest: &'a RuntimeManifest,
    runtime_manifest_sha256: &'a str,
    rpc_url: &'a str,
    outcome: SimRunOutcome,
    nonce_namespace: NonceNamespace,
    workload_counts: BTreeMap<WorkloadKind, WorkloadCounts>,
    finished_metrics: RunMetrics,
    assertion_results: Vec<AssertionResult>,
}

fn build_result(input: BuildResultInput<'_>) -> Result<SimResult, SimError> {
    Ok(SimResult {
        schema_version: SIM_RESULT_SCHEMA_VERSION,
        run_id: input.run_id.into(),
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
        metric_availability: SimMetricAvailability::phase_one_defaults(),
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
        .filter(|workload| workload.name != WorkloadKind::ChainTip)
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
    use groundhog_rpc_api_types::{BlockHeight, block::ChainTip, hex::Hex0x};
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
                rpc_url: "http://127.0.0.1:9545",
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
        assert!(events.contains("\"kind\":\"run-completed\""));
    }

    #[tokio::test]
    async fn prepare_result_directory_writes_existing_target_ownership() {
        let dir = tempdir().expect("temp dir should be created");
        let out_dir = dir.path().join("run");
        let mut writer =
            prepare_result_directory(&out_dir, b"{\"simulation\":true}", b"{\"runtime\":true}")
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
