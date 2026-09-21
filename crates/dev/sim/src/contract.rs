use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use groundhog_json_rpc_types::{
    hex::HexBytes,
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    transaction::TxHash,
};
use groundhog_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams, build_deploy_transaction,
    derive_deploy_address, sign_and_encode_transaction_hex,
};
use groundhog_xdr::{
    contract::{Contract, ContractBlob, ContractEntrypointID},
    types::{ContractAddress, SHA256Hash},
};
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

use crate::actor::{ActorError, ActorState, WalletKind};
use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent};
use crate::executor::{DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL, DEFAULT_RECEIPT_DRAIN_TIMEOUT};
use crate::manifest::{
    ContractArtifactSpec, EntrypointSpec, EventQueryAnchorSpec, EventQueryOrderSpec,
    SimulationManifest,
};
use crate::result::{ContractDeploymentRecord, ContractDeploymentStatus, DeployAddressSource};
use crate::rpc::{SimRpcClient, SimRpcError, receipt_wait_detail, wait_for_receipt};
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
/// The executor wraps the write through the scheduled actor's wallet adapter
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
/// The executor wraps this target through the scheduled actor's wallet adapter,
/// so the prepared value contains only the inner contract call details.
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
    let deploy_entrypoint = if actor.wallet_kind == WalletKind::NativePubkey {
        crate::native::DEPLOY
    } else {
        runtime_manifest
        .bootstrap
        .dev_wallet
        .deploy_entrypoint_id()
        .map_err(|source| {
            SimError::Executor(format!(
                "failed to parse runtime manifest bootstrap.devWallet.deployEntrypoint: {source}"
            ))
        })?
    };
    let mut records = Vec::new();
    let mut deployed = BTreeMap::new();
    let mut failed = false;

    for artifact in &manifest.contract_artifacts {
        let block_expiry_height = resolve_block_expiry_height(client.clone()).await?;
        let record = deploy_one_contract(
            client.clone(),
            actor,
            event_writer,
            DeployOneContractInput {
                artifact,
                collateral_raw: manifest
                    .accounts
                    .as_ref()
                    .and_then(|a| a.deployment_collateral_raw.as_deref()),
                scenario_path,
                contract_artifact_root,
                deploy_entrypoint,
                block_expiry_height,
            },
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
    actor: &mut ActorState,
    event_writer: &mut EventWriter,
    input: DeployOneContractInput<'_>,
) -> Result<ContractDeploymentRecord, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let artifact_path = resolve_artifact_path(
        input.scenario_path,
        input.contract_artifact_root,
        &input.artifact.artifact.path,
    );
    let mut record = ContractDeploymentRecord {
        collateral: None,
        name: input.artifact.name.clone(),
        status: ContractDeploymentStatus::Started,
        artifact_kind: Some(input.artifact.artifact.kind),
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

    // Reserve the transfer nonce first so the deployment retains a distinct
    // nonce/address while its collateral is committed in a preceding transaction.
    let collateral_nonce = if actor.wallet_kind == WalletKind::NativePubkey {
        let nonce = actor.current_nonce()?;
        actor.consume_setup_nonce()?;
        Some(nonce)
    } else {
        None
    };

    let prepared = match prepare_deploy(
        &artifact_path,
        actor,
        input.deploy_entrypoint,
        input.block_expiry_height,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            return fail_deployment(record, event_writer, error.to_string()).await;
        }
    };

    record.artifact_sha256 = Some(prepared.artifact_sha256.clone());
    record.contract_hash = Some(prepared.contract_hash.clone());
    record.nonce = Some(prepared.tx_nonce.to_string());
    record.deploy_address_nonce = Some(prepared.deploy_address_nonce.clone());
    if let Some(nonce) = collateral_nonce {
        let amount = input
            .collateral_raw
            .ok_or_else(|| SimError::Executor("native deploy collateral is missing".into()))?
            .parse::<u128>()
            .map_err(|_| SimError::Executor("invalid native deploy collateral".into()))?;
        if let Err(error) = fund_native_deployment(
            client.clone(),
            actor,
            nonce,
            input.block_expiry_height,
            (prepared.predicted_address, amount),
            &mut record,
            event_writer,
        )
        .await
        {
            return fail_deployment(record, event_writer, error.to_string()).await;
        }
    }
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
            record.error = Some(error.to_string());
            event_writer
                .write(&ResultEvent::ContractDeployFailed {
                    event: record.clone(),
                })
                .await?;
        }
    }
    Ok(record)
}

