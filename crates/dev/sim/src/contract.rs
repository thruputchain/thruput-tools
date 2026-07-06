use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use groundhog_json_rpc_types::{
    hex::HexBytes,
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    transaction::TxHash,
};
use groundhog_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams, build_deploy_transaction,
    derive_deploy_address, sign_and_encode_transaction_hex, source_address_auth_claim,
};
use groundhog_xdr::{
    contract::{Contract, ContractBlob, ContractEntrypointID},
    types::{ContractAddress, SHA256Hash},
};
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

use crate::actor::ActorState;
use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent};
use crate::executor::{
    DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL, DEFAULT_RECEIPT_DRAIN_TIMEOUT, SimRpcClient,
};
use crate::manifest::{
    ContractArtifactKind, ContractArtifactSpec, EventQueryAnchorSpec, EventQueryOrderSpec,
    SimulationManifest,
};
use crate::result::{ContractDeploymentRecord, ContractDeploymentStatus, DeployAddressSource};
use crate::util::{json_string, sha256_hex};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;

#[derive(Debug, Clone)]
pub struct ContractSetup {
    pub records: Vec<ContractDeploymentRecord>,
    pub view_calls: Vec<PreparedViewCall>,
    pub contract_calls: Vec<PreparedContractCall>,
    pub storage_reads: Vec<PreparedStorageRead>,
    pub event_queries: Vec<PreparedEventQuery>,
    pub read_after_write: Vec<PreparedReadAfterWrite>,
    pub failed: bool,
}

/// A manifest read-after-write target resolved to a deployed contract address.
///
/// The executor wraps the write in the scheduled actor's dev-wallet `doAction`
/// transaction and issues the read as a `groundhog_view` call, so this carries
/// only the inner write/read entrypoints and calldata plus the deployed address.
#[derive(Debug, Clone)]
pub struct PreparedReadAfterWrite {
    pub name: String,
    pub contract_name: String,
    pub weight: u32,
    pub contract: ContractAddress,
    pub write_entrypoint: Vec<u8>,
    pub write_calldata: Vec<u8>,
    pub read_entrypoint: Vec<u8>,
    pub read_calldata: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PreparedViewCall {
    pub name: String,
    pub contract_name: String,
    pub weight: u32,
    pub contract: ContractAddress,
    pub entrypoint: Vec<u8>,
    pub calldata: Vec<u8>,
    pub expect_return_data: Option<Vec<u8>>,
}

/// A manifest contract-call target resolved to a deployed contract address.
///
/// The executor wraps this target in the scheduled actor's dev-wallet
/// `doAction` transaction, so the prepared value contains only the inner
/// contract call details.
#[derive(Debug, Clone)]
pub struct PreparedContractCall {
    /// Manifest-local contract-call name.
    pub name: String,
    /// Manifest-local contract artifact name.
    pub contract_name: String,
    /// Relative selection weight among configured contract-call targets.
    pub weight: u32,
    /// Deployed contract address resolved during startup setup.
    pub contract: ContractAddress,
    /// Raw target contract entrypoint selector bytes.
    pub entrypoint: Vec<u8>,
    /// Raw target contract calldata bytes.
    pub calldata: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PreparedStorageRead {
    pub name: String,
    pub contract_name: String,
    pub weight: u32,
    pub contract: ContractAddress,
    pub key: Vec<u8>,
    pub expect_present: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct PreparedEventQuery {
    pub name: String,
    pub contract_name: String,
    pub weight: u32,
    pub contract: ContractAddress,
    pub order: EventQueryOrderSpec,
    pub anchor: EventQueryAnchorSpec,
    pub cursor: Vec<u8>,
    pub limit: u32,
    pub expect_min_events: Option<u64>,
}

pub async fn deploy_manifest_contracts<C>(
    client: C,
    manifest: &SimulationManifest,
    runtime_manifest: &RuntimeManifest,
    scenario_path: &Path,
    contract_artifact_root: Option<&Path>,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
) -> Result<ContractSetup, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    if manifest.contract_artifacts.is_empty() {
        return Ok(ContractSetup {
            records: Vec::new(),
            view_calls: Vec::new(),
            contract_calls: Vec::new(),
            storage_reads: Vec::new(),
            event_queries: Vec::new(),
            read_after_write: Vec::new(),
            failed: false,
        });
    }
    let actor = actor_states.get_mut(0).ok_or(SimError::Executor(
        "contract deploy requires actor 0".into(),
    ))?;
    let deploy_entrypoint = runtime_manifest
        .bootstrap
        .dev_wallet
        .deploy_entrypoint_id()
        .map_err(|source| {
            SimError::Executor(format!(
                "failed to parse runtime manifest bootstrap.devWallet.deployEntrypoint: {source}"
            ))
        })?;
    let mut records = Vec::new();
    let mut deployed = BTreeMap::new();
    let mut failed = false;

    for artifact in &manifest.contract_artifacts {
        let block_expiry_height = resolve_block_expiry_height(client.clone()).await?;
        let record = deploy_one_contract(
            client.clone(),
            artifact,
            scenario_path,
            contract_artifact_root,
            actor,
            deploy_entrypoint,
            event_writer,
            block_expiry_height,
        )
        .await?;
        if record.status == ContractDeploymentStatus::Completed {
            if let Some(address) = record.address.as_deref() {
                deployed.insert(artifact.name.clone(), parse_contract_address(address)?);
            }
        } else {
            failed = true;
        }
        records.push(record);
        if failed {
            break;
        }
    }

    let (view_calls, contract_calls, storage_reads, event_queries, read_after_write) = if failed {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
    } else {
        (
            prepare_view_calls(manifest, &deployed)?,
            prepare_contract_calls(manifest, &deployed)?,
            prepare_storage_reads(manifest, &deployed)?,
            prepare_event_queries(manifest, &deployed)?,
            prepare_read_after_write(manifest, &deployed)?,
        )
    };

    Ok(ContractSetup {
        records,
        view_calls,
        contract_calls,
        storage_reads,
        event_queries,
        read_after_write,
        failed,
    })
}

async fn deploy_one_contract<C>(
    client: C,
    artifact: &ContractArtifactSpec,
    scenario_path: &Path,
    contract_artifact_root: Option<&Path>,
    actor: &mut ActorState,
    deploy_entrypoint: ContractEntrypointID,
    event_writer: &mut EventWriter,
    block_expiry_height: i64,
) -> Result<ContractDeploymentRecord, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let artifact_path = resolve_artifact_path(
        scenario_path,
        contract_artifact_root,
        &artifact.artifact.path,
    );
    let mut record = ContractDeploymentRecord {
        name: artifact.name.clone(),
        status: ContractDeploymentStatus::Started,
        artifact_kind: Some(artifact.artifact.kind),
        artifact_path: Some(artifact_path.display().to_string()),
        artifact_sha256: None,
        contract_hash: None,
        nonce: None,
        deploy_address_nonce: None,
        tx_hash: None,
        address: None,
        address_source: None,
        height: None,
        error: None,
    };