struct DeployOneContractInput<'a> {
    collateral_raw: Option<&'a str>,
    artifact: &'a ContractArtifactSpec,
    scenario_path: &'a Path,
    contract_artifact_root: Option<&'a Path>,
    deploy_entrypoint: ContractEntrypointID,
    block_expiry_height: i64,
}

async fn fail_deployment(
    mut record: ContractDeploymentRecord,
    writer: &mut EventWriter,
    error: String,
) -> Result<ContractDeploymentRecord, SimError> {
    record.status = ContractDeploymentStatus::Failed;
    record.error = Some(error);
    writer
        .write(&ResultEvent::ContractDeployFailed {
            event: record.clone(),
        })
        .await?;
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
    let contract = Contract::kRv32 {
        rv32_contract: blob,
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
        auth_claim: actor.auth_claim(),
    });
    let tx_hex = sign_and_encode_transaction_hex(
        tx,
        &actor.signer,
        SignTransactionParams::default(),
        &actor.network_hash,
    )
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
    result: Result<(), DeployAttemptError>,
}

/// Why one contract deployment attempt did not complete.
#[derive(Debug, thiserror::Error)]
enum DeployAttemptError {
    /// The deploy transaction was not accepted for submission.
    #[error("contract deploy submit failed: {0}")]
    Submit(#[from] SimRpcError),
    /// The deploying actor could not advance its setup nonce.
    #[error("{0}")]
    Nonce(#[from] ActorError),
    /// No committed receipt resolved, or its contents could not be read.
    #[error("{0}")]
    Resolve(#[from] SimError),
    /// The receipt committed but reported a non-`Ok` execution status.
    #[error("contract deploy receipt status {status}")]
    ReceiptStatus { status: &'static str },
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
                result: Err(error.into()),
            };
        }
    };
    if let Err(error) = actor.consume_setup_nonce() {
        return DeployAttempt {
            progress,
            result: Err(error.into()),
        };
    }
    progress.tx_hash = match json_string(&tx_hash, "txHash") {
        Ok(tx_hash) => Some(tx_hash),
        Err(error) => {
            return DeployAttempt {
                progress,
                result: Err(error.into()),
            };
        }
    };
    let receipt = match poll_receipt(client, tx_hash).await {
        Ok(receipt) => receipt,
        Err(error) => {
            return DeployAttempt {
                progress,
                result: Err(error.into()),
            };
        }
    };
    progress.height = Some(*receipt.height);
    if !matches!(receipt.status, ExecStatus::Ok) {
        return DeployAttempt {
            progress,
            result: Err(DeployAttemptError::ReceiptStatus {
                status: exec_status_name(&receipt.status),
            }),
        };
    }
    let (address, address_source) =
        match resolve_deployed_address(receipt.return_data.as_ref(), &prepared.predicted_address) {
            Ok(resolved) => resolved,
            Err(error) => {
                return DeployAttempt {
                    progress,
                    result: Err(error.into()),
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
    wait_for_receipt(
        &client,
        tx_hash,
        DEFAULT_RECEIPT_DRAIN_TIMEOUT,
        DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL,
    )
    .await
    .map_err(|error| SimError::Executor(receipt_wait_detail("contract deploy", &error)))
}

async fn fund_native_deployment<C>(
    client: C,
    actor: &ActorState,
    nonce: u64,
    expiry: i64,
    transfer: (ContractAddress, u128),
    record: &mut ContractDeploymentRecord,
    event_writer: &mut EventWriter,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let (target, amount) = transfer;
    record.collateral = Some(crate::result::NativeDeploymentCollateral {
        recipient: json_string(
            &groundhog_json_rpc_types::hex::Hex0x(target),
            "collateral recipient",
        )?,
        raw_amount: amount.to_string(),
        tx_hash: None,
        returned_tx_hash: None,
        height: None,
    });
    let calldata = groundhog_signing::native_hog_transfer_calldata(&target, amount)
        .map_err(|error| SimError::Executor(error.to_string()))?;
    let tx = crate::native::execute(
        actor,
        groundhog_signing::NATIVE_HOG_ADDRESS,
        groundhog_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
        &calldata,
        nonce,
        expiry,
    )
    .map_err(|error| SimError::Executor(error.to_string()))?;
    let expected = groundhog_signing::transaction_hash(&tx)
        .map_err(|error| SimError::Executor(error.to_string()))?;
    let tx_hex = sign_and_encode_transaction_hex(
        tx,
        &actor.signer,
        SignTransactionParams::default(),
        &actor.network_hash,
    )
    .map_err(|error| SimError::Executor(error.to_string()))?;
    record
        .collateral
        .as_mut()
        .ok_or_else(|| SimError::Executor("collateral record missing".into()))?
        .tx_hash = Some(json_string(
        &TxHash(groundhog_json_rpc_types::hex::Hex0x(expected)),
        "collateral expected txHash",
    )?);
    // Persist the locally signed identity before the transport can have an ambiguous outcome.
    event_writer
        .write(&ResultEvent::ContractDeployStarted {
            event: record.clone(),
        })
        .await?;
    let tx_hash = client.send_raw_transaction(tx_hex).await.map_err(|error| {
        SimError::Executor(format!(
            "native deployment collateral submit failed: {error}"
        ))
    })?;
    record
        .collateral
        .as_mut()
        .ok_or_else(|| SimError::Executor("collateral record missing".into()))?
        .returned_tx_hash = Some(json_string(&tx_hash, "collateral returned txHash")?);
    event_writer
        .write(&ResultEvent::ContractDeployStarted {
            event: record.clone(),
        })
        .await?;
    if tx_hash.0.0 != expected {
        return Err(SimError::Executor(
            "native collateral transaction hash mismatch".into(),
        ));
    }
    let collateral = record
        .collateral
        .as_mut()
        .ok_or_else(|| SimError::Executor("collateral record missing".into()))?;
    let receipt =
        tokio::time::timeout(DEFAULT_RECEIPT_DRAIN_TIMEOUT, poll_receipt(client, tx_hash))
            .await
            .map_err(|_| {
                SimError::Executor("native deployment collateral receipt timed out".into())
            })??;
    if receipt.tx_hash != tx_hash || !matches!(receipt.status, ExecStatus::Ok) {
        return Err(SimError::Executor(format!(
            "native deployment collateral did not commit successfully: {:?}",
            receipt.status
        )));
    }
    collateral.height = Some(*receipt.height);
    Ok(())
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
                entrypoint: resolve_entrypoint(&view.entrypoint)?,
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
                entrypoint: resolve_entrypoint(&call.entrypoint)?,
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
                write_entrypoint: resolve_entrypoint(&entry.write.entrypoint)?,
                write_calldata: parse_hex(&entry.write.calldata)?,
                read_entrypoint: resolve_entrypoint(&entry.read.entrypoint)?,
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

fn resolve_entrypoint(entrypoint: &EntrypointSpec) -> Result<Vec<u8>, SimError> {
    entrypoint
        .resolve()
        .map(|selector| selector.to_vec())
        .map_err(|error| SimError::Executor(format!("invalid contract entrypoint: {error}")))
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

    use crate::rpc::SimRpcError;

    use crate::event::EventWriter;
    use crate::manifest::{
        ContractArtifactKind, EntrypointSpec, EventQueryAnchorSpec, EventQueryOrderSpec,
        EventQuerySpec, LoadPhase, SymbolicEntrypointSpec, ViewCallSpec, WorkloadKind,
        WorkloadSpec,
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
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, SimRpcError>> + Send {
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
        ) -> impl Future<Output = Result<TxHash, SimRpcError>> + Send {
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
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, SimRpcError>> + Send {
            let receipt = self.receipt.clone();
            async move { Ok(receipt) }
        }

        #[allow(clippy::manual_async_fn)]
        fn view(
            &self,
            _request: ViewRequest,
        ) -> impl Future<Output = Result<ViewResponse, SimRpcError>> + Send {
            async { Err(SimRpcError::detail("view", "view should not be called")) }
        }
    }

    fn opaque_rv32_artifact() -> NamedTempFile {
        let artifact = NamedTempFile::new().expect("rv32 temp should create");
        std::fs::write(artifact.path(), b"opaque rv32 artifact")
            .expect("rv32 fixture should write");
        artifact
    }

    #[test]
    fn resolves_relative_artifact_against_override_root() {
        let scenario = Path::new("/repo/examples/sim/demo.json");
        let root = Path::new("/tmp/artifacts");

        let path = resolve_artifact_path(scenario, Some(root), Path::new("fibonacci.rv32a"));

        assert_eq!(path, PathBuf::from("/tmp/artifacts/fibonacci.rv32a"));
    }

    #[test]
    fn resolves_relative_artifact_against_scenario_dir_without_override() {
        let scenario = Path::new("/repo/examples/sim/demo.json");

        let path = resolve_artifact_path(scenario, None, Path::new("fibonacci.rv32a"));

        assert_eq!(path, PathBuf::from("/repo/examples/sim/fibonacci.rv32a"));
    }

    #[tokio::test]
    async fn deploy_contract_records_success_and_consumes_setup_nonce() {
        let rv32 = opaque_rv32_artifact();
        let manifest = manifest(rv32.path());
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
        assert_eq!(
            setup.view_calls[0].entrypoint,
            vec![0x14, 0x50, 0x68, 0x6b, 0xab, 0x1d, 0x6a, 0x5f]
        );
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
        assert_eq!(
            setup.read_after_write[0].write_entrypoint,
            vec![0xb3, 0x04, 0xd6, 0xb5, 0xe0, 0x57, 0xd0, 0xf6]
        );
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
        let rv32 = opaque_rv32_artifact();
        let mut manifest = manifest(rv32.path());
        manifest.contract_artifacts.push(ContractArtifactSpec {
            name: "events".into(),
            artifact: crate::manifest::ContractArtifactRef {
                kind: ContractArtifactKind::Rv32,
                path: rv32.path().to_path_buf(),
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
        let rv32 = opaque_rv32_artifact();
        let manifest = manifest(rv32.path());
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

    #[derive(Clone)]
    struct NativeDeployClient {
        submitted: Arc<Mutex<Vec<SignedTransaction>>>,
        fail_collateral: bool,
        submit_failure: Option<bool>,
        event_path: Option<std::path::PathBuf>,
    }
    impl SimRpcClient for NativeDeployClient {
        async fn chain_tip(
            &self,
        ) -> Result<groundhog_json_rpc_types::block::ChainTip, SimRpcError> {
            Ok(groundhog_json_rpc_types::block::ChainTip {
                height: 1.into(),
                block_hash: Hex0x(SHA256Hash::from([0; 32])),
            })
        }
        async fn send_raw_transaction(&self, tx_hex: String) -> Result<TxHash, SimRpcError> {
            let decode_failed = |error: &dyn std::fmt::Display| {
                SimRpcError::detail("sendRawTransaction", error.to_string())
            };
            let bytes =
                hex::decode(tx_hex.trim_start_matches("0x")).map_err(|e| decode_failed(&e))?;
            let signed: SignedTransaction =
                xdrs::txs::decode(&bytes).map_err(|e| decode_failed(&e))?;
            let hash =
                groundhog_signing::transaction_hash(&signed.tx).map_err(|e| decode_failed(&e))?;
            self.submitted
                .lock()
                .expect("submitted tx lock")
                .push(signed);
            if let Some(path) = &self.event_path {
                let events = std::fs::read_to_string(path).expect("pre-submit evidence");
                assert!(events.contains(&hex::encode(hash.as_ref())));
            }
            match self.submit_failure {
                Some(false) => Err(SimRpcError::detail(
                    "sendRawTransaction",
                    "ambiguous transport failure",
                )),
                Some(true) => Ok(TxHash(Hex0x(SHA256Hash::from([7; 32])))),
                None => Ok(TxHash(Hex0x(hash))),
            }
        }
        async fn transaction_receipt(
            &self,
            hash: TxHash,
        ) -> Result<Option<TransactionReceipt>, SimRpcError> {
            Ok(Some(deploy_receipt(
                hash,
                if self.fail_collateral {
                    ExecStatus::Reverted { message: None }
                } else {
                    ExecStatus::Ok
                },
                None,
            )))
        }
    }

    #[tokio::test]
    async fn collateral_ambiguous_submit_retains_expected_and_returned_identity() {
        for mismatch in [false, true] {
            let artifact = opaque_rv32_artifact();
            let mut manifest = manifest(artifact.path());
            manifest.accounts = Some(serde_json::from_value(serde_json::json!({"kind":"native","seed":"test","deploymentCollateralRaw":"10000000"})).expect("native config"));
            let mut actor = actor_states().remove(0);
            actor.use_native_wallet();
            let file = tempfile::NamedTempFile::new().expect("events");
            let submitted = Arc::new(Mutex::new(Vec::new()));
            let client = NativeDeployClient {
                submitted: submitted.clone(),
                fail_collateral: false,
                submit_failure: Some(mismatch),
                event_path: Some(file.path().to_path_buf()),
            };
            let mut writer = EventWriter::create(file.path()).await.expect("writer");
            let setup = deploy_manifest_contracts(
                client,
                &manifest,
                &runtime_manifest(),
                Path::new("scenario.json"),
                None,
                &mut [actor],
                &mut writer,
            )
            .await
            .expect("recorded failure");
            assert!(setup.failed);
            let record = &setup.records[0];
            let collateral = record.collateral.as_ref().expect("collateral");
            let transactions = submitted.lock().expect("transactions");
            assert_eq!(transactions.len(), 1);
            let expected = groundhog_signing::transaction_hash(&transactions[0].tx).expect("hash");
            assert_eq!(
                collateral.tx_hash.as_deref(),
                Some(format!("0x{}", hex::encode(expected.as_ref())).as_str())
            );
            assert_eq!(collateral.returned_tx_hash.is_some(), mismatch);
            if mismatch {
                assert_ne!(collateral.tx_hash, collateral.returned_tx_hash);
            }
            let events = std::fs::read_to_string(file.path()).expect("events");
            assert!(events.contains("contract-deploy-failed"));
        }
    }

    #[tokio::test]
    async fn native_deploy_funds_predicted_address_before_deploy_with_distinct_nonces() {
        for fail_collateral in [false, true] {
            let artifact = opaque_rv32_artifact();
            let mut manifest = manifest(artifact.path());
            manifest.accounts = Some(
                serde_json::from_value(
                    json!({"kind":"native","seed":"test","deploymentCollateralRaw":"10000000"}),
                )
                .expect("native config"),
            );
            let submitted = Arc::new(Mutex::new(Vec::new()));
            let client = NativeDeployClient {
                submitted: submitted.clone(),
                fail_collateral,
                submit_failure: None,
                event_path: None,
            };
            let mut actors = actor_states();
            actors[0].use_native_wallet();
            let file = NamedTempFile::new().expect("events");
            let mut writer = EventWriter::create(file.path()).await.expect("writer");
            let setup = deploy_manifest_contracts(
                client,
                &manifest,
                &runtime_manifest(),
                Path::new("/tmp/scenario.json"),
                None,
                &mut actors,
                &mut writer,
            )
            .await
            .expect("setup completes with structured outcome");
            assert_eq!(setup.failed, fail_collateral);
            let transactions = submitted.lock().expect("tx lock");
            assert_eq!(transactions.len(), if fail_collateral { 1 } else { 2 });
            assert_eq!(
                transactions[0].tx.invocation.entrypoint,
                crate::native::EXECUTE
            );
            assert_eq!(transactions[0].tx.auth_claim, actors[0].auth_claim());
            let record = &setup.records[0];
            let collateral = record.collateral.as_ref().expect("collateral proof");
            assert_eq!(collateral.raw_amount, "10000000");
            assert!(collateral.tx_hash.is_some());
            if !fail_collateral {
                assert_eq!(
                    transactions[1].tx.invocation.entrypoint,
                    crate::native::DEPLOY
                );
                assert_eq!(transactions[1].tx.auth_claim, actors[0].auth_claim());
                assert_ne!(
                    transactions[0].tx.uniquifier.nonce,
                    transactions[1].tx.uniquifier.nonce
                );
                assert_eq!(record.address.as_ref(), Some(&collateral.recipient));
                assert_eq!(actors[0].next_nonce_sequence, 2);
            }
        }
    }

    #[test]
    fn prepare_deploy_wraps_rv32_artifact_as_rv32_contract() {
        let rv32 = NamedTempFile::new().expect("rv32 temp should create");
        let bytes = b"rv32 artifact bytes".to_vec();
        std::fs::write(rv32.path(), &bytes).expect("rv32 fixture should write");
        let actors = actor_states();
        let deploy_entrypoint =
            ContractEntrypointID::from([0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]);

        let prepared = prepare_deploy(rv32.path(), &actors[0], deploy_entrypoint, 42)
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
            "schemaVersion": 2,
            "kind": "hosted-devnet-runtime",
            "networkHash": "11".repeat(32),
            "protocol": "hogp/test00000000000",
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
                    "artifactHint": "dev_wallet.rv32a",
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
            accounts: None,
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
                    kind: ContractArtifactKind::Rv32,
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
                entrypoint: EntrypointSpec::Symbolic(SymbolicEntrypointSpec {
                    interface: "Fibonacci".into(),
                    version: "0.1.0".into(),
                    method: "compute".into(),
                }),
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
                    entrypoint: EntrypointSpec::Symbolic(SymbolicEntrypointSpec {
                        interface: "CallerCounter".into(),
                        version: "0.1.0".into(),
                        method: "bump".into(),
                    }),
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
                network_hash: groundhog_xdr::types::NetworkHash::new(
                    groundhog_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
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
            gas_used: 1_500,
        }
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