    let prepared = match prepare_deploy(
        artifact.artifact.kind,
        &artifact_path,
        actor,
        deploy_entrypoint,
        block_expiry_height,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            record.status = ContractDeploymentStatus::Failed;
            record.error = Some(error.to_string());
            event_writer
                .write(&ResultEvent::ContractDeployFailed {
                    event: record.clone(),
                })
                .await?;
            return Ok(record);
        }
    };

    record.artifact_sha256 = Some(prepared.artifact_sha256.clone());
    record.contract_hash = Some(prepared.contract_hash.clone());
    record.nonce = Some(prepared.tx_nonce.to_string());
    record.deploy_address_nonce = Some(prepared.deploy_address_nonce.clone());
    event_writer
        .write(&ResultEvent::ContractDeployStarted {
            event: record.clone(),
        })
        .await?;

    let completed = submit_and_resolve_deploy(client, prepared, actor).await;
    apply_deploy_progress(&mut record, &completed.progress);
    match completed.result {
        Ok(()) => {
            record.status = ContractDeploymentStatus::Completed;
            event_writer
                .write(&ResultEvent::ContractDeployCompleted {
                    event: record.clone(),
                })
                .await?;
        }
        Err(error) => {
            record.status = ContractDeploymentStatus::Failed;
            record.error = Some(error);
            event_writer
                .write(&ResultEvent::ContractDeployFailed {
                    event: record.clone(),
                })
                .await?;
        }
    }
    Ok(record)
}

fn apply_deploy_progress(record: &mut ContractDeploymentRecord, progress: &DeployProgress) {
    if let Some(tx_hash) = &progress.tx_hash {
        record.tx_hash = Some(tx_hash.clone());
    }
    if let Some(height) = progress.height {
        record.height = Some(height);
    }
    if let Some(address) = &progress.address {
        record.address = Some(address.clone());
    }
    if let Some(source) = progress.address_source {
        record.address_source = Some(source);
    }
}

fn resolve_artifact_path(
    scenario_path: &Path,
    contract_artifact_root: Option<&Path>,
    artifact: &Path,
) -> PathBuf {
    if artifact.is_absolute() {
        return artifact.to_path_buf();
    }
    if let Some(root) = contract_artifact_root {
        return root.join(artifact);
    }
    scenario_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(artifact)
}

struct PreparedDeploy {
    tx_hex: String,
    tx_nonce: u64,
    predicted_address: ContractAddress,
    artifact_sha256: String,
    contract_hash: String,
    deploy_address_nonce: String,
}

fn prepare_deploy(
    artifact_kind: ContractArtifactKind,
    artifact_path: &Path,
    actor: &ActorState,
    deploy_entrypoint: ContractEntrypointID,
    block_expiry_height: i64,
) -> Result<PreparedDeploy, SimError> {
    let artifact_bytes = std::fs::read(artifact_path)?;
    let artifact_sha256 = sha256_hex(&artifact_bytes);
    let blob = ContractBlob {
        data: BytesM(artifact_bytes),
    };
    let contract = match artifact_kind {
        ContractArtifactKind::Wasm => Contract::kWasmi {
            wasmi_contract: blob,
        },
        ContractArtifactKind::Rv32 => Contract::kRv32 {
            rv32_contract: blob,
        },
    };
    let contract_xdr = xdrs::txs::encode(&contract)
        .map_err(|error| SimError::Executor(format!("failed to encode contract XDR: {error}")))?;
    let contract_hash_bytes: [u8; 32] = Sha256::digest(&contract_xdr).into();
    let contract_hash = SHA256Hash::from(contract_hash_bytes);
    let tx_nonce = actor.current_nonce()?;
    let deploy_address_nonce_bytes: [u8; 32] = Sha256::digest(tx_nonce.to_le_bytes()).into();
    let deploy_address_nonce = SHA256Hash::from(deploy_address_nonce_bytes);
    let predicted_address =
        derive_deploy_address(&actor.wallet_address, &deploy_address_nonce, &contract_hash)
            .map_err(|source| SimError::Executor(source.to_string()))?;
    let calldata =
        xdrs::cxs::encode(&(&deploy_address_nonce, &contract_hash)).map_err(|error| {
            SimError::Executor(format!("failed to encode deploy calldata: {error}"))
        })?;
    let tx = build_deploy_transaction(BuildDeployTransaction {
        invoked_address: actor.wallet_address,
        entrypoint: deploy_entrypoint,
        calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce: tx_nonce,
        contracts_to_deploy: VecM(vec![contract]),
        auth_claim: source_address_auth_claim(),
    });
    let tx_hex =
        sign_and_encode_transaction_hex(tx, &actor.signer, SignTransactionParams::default())
            .map_err(|source| SimError::Executor(source.to_string()))?;

    Ok(PreparedDeploy {
        tx_hex,
        tx_nonce,
        predicted_address,
        artifact_sha256,
        contract_hash: format!("0x{}", hex::encode(contract_hash.as_ref())),
        deploy_address_nonce: format!("0x{}", hex::encode(deploy_address_nonce.as_ref())),
    })
}

async fn resolve_block_expiry_height<C>(client: C) -> Result<i64, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let tip = client.chain_tip().await.map_err(|error| {
        SimError::Executor(format!("contract deploy chain tip failed: {error}"))
    })?;
    Ok((*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET))
}

struct DeployProgress {
    tx_hash: Option<String>,
    height: Option<i64>,
    address: Option<String>,
    address_source: Option<DeployAddressSource>,
}

struct DeployAttempt {
    progress: DeployProgress,
    result: Result<(), String>,
}

async fn submit_and_resolve_deploy<C>(
    client: C,
    prepared: PreparedDeploy,
    actor: &mut ActorState,
) -> DeployAttempt
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let mut progress = DeployProgress {
        tx_hash: None,
        height: None,
        address: None,
        address_source: None,
    };
    let tx_hash = match client.send_raw_transaction(prepared.tx_hex).await {
        Ok(tx_hash) => tx_hash,
        Err(error) => {
            return DeployAttempt {
                progress,
                result: Err(format!("contract deploy submit failed: {error}")),
            };
        }
    };
    if let Err(error) = actor.consume_setup_nonce() {
        return DeployAttempt {
            progress,
            result: Err(error.to_string()),
        };
    }
    progress.tx_hash = match json_string(&tx_hash, "txHash") {
        Ok(tx_hash) => Some(tx_hash),
        Err(error) => {
            return DeployAttempt {
                progress,
                result: Err(error.to_string()),
            };
        }
    };
    let receipt = match poll_receipt(client, tx_hash).await {
        Ok(receipt) => receipt,
        Err(error) => {
            return DeployAttempt {
                progress,
                result: Err(error.to_string()),
            };
        }
    };
    progress.height = Some(*receipt.height);
    if !matches!(receipt.status, ExecStatus::Ok) {
        return DeployAttempt {
            progress,
            result: Err(format!(
                "contract deploy receipt status {}",
                exec_status_name(&receipt.status)
            )),
        };
    }
    let (address, address_source) =
        match resolve_deployed_address(receipt.return_data.as_ref(), &prepared.predicted_address) {
            Ok(resolved) => resolved,
            Err(error) => {
                return DeployAttempt {
                    progress,
                    result: Err(error.to_string()),
                };
            }
        };
    progress.address = Some(address);
    progress.address_source = Some(address_source);

    DeployAttempt {
        progress,
        result: Ok(()),
    }
}

async fn poll_receipt<C>(client: C, tx_hash: TxHash) -> Result<TransactionReceipt, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let deadline = Instant::now()
        .checked_add(DEFAULT_RECEIPT_DRAIN_TIMEOUT)
        .unwrap_or_else(Instant::now);
    loop {
        if let Some(receipt) = client.transaction_receipt(tx_hash).await.map_err(|error| {
            SimError::Executor(format!("contract deploy receipt poll failed: {error}"))
        })? {
            return Ok(receipt);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SimError::Executor(
                "contract deploy receipt timed out".into(),
            ));
        }
        tokio::time::sleep(DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL.min(remaining)).await;
    }
}

fn resolve_deployed_address(
    return_data: Option<&HexBytes>,
    predicted_address: &ContractAddress,
) -> Result<(String, DeployAddressSource), SimError> {
    if let Some(return_data) = return_data {
        if return_data.0.len() != 32 {
            return Err(SimError::Executor(format!(
                "deploy receipt return_data had {} bytes, expected 32-byte contract address",
                return_data.0.len()
            )));
        }
        return Ok((
            format!("0x{}", hex::encode(&return_data.0)),
            DeployAddressSource::Receipt,
        ));
    }
    Ok((
        format!("0x{}", hex::encode(predicted_address.as_ref())),
        DeployAddressSource::Derived,
    ))
}

fn prepare_view_calls(
    manifest: &SimulationManifest,
    deployed: &BTreeMap<String, ContractAddress>,
) -> Result<Vec<PreparedViewCall>, SimError> {
    manifest
        .view_calls
        .iter()
        .map(|view| {
            let contract = deployed.get(&view.contract).copied().ok_or_else(|| {
                SimError::Executor(format!(
                    "view call `{}` references undeployed contract `{}`",
                    view.name, view.contract
                ))
            })?;
            Ok(PreparedViewCall {
                name: view.name.clone(),
                contract_name: view.contract.clone(),
                weight: view.weight,
                contract,
                entrypoint: parse_hex(&view.entrypoint)?,
                calldata: parse_hex(&view.calldata)?,
                expect_return_data: view
                    .expect_return_data
                    .as_deref()
                    .map(parse_hex)
                    .transpose()?,
            })
        })
        .collect()
}

fn prepare_storage_reads(
    manifest: &SimulationManifest,
    deployed: &BTreeMap<String, ContractAddress>,
) -> Result<Vec<PreparedStorageRead>, SimError> {
    manifest
        .storage_reads
        .iter()
        .map(|read| {
            let contract = deployed.get(&read.contract).copied().ok_or_else(|| {
                SimError::Executor(format!(
                    "storage read `{}` references undeployed contract `{}`",
                    read.name, read.contract
                ))
            })?;
            Ok(PreparedStorageRead {
                name: read.name.clone(),
                contract_name: read.contract.clone(),
                weight: read.weight,
                contract,
                key: parse_hex(&read.key)?,
                expect_present: read.expect_present,
            })
        })
        .collect()
}

fn prepare_contract_calls(
    manifest: &SimulationManifest,
    deployed: &BTreeMap<String, ContractAddress>,
) -> Result<Vec<PreparedContractCall>, SimError> {
    manifest
        .contract_calls
        .iter()
        .map(|call| {
            let contract = deployed.get(&call.contract).copied().ok_or_else(|| {
                SimError::Executor(format!(
                    "contract call `{}` references undeployed contract `{}`",
                    call.name, call.contract
                ))
            })?;
            Ok(PreparedContractCall {
                name: call.name.clone(),
                contract_name: call.contract.clone(),
                weight: call.weight,
                contract,
                entrypoint: parse_hex(&call.entrypoint)?,
                calldata: parse_hex(&call.calldata)?,
            })
        })
        .collect()
}

fn prepare_read_after_write(
    manifest: &SimulationManifest,
    deployed: &BTreeMap<String, ContractAddress>,
) -> Result<Vec<PreparedReadAfterWrite>, SimError> {
    manifest
        .read_after_write
        .iter()
        .map(|entry| {
            let contract = deployed.get(&entry.contract).copied().ok_or_else(|| {
                SimError::Executor(format!(
                    "read-after-write `{}` references undeployed contract `{}`",
                    entry.name, entry.contract
                ))
            })?;
            Ok(PreparedReadAfterWrite {
                name: entry.name.clone(),
                contract_name: entry.contract.clone(),
                weight: entry.weight,
                contract,
                write_entrypoint: parse_hex(&entry.write.entrypoint)?,
                write_calldata: parse_hex(&entry.write.calldata)?,
                read_entrypoint: parse_hex(&entry.read.entrypoint)?,
                read_calldata: parse_hex(&entry.read.calldata)?,
            })
        })
        .collect()
}

fn prepare_event_queries(
    manifest: &SimulationManifest,
    deployed: &BTreeMap<String, ContractAddress>,
) -> Result<Vec<PreparedEventQuery>, SimError> {
    manifest
        .event_queries
        .iter()
        .map(|query| {
            let contract = deployed.get(&query.contract).copied().ok_or_else(|| {
                SimError::Executor(format!(
                    "event query `{}` references undeployed contract `{}`",
                    query.name, query.contract
                ))
            })?;
            Ok(PreparedEventQuery {
                name: query.name.clone(),
                contract_name: query.contract.clone(),
                weight: query.weight,
                contract,
                order: query.order,
                anchor: query.anchor.clone(),
                cursor: parse_hex(&query.cursor)?,
                limit: query.limit,
                expect_min_events: query.expect_min_events,
            })
        })
        .collect()
}

fn parse_hex(value: &str) -> Result<Vec<u8>, SimError> {
    let stripped = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| SimError::Executor("hex value must be 0x-prefixed".into()))?;
    hex::decode(stripped).map_err(|error| SimError::Executor(error.to_string()))
}

fn parse_contract_address(value: &str) -> Result<ContractAddress, SimError> {
    let bytes = parse_hex(value)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SimError::Executor("contract address must be 32 bytes".into()))?;
    Ok(ContractAddress::from(bytes))
}

fn exec_status_name(status: &ExecStatus) -> &'static str {
    match status {
        ExecStatus::Ok => "ok",
        ExecStatus::Reverted { .. } => "reverted",
        ExecStatus::DeterministicError { .. } => "deterministic-error",
        ExecStatus::NonDeterministicError => "non-deterministic-error",
        ExecStatus::FailedGasSponsorship => "failed-gas-sponsorship",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::{Arc, Mutex};

    use groundhog_json_rpc_types::{
        BlockHeight,
        block::ChainTip,
        hex::Hex0x,
        runtime_manifest::RuntimeManifest,
        view::{ViewRequest, ViewResponse},
    };
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::transaction::SignedTransaction;
    use serde_json::json;
    use tempfile::NamedTempFile;

    use crate::event::EventWriter;
    use crate::manifest::{
        EventQueryAnchorSpec, EventQueryOrderSpec, EventQuerySpec, LoadPhase, ViewCallSpec,
        WorkloadKind, WorkloadSpec,
    };
    use crate::nonce::NonceNamespace;
    use crate::runtime::SimAccount;
    use crate::target::{TargetProviderKind, TargetProviderSpec};

    use super::*;

    #[derive(Clone)]
    struct DeployClient {
        tx_hash: TxHash,
        receipt: Option<TransactionReceipt>,
        submitted: Arc<Mutex<Vec<String>>>,
        tip_height: i64,
    }

    impl SimRpcClient for DeployClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            let tip_height = self.tip_height;
            async move {
                Ok(ChainTip {
                    height: BlockHeight::from(tip_height),
                    block_hash: Hex0x(SHA256Hash::from([0x42; 32])),
                })
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            let submitted = self.submitted.clone();
            let tx_hash = self.tx_hash;
            async move {
                submitted
                    .lock()
                    .expect("submitted list should lock")
                    .push(tx_hex);
                Ok(tx_hash)
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            _tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let receipt = self.receipt.clone();
            async move { Ok(receipt) }
        }

        #[allow(clippy::manual_async_fn)]
        fn view(
            &self,
            _request: ViewRequest,
        ) -> impl Future<Output = Result<ViewResponse, String>> + Send {
            async { Err("view should not be called".into()) }
        }
    }

    #[test]
    fn resolves_relative_artifact_against_override_root() {
        let scenario = Path::new("/repo/examples/sim/demo.json");
        let root = Path::new("/tmp/artifacts");

        let path = resolve_artifact_path(scenario, Some(root), Path::new("fibonacci.wasm"));

        assert_eq!(path, PathBuf::from("/tmp/artifacts/fibonacci.wasm"));
    }

    #[test]
    fn resolves_relative_artifact_against_scenario_dir_without_override() {
        let scenario = Path::new("/repo/examples/sim/demo.json");

        let path = resolve_artifact_path(scenario, None, Path::new("fibonacci.wasm"));

        assert_eq!(path, PathBuf::from("/repo/examples/sim/fibonacci.wasm"));
    }

    #[tokio::test]
    async fn deploy_contract_records_success_and_consumes_setup_nonce() {
        let wasm = NamedTempFile::new().expect("wasm temp should create");
        std::fs::write(wasm.path(), b"\0asm").expect("wasm fixture should write");
        let manifest = manifest(wasm.path());
        let file = NamedTempFile::new().expect("event temp should create");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut actors = actor_states();
        let tx_hash = TxHash(Hex0x(SHA256Hash::from([0x5a; 32])));
        let receipt = deploy_receipt(tx_hash, ExecStatus::Ok, Some(vec![0x77; 32]));
        let submitted = Arc::new(Mutex::new(Vec::new()));

        let setup = deploy_manifest_contracts(
            DeployClient {
                tx_hash,
                receipt: Some(receipt),
                submitted: submitted.clone(),
                tip_height: 15_000_000,
            },
            &manifest,
            &runtime_manifest(),
            Path::new("/repo/examples/sim/demo.json"),
            None,
            &mut actors,
            &mut event_writer,
        )
        .await
        .expect("deploy should run");

        assert!(!setup.failed);
        assert_eq!(actors[0].next_nonce_sequence, 1);
        assert_eq!(
            submitted.lock().expect("submitted list should lock").len(),
            1
        );
        let record = &setup.records[0];
        assert_eq!(record.status, ContractDeploymentStatus::Completed);
        assert_eq!(record.tx_hash.as_deref(), Some(prefixed_hex(0x5a).as_str()));
        assert_eq!(record.address.as_deref(), Some(prefixed_hex(0x77).as_str()));
        assert_eq!(record.address_source, Some(DeployAddressSource::Receipt));
        assert_eq!(record.height, Some(42));
        assert_eq!(setup.view_calls.len(), 1);
        assert_eq!(setup.storage_reads.len(), 1);
        assert_eq!(setup.storage_reads[0].name, "fib-zero-key");
        assert_eq!(
            setup.storage_reads[0].contract,
            Hex0x(ContractAddress::from([0x77; 32])).0
        );
        assert_eq!(setup.storage_reads[0].key, vec![0; 32]);
        assert_eq!(setup.event_queries.len(), 1);
        assert_eq!(setup.event_queries[0].name, "fib-events");
        assert_eq!(
            setup.event_queries[0].contract,
            Hex0x(ContractAddress::from([0x77; 32])).0
        );
        assert_eq!(setup.event_queries[0].cursor, Vec::<u8>::new());
        assert_eq!(setup.read_after_write.len(), 1);
        assert_eq!(setup.read_after_write[0].name, "counter-roundtrip");
        assert_eq!(
            setup.read_after_write[0].contract,
            Hex0x(ContractAddress::from([0x77; 32])).0
        );
        assert_eq!(setup.read_after_write[0].write_entrypoint, vec![0x4f; 8]);
        assert_eq!(setup.read_after_write[0].read_entrypoint, vec![0xdf; 8]);
        assert_eq!(setup.contract_calls.len(), 1);
        assert_eq!(setup.contract_calls[0].name, "events-store-value");
        assert_eq!(
            setup.contract_calls[0].contract,
            ContractAddress::from([0x77; 32])
        );
        assert_eq!(setup.contract_calls[0].entrypoint, vec![0x11; 8]);
        assert_eq!(setup.contract_calls[0].calldata, vec![0x2a, 0x64]);
    }

    #[tokio::test]
    async fn deploy_contract_corpus_records_each_artifact() {
        let wasm = NamedTempFile::new().expect("wasm temp should create");
        std::fs::write(wasm.path(), b"\0asm").expect("wasm fixture should write");
        let mut manifest = manifest(wasm.path());
        manifest.contract_artifacts.push(ContractArtifactSpec {
            name: "events".into(),
            artifact: crate::manifest::ContractArtifactRef {
                kind: ContractArtifactKind::Wasm,
                path: wasm.path().to_path_buf(),
            },
        });
        let file = NamedTempFile::new().expect("event temp should create");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut actors = actor_states();
        let tx_hash = TxHash(Hex0x(SHA256Hash::from([0x5a; 32])));
        let receipt = deploy_receipt(tx_hash, ExecStatus::Ok, Some(vec![0x77; 32]));
        let submitted = Arc::new(Mutex::new(Vec::new()));

        let setup = deploy_manifest_contracts(
            DeployClient {
                tx_hash,
                receipt: Some(receipt),
                submitted: submitted.clone(),
                tip_height: 15_000_000,
            },
            &manifest,
            &runtime_manifest(),
            Path::new("/repo/examples/sim/demo.json"),
            None,
            &mut actors,
            &mut event_writer,
        )
        .await
        .expect("deploy should run");

        assert!(!setup.failed);
        assert_eq!(setup.records.len(), 2);
        assert_eq!(setup.records[0].name, "fibonacci");
        assert_eq!(setup.records[1].name, "events");
        assert_eq!(
            setup
                .records
                .iter()
                .map(|record| record.status)
                .collect::<Vec<_>>(),
            vec![
                ContractDeploymentStatus::Completed,
                ContractDeploymentStatus::Completed
            ]
        );
        assert_eq!(actors[0].next_nonce_sequence, 2);
        assert_eq!(
            submitted.lock().expect("submitted list should lock").len(),
            2
        );
    }

    #[tokio::test]
    async fn deploy_contract_failure_preserves_post_submit_fields() {
        let wasm = NamedTempFile::new().expect("wasm temp should create");
        std::fs::write(wasm.path(), b"\0asm").expect("wasm fixture should write");
        let manifest = manifest(wasm.path());
        let file = NamedTempFile::new().expect("event temp should create");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut actors = actor_states();
        let tx_hash = TxHash(Hex0x(SHA256Hash::from([0x6b; 32])));
        let receipt = deploy_receipt(
            tx_hash,
            ExecStatus::DeterministicError {
                kind: "outOfGas".into(),
            },
            None,
        );

        let setup = deploy_manifest_contracts(
            DeployClient {
                tx_hash,
                receipt: Some(receipt),
                submitted: Arc::new(Mutex::new(Vec::new())),
                tip_height: 10,
            },
            &manifest,
            &runtime_manifest(),
            Path::new("/repo/examples/sim/demo.json"),
            None,
            &mut actors,
            &mut event_writer,
        )
        .await
        .expect("deploy failure should still produce setup record");

        assert!(setup.failed);
        assert_eq!(actors[0].next_nonce_sequence, 1);
        let record = &setup.records[0];
        assert_eq!(record.status, ContractDeploymentStatus::Failed);
        assert_eq!(record.tx_hash.as_deref(), Some(prefixed_hex(0x6b).as_str()));
        assert_eq!(record.height, Some(42));
        assert!(record.error.as_deref().is_some_and(|error| {
            error.contains("contract deploy receipt status deterministic-error")
        }));
    }

    #[test]
    fn prepare_deploy_wraps_rv32_artifact_as_rv32_contract() {
        let rv32 = NamedTempFile::new().expect("rv32 temp should create");
        let bytes = b"rv32 artifact bytes".to_vec();
        std::fs::write(rv32.path(), &bytes).expect("rv32 fixture should write");
        let actors = actor_states();
        let deploy_entrypoint =
            ContractEntrypointID::from([0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]);

        let prepared = prepare_deploy(
            ContractArtifactKind::Rv32,
            rv32.path(),
            &actors[0],
            deploy_entrypoint,
            42,
        )
        .expect("rv32 deploy should prepare");
        let tx_bytes =
            hex::decode(prepared.tx_hex.trim_start_matches("0x")).expect("signed tx should be hex");
        let signed_tx: SignedTransaction =
            xdrs::txs::decode(&tx_bytes).expect("signed tx should decode");

        let contract = Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(bytes),
            },
        };
        let contract_xdr = xdrs::txs::encode(&contract).expect("contract XDR should encode");
        let contract_hash_bytes: [u8; 32] = Sha256::digest(&contract_xdr).into();

        assert_eq!(
            prepared.contract_hash,
            format!("0x{}", hex::encode(contract_hash_bytes))
        );
        assert_eq!(signed_tx.tx.invocation.entrypoint, deploy_entrypoint);
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hosted-devnet-runtime",
            "networkId": "devnet",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
            "producer": {
                "binary": "chuckd",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001",
                "dbGateway": "127.0.0.1:1002",
                "viewGateway": "127.0.0.1:1003"
            },
            "bootstrapAccountDerivation": {
                "scheme": crate::runtime::SUPPORTED_DERIVATION_SCHEME,
                "seed": "seed"
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": "dev-wallet.rv32a",
                    "deployEntrypoint": "0xf969542572bd6860",
                    "createWalletEntrypoint": "0x78b187d37f222827",
                    "doActionEntrypoint": "0xa0fff94c64fbaf3c"
                },
                "storageCostModel": {
                    "hash": prefixed_hex(0x55),
                    "baseDataItemCost": "1",
                    "bytestringCostPerByte": "1",
                    "baseContractDeploymentCost": "1",
                    "contractDeploymentCostPerByte": "1",
                    "authPerKeyCost": "1",
                    "rcqCostPerEntry": "1",
                    "replayCacheCostPerEntry": "1",
                    "balanceCost": "1"
                }
            }
        }))
        .expect("runtime manifest fixture should parse")
    }

    fn manifest(artifact_path: &Path) -> SimulationManifest {
        SimulationManifest {
            schema_version: crate::manifest::SIMULATION_SCHEMA_VERSION,
            kind: crate::manifest::SIMULATION_KIND.into(),
            name: "contract-test".into(),
            seed: "seed".into(),
            target: TargetProviderSpec {
                provider: TargetProviderKind::Local,
                rpc_url: None,
                runtime_manifest: None,
                hogd_dev_binary: None,
                block_driver: None,
                block_interval_ms: None,
                max_rpc_connections: None,
                max_rpc_client_concurrency: None,
            },
            client: None,
            bootstrap: None,
            load_shape: vec![LoadPhase {
                duration: crate::manifest::DurationSpec("1s".into()),
                users: 1,
                rate_per_user_hz: 1.0,
                stagger_actors: false,
            }],
            workloads: vec![WorkloadSpec {
                name: WorkloadKind::ViewCall,
                weight: 1,
            }],
            contract_artifacts: vec![ContractArtifactSpec {
                name: "fibonacci".into(),
                artifact: crate::manifest::ContractArtifactRef {
                    kind: ContractArtifactKind::Wasm,
                    path: artifact_path.to_path_buf(),
                },
            }],
            contract_calls: vec![crate::manifest::ContractCallSpec {
                name: "events-store-value".into(),
                contract: "fibonacci".into(),
                weight: 1,
                entrypoint: "0x1111111111111111".into(),
                calldata: "0x2a64".into(),
            }],
            view_calls: vec![ViewCallSpec {
                name: "fib-10".into(),
                contract: "fibonacci".into(),
                weight: 1,
                entrypoint: "0x1111111111111111".into(),
                calldata: "0x0a".into(),
                expect_return_data: Some("0x37".into()),
            }],
            storage_reads: vec![crate::manifest::StorageReadSpec {
                name: "fib-zero-key".into(),
                contract: "fibonacci".into(),
                weight: 1,
                key: format!("0x{}", "00".repeat(32)),
                expect_present: Some(false),
            }],
            event_queries: vec![EventQuerySpec {
                name: "fib-events".into(),
                contract: "fibonacci".into(),
                weight: 1,
                order: EventQueryOrderSpec::Descending,
                anchor: EventQueryAnchorSpec::AtEnd,
                cursor: "0x".into(),
                limit: 10,
                expect_min_events: Some(0),
            }],
            read_after_write: vec![crate::manifest::ReadAfterWriteSpec {
                name: "counter-roundtrip".into(),
                contract: "fibonacci".into(),
                weight: 1,
                write: crate::manifest::RawWriteSpec {
                    entrypoint: "0x4f4f4f4f4f4f4f4f".into(),
                    calldata: "0x".into(),
                },
                read: crate::manifest::RawReadSpec {
                    entrypoint: "0xdfdfdfdfdfdfdfdf".into(),
                    calldata: "0x".into(),
                },
            }],
            assertions: crate::manifest::AssertionSpec::default(),
            chaos: Vec::new(),
            observability: None,
            baseline: None,
            resolved: None,
        }
    }

    fn actor_states() -> Vec<ActorState> {
        vec![ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("contract-test-seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x99; 32])).0,
            },
            NonceNamespace::new(1).expect("namespace should fit"),
        )]
    }

    fn deploy_receipt(
        tx_hash: TxHash,
        status: ExecStatus,
        return_data: Option<Vec<u8>>,
    ) -> TransactionReceipt {
        TransactionReceipt {
            tx_hash,
            height: BlockHeight::from(42),
            status,
            events: Vec::new(),
            return_data: return_data.map(HexBytes),
            abort_data: None,
        }
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
