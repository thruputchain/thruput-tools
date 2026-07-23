use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
use groundhog_json_rpc_types::{
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    transaction::TxHash,
};
use groundhog_signing::{
    BuildDeployTransaction, BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT,
    SignTransactionParams, build_deploy_transaction, build_dev_wallet_do_action_transaction,
    derive_deploy_address, sign_and_encode_transaction_hex, source_address_auth_claim,
};
use groundhog_xdr::{
    contract::{Contract, ContractBlob, ContractEntrypointID},
    types::{ContractAddress, Ed25519PublicKey, SHA256Hash},
};
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

use crate::{
    actor::{ActorState, required_actor_count},
    error::SimError,
    event::{ActorProvisionEvent, EventWriter, ResultEvent},
    executor::{DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL, SimRpcClient},
    manifest::{ActorProvisioningSpec, ContractArtifactKind, SimulationManifest},
    runtime::{SimAccount, derive_provisioned_account_signer},
    util::json_string,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const HOG_TOKEN_ADDRESS: [u8; 32] = {
    let mut bytes = [0_u8; 32];
    bytes[31] = 1;
    bytes
};
const HOG_DECIMALS: usize = 7;
const HOG_SCALE: u128 = 10_000_000;
const PROVISION_RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
const WALLET_SPONSOR_ROLE: &str = "wallet_sponsor";
const FAUCET_ROLE: &str = "faucet";

pub(crate) struct ActorProvisionInputs<'a> {
    pub(crate) manifest: &'a SimulationManifest,
    pub(crate) runtime_manifest: &'a RuntimeManifest,
    pub(crate) runtime_manifest_path: &'a Path,
    pub(crate) scenario_path: &'a Path,
    pub(crate) contract_artifact_root: Option<&'a Path>,
}

pub(crate) async fn provision_missing_actor_accounts<C>(
    client: C,
    inputs: ActorProvisionInputs<'_>,
    actor_states: &mut Vec<ActorState>,
    event_writer: &mut EventWriter,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let Some(spec) = inputs
        .manifest
        .bootstrap
        .as_ref()
        .and_then(|bootstrap| bootstrap.actor_provisioning.as_ref())
        .filter(|spec| spec.enabled)
    else {
        return Ok(());
    };
    let required = required_actor_count(inputs.manifest);
    if actor_states.len() >= required {
        return Ok(());
    }
    if actor_states.is_empty() {
        return Err(SimError::Executor(
            "actor provisioning requires at least one runtime bootstrap account as sponsor".into(),
        ));
    }

    let plan = prepare_provision_plan(&inputs, spec, actor_states)?;
    while actor_states.len() < required {
        provision_one_actor(client.clone(), &inputs, &plan, actor_states, event_writer).await?;
    }

    Ok(())
}

fn prepare_provision_plan(
    inputs: &ActorProvisionInputs<'_>,
    spec: &ActorProvisioningSpec,
    actor_states: &[ActorState],
) -> Result<ProvisionPlan, SimError> {
    let artifact_path = resolve_dev_wallet_artifact(
        spec,
        inputs.runtime_manifest,
        inputs.runtime_manifest_path,
        inputs.scenario_path,
        inputs.contract_artifact_root,
    )?;
    let wallet_artifact_kind = spec
        .dev_wallet_artifact_kind
        .unwrap_or_else(|| infer_dev_wallet_artifact_kind(&artifact_path));
    let wallet_contract = load_dev_wallet_contract(&artifact_path, wallet_artifact_kind)?;
    let wallet_contract_hash = contract_hash(&wallet_contract)?;
    let create_wallet_entrypoint = inputs
        .runtime_manifest
        .bootstrap
        .dev_wallet
        .create_wallet_entrypoint_id()
        .map_err(|source| {
            SimError::Executor(format!(
                "failed to parse runtime manifest bootstrap.devWallet.createWalletEntrypoint: {source}"
            ))
        })?;
    let wallet_action_entrypoint = inputs
        .runtime_manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|source| {
            SimError::Executor(format!(
                "failed to parse runtime manifest bootstrap.devWallet.doActionEntrypoint: {source}"
            ))
        })?;
    let airdrop_raw_amount = spec
        .airdrop_hog
        .as_deref()
        .map(parse_hog_amount)
        .transpose()?;
    let preferred_create_sponsor_index = select_sponsor_actor_index(
        actor_states,
        inputs.runtime_manifest,
        &[WALLET_SPONSOR_ROLE, FAUCET_ROLE],
    );
    let preferred_airdrop_sponsor_index = select_sponsor_actor_index(
        actor_states,
        inputs.runtime_manifest,
        &[FAUCET_ROLE, WALLET_SPONSOR_ROLE],
    );

    Ok(ProvisionPlan {
        wallet_contract,
        wallet_contract_hash,
        create_wallet_entrypoint,
        wallet_action_entrypoint,
        airdrop_raw_amount,
        airdrop_hog: spec.airdrop_hog.clone(),
        initial_sponsor_count: actor_states.len(),
        preferred_create_sponsor_index,
        preferred_airdrop_sponsor_index,
    })
}

struct ProvisionPlan {
    wallet_contract: Contract,
    wallet_contract_hash: SHA256Hash,
    create_wallet_entrypoint: ContractEntrypointID,
    wallet_action_entrypoint: ContractEntrypointID,
    airdrop_raw_amount: Option<u128>,
    airdrop_hog: Option<String>,
    initial_sponsor_count: usize,
    preferred_create_sponsor_index: Option<usize>,
    preferred_airdrop_sponsor_index: Option<usize>,
}

async fn provision_one_actor<C>(
    client: C,
    inputs: &ActorProvisionInputs<'_>,
    plan: &ProvisionPlan,
    actor_states: &mut Vec<ActorState>,
    event_writer: &mut EventWriter,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let actor_id = u32::try_from(actor_states.len())
        .map_err(|_| SimError::Executor("provisioned actor id does not fit in u32".into()))?;
    let account_index = next_unused_account_index(actor_states)?;
    let signer = derive_provisioned_account_signer(inputs.runtime_manifest, account_index)?;
    let owner_public_key = Ed25519PublicKey::from(signer.verifying_key_bytes());
    let fallback_sponsor_index =
        (actor_states.len() - plan.initial_sponsor_count + 1) % plan.initial_sponsor_count;
    let create_sponsor_index = plan
        .preferred_create_sponsor_index
        .unwrap_or(fallback_sponsor_index);
    let airdrop_sponsor_index = plan
        .preferred_airdrop_sponsor_index
        .unwrap_or(create_sponsor_index);
    let prepared = prepare_wallet_create(
        &actor_states[create_sponsor_index],
        plan.create_wallet_entrypoint,
        &plan.wallet_contract,
        plan.wallet_contract_hash,
        owner_public_key,
        resolve_block_expiry_height(client.clone()).await?,
    )?;
    let mut event = actor_provision_event(
        actor_states,
        actor_id,
        account_index,
        create_sponsor_index,
        airdrop_sponsor_index,
        plan,
        prepared.wallet_address,
    );
    event_writer
        .write(&ResultEvent::ActorProvisionStarted {
            event: event.clone(),
        })
        .await?;
    submit_wallet_create_or_fail(
        client.clone(),
        &mut actor_states[create_sponsor_index],
        &prepared,
        &mut event,
        event_writer,
    )
    .await?;
    let nonce_namespace = actor_states[create_sponsor_index].nonce_namespace;
    actor_states.push(ActorState::new(
        actor_id,
        SimAccount {
            index: account_index,
            signer,
            wallet_address: prepared.wallet_address,
        },
        nonce_namespace,
    ));
    submit_airdrop_if_configured(
        client,
        &mut actor_states[airdrop_sponsor_index],
        plan,
        prepared.wallet_address,
        &mut event,
        event_writer,
    )
    .await?;
    event_writer
        .write(&ResultEvent::ActorProvisionCompleted { event })
        .await?;
    Ok(())
}

fn next_unused_account_index(actor_states: &[ActorState]) -> Result<u32, SimError> {
    let used = actor_states
        .iter()
        .map(|actor| actor.account_index)
        .collect::<HashSet<_>>();
    let mut index = 0_u32;
    while used.contains(&index) {
        index = index.checked_add(1).ok_or_else(|| {
            SimError::Executor("no unused runtime derivation account index is available".into())
        })?;
    }
    Ok(index)
}

fn actor_provision_event(
    actor_states: &[ActorState],
    actor_id: u32,
    account_index: u32,
    create_sponsor_index: usize,
    airdrop_sponsor_index: usize,
    plan: &ProvisionPlan,
    wallet_address: ContractAddress,
) -> ActorProvisionEvent {
    ActorProvisionEvent {
        actor_id,
        account_index,
        sponsor_actor_id: actor_states[create_sponsor_index].actor_id,
        airdrop_sponsor_actor_id: plan
            .airdrop_raw_amount
            .is_some()
            .then_some(actor_states[airdrop_sponsor_index].actor_id),
        wallet_address: format_contract_address(&wallet_address),
        create_tx_hash: None,
        create_height: None,
        airdrop_tx_hash: None,
        airdrop_height: None,
        airdrop_hog: plan.airdrop_hog.clone(),
        error: None,
    }
}

async fn submit_wallet_create_or_fail<C>(
    client: C,
    sponsor: &mut ActorState,
    prepared: &PreparedWalletCreate,
    event: &mut ActorProvisionEvent,
    event_writer: &mut EventWriter,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    match submit_create_wallet(client, sponsor, prepared).await {
        Ok((tx_hash, receipt)) => {
            event.create_tx_hash = Some(json_string(&tx_hash, "txHash")?);
            event.create_height = Some(*receipt.height);
            Ok(())
        }
        Err(error) => {
            if let Some(tx_hash) = error.tx_hash {
                event.create_tx_hash = Some(json_string(&tx_hash, "txHash")?);
            }
            event.error = Some(error.message);
            event_writer
                .write(&ResultEvent::ActorProvisionFailed {
                    event: event.clone(),
                })
                .await?;
            Err(SimError::Executor(
                "actor provisioning wallet create failed".into(),
            ))
        }
    }
}

async fn submit_airdrop_if_configured<C>(
    client: C,
    sponsor: &mut ActorState,
    plan: &ProvisionPlan,
    wallet_address: ContractAddress,
    event: &mut ActorProvisionEvent,
    event_writer: &mut EventWriter,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let Some(raw_amount) = plan.airdrop_raw_amount else {
        return Ok(());
    };
    match submit_airdrop(
        client,
        sponsor,
        plan.wallet_action_entrypoint,
        wallet_address,
        raw_amount,
    )
    .await
    {
        Ok((tx_hash, receipt)) => {
            event.airdrop_tx_hash = Some(json_string(&tx_hash, "txHash")?);
            event.airdrop_height = Some(*receipt.height);
            Ok(())
        }
        Err(error) => {
            if let Some(tx_hash) = error.tx_hash {
                event.airdrop_tx_hash = Some(json_string(&tx_hash, "txHash")?);
            }
            event.error = Some(error.message);
            event_writer
                .write(&ResultEvent::ActorProvisionFailed {
                    event: event.clone(),
                })
                .await?;
            Err(SimError::Executor(
                "actor provisioning airdrop failed".into(),
            ))
        }
    }
}

fn select_sponsor_actor_index(
    actor_states: &[ActorState],
    runtime_manifest: &RuntimeManifest,
    preferred_roles: &[&str],
) -> Option<usize> {
    preferred_roles.iter().find_map(|role| {
        let account = runtime_manifest
            .bootstrap_accounts
            .iter()
            .find(|account| account.role == *role)?;
        actor_states
            .iter()
            .position(|actor| actor.account_index == account.index)
    })
}

pub fn actor_provisioning_enabled(manifest: &SimulationManifest) -> bool {
    manifest
        .bootstrap
        .as_ref()
        .and_then(|bootstrap| bootstrap.actor_provisioning.as_ref())
        .is_some_and(|spec| spec.enabled)
}

struct PreparedWalletCreate {
    tx_hex: String,
    wallet_address: ContractAddress,
}

fn prepare_wallet_create(
    sponsor: &ActorState,
    create_wallet_entrypoint: ContractEntrypointID,
    wallet_contract: &Contract,
    wallet_contract_hash: SHA256Hash,
    owner_public_key: Ed25519PublicKey,
    block_expiry_height: i64,
) -> Result<PreparedWalletCreate, SimError> {
    let nonce = sponsor.current_nonce()?;
    let deploy_nonce = deploy_nonce_from_u64(nonce);
    let wallet_address = derive_deploy_address(
        &sponsor.wallet_address,
        &deploy_nonce,
        &wallet_contract_hash,
    )
    .map_err(|source| SimError::Executor(source.to_string()))?;
    let calldata = xdrs::cxs::encode(&(&deploy_nonce, &wallet_contract_hash, &owner_public_key))
        .map_err(|error| {
            SimError::Executor(format!("failed to encode wallet create calldata: {error}"))
        })?;
    let tx = build_deploy_transaction(BuildDeployTransaction {
        invoked_address: sponsor.wallet_address,
        entrypoint: create_wallet_entrypoint,
        calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce,
        contracts_to_deploy: VecM(vec![wallet_contract.clone()]),
        auth_claim: source_address_auth_claim(),
    });
    let tx_hex =
        sign_and_encode_transaction_hex(tx, &sponsor.signer, SignTransactionParams::default())
            .map_err(|source| SimError::Executor(source.to_string()))?;
    Ok(PreparedWalletCreate {
        tx_hex,
        wallet_address,
    })
}

async fn submit_create_wallet<C>(
    client: C,
    sponsor: &mut ActorState,
    prepared: &PreparedWalletCreate,
) -> Result<(TxHash, TransactionReceipt), ProvisionTxError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let tx_hash = client
        .send_raw_transaction(prepared.tx_hex.clone())
        .await
        .map_err(|error| {
            ProvisionTxError::without_hash(format!("wallet create submit failed: {error}"))
        })?;
    sponsor
        .consume_setup_nonce()
        .map_err(|error| ProvisionTxError::with_hash(tx_hash, error.to_string()))?;
    let receipt = poll_receipt(client, tx_hash, "wallet create")
        .await
        .map_err(|message| ProvisionTxError::with_hash(tx_hash, message))?;
    if !matches!(receipt.status, ExecStatus::Ok) {
        return Err(ProvisionTxError::with_hash(
            tx_hash,
            format!(
                "wallet create receipt status {}",
                exec_status_name(&receipt.status)
            ),
        ));
    }
    Ok((tx_hash, receipt))
}

async fn submit_airdrop<C>(
    client: C,
    sponsor: &mut ActorState,
    wallet_entrypoint: ContractEntrypointID,
    recipient: ContractAddress,
    raw_amount: u128,
) -> Result<(TxHash, TransactionReceipt), ProvisionTxError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let nonce = sponsor
        .current_nonce()
        .map_err(|error| ProvisionTxError::without_hash(error.to_string()))?;
    let block_expiry_height = resolve_block_expiry_height(client.clone())
        .await
        .map_err(|error| ProvisionTxError::without_hash(error.to_string()))?;
    let calldata = xdrs::cxs::encode(&(&recipient, raw_amount)).map_err(|error| {
        ProvisionTxError::without_hash(format!("failed to encode airdrop calldata: {error}"))
    })?;
    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: sponsor.wallet_address,
        wallet_entrypoint,
        target_address: ContractAddress::from(HOG_TOKEN_ADDRESS),
        target_entrypoint: hog_transfer_entrypoint(),
        target_calldata: &calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        target_gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce,
    })
    .map_err(|error| {
        ProvisionTxError::without_hash(format!("failed to build airdrop transaction: {error}"))
    })?;
    let tx_hex =
        sign_and_encode_transaction_hex(tx, &sponsor.signer, SignTransactionParams::default())
            .map_err(|error| {
                ProvisionTxError::without_hash(format!(
                    "failed to sign airdrop transaction: {error}"
                ))
            })?;
    let tx_hash = client.send_raw_transaction(tx_hex).await.map_err(|error| {
        ProvisionTxError::without_hash(format!("airdrop submit failed: {error}"))
    })?;
    sponsor
        .consume_setup_nonce()
        .map_err(|error| ProvisionTxError::with_hash(tx_hash, error.to_string()))?;
    let receipt = poll_receipt(client, tx_hash, "airdrop")
        .await
        .map_err(|message| ProvisionTxError::with_hash(tx_hash, message))?;
    if !matches!(receipt.status, ExecStatus::Ok) {
        return Err(ProvisionTxError::with_hash(
            tx_hash,
            format!(
                "airdrop receipt status {}",
                exec_status_name(&receipt.status)
            ),
        ));
    }
    Ok((tx_hash, receipt))
}

fn hog_transfer_entrypoint() -> ContractEntrypointID {
    let selector = contract_entrypoint_id(
        Some("ERC20"),
        "0.1.0",
        &NormalizedMethodSignature::new("transfer"),
    );
    let bytes: [u8; 8] = selector
        .as_ref()
        .try_into()
        .expect("bug: contract entrypoint selectors are always eight bytes");
    ContractEntrypointID::from(bytes)
}

struct ProvisionTxError {
    message: String,
    tx_hash: Option<TxHash>,
}

impl ProvisionTxError {
    fn without_hash(message: String) -> Self {
        Self {
            message,
            tx_hash: None,
        }
    }

    fn with_hash(tx_hash: TxHash, message: String) -> Self {
        Self {
            message,
            tx_hash: Some(tx_hash),
        }
    }
}

async fn resolve_block_expiry_height<C>(client: C) -> Result<i64, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let tip = client.chain_tip().await.map_err(|error| {
        SimError::Executor(format!("actor provisioning chain tip failed: {error}"))
    })?;
    Ok((*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET))
}

async fn poll_receipt<C>(
    client: C,
    tx_hash: TxHash,
    label: &'static str,
) -> Result<TransactionReceipt, String>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let deadline = Instant::now()
        .checked_add(PROVISION_RECEIPT_TIMEOUT)
        .unwrap_or_else(Instant::now);
    loop {
        if let Some(receipt) = client
            .transaction_receipt(tx_hash)
            .await
            .map_err(|error| format!("{label} receipt poll failed: {error}"))?
        {
            return Ok(receipt);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("{label} receipt timed out"));
        }
        tokio::time::sleep(DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL.min(remaining)).await;
    }
}

fn resolve_dev_wallet_artifact(
    spec: &ActorProvisioningSpec,
    runtime_manifest: &RuntimeManifest,
    runtime_manifest_path: &Path,
    scenario_path: &Path,
    contract_artifact_root: Option<&Path>,
) -> Result<PathBuf, SimError> {
    let explicit_hint = spec.dev_wallet_artifact.as_deref();
    let hint = explicit_hint
        .unwrap_or_else(|| Path::new(&runtime_manifest.bootstrap.dev_wallet.artifact_hint));
    let mut candidates = Vec::new();
    if hint.is_absolute() {
        push_candidate(&mut candidates, hint.to_path_buf());
    } else {
        if let Some(root) = contract_artifact_root {
            push_candidate(&mut candidates, root.join(hint));
            if let Some(file_name) = hint.file_name() {
                push_candidate(&mut candidates, root.join(file_name));
            }
        }
        if explicit_hint.is_some() {
            if let Some(parent) = scenario_path.parent() {
                push_candidate(&mut candidates, parent.join(hint));
            }
            if let Some(parent) = runtime_manifest_path.parent() {
                push_candidate(&mut candidates, parent.join(hint));
            }
        } else {
            if let Some(parent) = runtime_manifest_path.parent() {
                push_candidate(&mut candidates, parent.join(hint));
            }
            if let Some(parent) = scenario_path.parent() {
                push_candidate(&mut candidates, parent.join(hint));
            }
        }
    }

    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            SimError::Executor(format!(
                "dev-wallet artifact `{}` was not found for actor provisioning",
                hint.display()
            ))
        })
}

fn push_candidate(candidates: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidates.iter().any(|existing| existing == &candidate) {
        candidates.push(candidate);
    }
}

fn load_dev_wallet_contract(
    path: &Path,
    artifact_kind: ContractArtifactKind,
) -> Result<Contract, SimError> {
    let bytes = std::fs::read(path)?;
    let blob = ContractBlob {
        data: BytesM(bytes),
    };
    match artifact_kind {
        ContractArtifactKind::Rv32 => Ok(Contract::kRv32 {
            rv32_contract: blob,
        }),
        ContractArtifactKind::Wasm => Ok(Contract::kWasmi {
            wasmi_contract: blob,
        }),
    }
}

fn infer_dev_wallet_artifact_kind(path: &Path) -> ContractArtifactKind {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rv32a") => ContractArtifactKind::Rv32,
        _ => ContractArtifactKind::Wasm,
    }
}

fn contract_hash(contract: &Contract) -> Result<SHA256Hash, SimError> {
    let encoded = xdrs::txs::encode(contract).map_err(|error| {
        SimError::Executor(format!("failed to encode wallet contract: {error}"))
    })?;
    let bytes: [u8; 32] = Sha256::digest(encoded).into();
    Ok(SHA256Hash::from(bytes))
}

fn deploy_nonce_from_u64(nonce: u64) -> SHA256Hash {
    let bytes: [u8; 32] = Sha256::digest(nonce.to_le_bytes()).into();
    SHA256Hash::from(bytes)
}

fn parse_hog_amount(input: &str) -> Result<u128, SimError> {
    let input = input.trim();
    if input.is_empty() || input.starts_with('-') || input.starts_with('+') {
        return Err(SimError::Executor(
            "actor provisioning airdropHog must be a positive HOG amount".into(),
        ));
    }
    let mut parts = input.split('.');
    let whole = parts.next().unwrap_or_default();
    let fractional = parts.next();
    if parts.next().is_some() || whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return Err(SimError::Executor(format!(
            "invalid actor provisioning airdropHog amount `{input}`"
        )));
    }
    let whole = whole.parse::<u128>().map_err(|error| {
        SimError::Executor(format!(
            "invalid actor provisioning airdropHog amount `{input}`: {error}"
        ))
    })?;
    let fractional = match fractional {
        Some(value) => parse_fractional_hog(input, value)?,
        None => 0,
    };
    let raw = whole
        .checked_mul(HOG_SCALE)
        .and_then(|value| value.checked_add(fractional))
        .ok_or_else(|| {
            SimError::Executor(format!(
                "actor provisioning airdropHog amount `{input}` is too large"
            ))
        })?;
    if raw == 0 {
        return Err(SimError::Executor(
            "actor provisioning airdropHog must be greater than zero".into(),
        ));
    }
    Ok(raw)
}

fn parse_fractional_hog(input: &str, fractional: &str) -> Result<u128, SimError> {
    if fractional.is_empty()
        || fractional.len() > HOG_DECIMALS
        || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err(SimError::Executor(format!(
            "invalid actor provisioning airdropHog amount `{input}`; at most {HOG_DECIMALS} decimal places are supported"
        )));
    }
    let mut padded = fractional.to_owned();
    while padded.len() < HOG_DECIMALS {
        padded.push('0');
    }
    padded.parse::<u128>().map_err(|error| {
        SimError::Executor(format!(
            "invalid actor provisioning airdropHog amount `{input}`: {error}"
        ))
    })
}

fn format_contract_address(address: &ContractAddress) -> String {
    format!("0x{}", hex::encode(address.as_ref()))
}

fn exec_status_name(status: &ExecStatus) -> &'static str {
    match status {
        ExecStatus::Ok => "ok",
        ExecStatus::Reverted { .. } => "reverted",
        ExecStatus::DeterministicError { .. } => "deterministic-error",
        ExecStatus::NonDeterministicError => "non-deterministic-error",
        ExecStatus::FailedGasSponsorship => "failed-gas-sponsorship",
        ExecStatus::Other => "other",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::contract::Contract;
    use groundhog_xdr::types::ContractAddress;
    use serde_json::json;

    use super::{
        hog_transfer_entrypoint, load_dev_wallet_contract, next_unused_account_index,
        parse_hog_amount, resolve_dev_wallet_artifact,
    };
    use crate::actor::ActorState;
    use crate::manifest::{ActorProvisioningSpec, ContractArtifactKind};
    use crate::nonce::NonceNamespace;
    use crate::runtime::SimAccount;

    #[test]
    fn parses_airdrop_hog_amounts() {
        assert_eq!(parse_hog_amount("1").unwrap(), 10_000_000);
        assert_eq!(parse_hog_amount("1.25").unwrap(), 12_500_000);
        assert!(parse_hog_amount("0").is_err());
        assert!(parse_hog_amount("1.12345678").is_err());
    }

    #[test]
    fn hog_transfer_entrypoint_uses_canonical_erc20_identity() {
        assert_eq!(
            hog_transfer_entrypoint().as_ref(),
            &0x6328_5b35_23f9_dc3f_u64.to_be_bytes()
        );
    }

    #[test]
    fn resolves_default_artifact_hint_relative_to_runtime_manifest() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let runtime_dir = temp.path().join("runtime");
        let scenario_dir = temp.path().join("scenarios");
        std::fs::create_dir_all(runtime_dir.join("contracts")).expect("runtime dirs should create");
        std::fs::create_dir_all(&scenario_dir).expect("scenario dir should create");
        let artifact = runtime_dir.join("contracts/dev-wallet.wasm");
        std::fs::write(&artifact, b"wallet").expect("artifact should write");

        let resolved = resolve_dev_wallet_artifact(
            &ActorProvisioningSpec {
                enabled: true,
                dev_wallet_artifact: None,
                dev_wallet_artifact_kind: None,
                airdrop_hog: None,
            },
            &runtime_manifest("contracts/dev-wallet.wasm"),
            &runtime_dir.join("runtime-manifest.json"),
            &scenario_dir.join("scenario.json"),
            None,
        )
        .expect("runtime-relative artifact should resolve");

        assert_eq!(resolved, artifact);
    }

    #[test]
    fn resolves_explicit_artifact_hint_relative_to_scenario() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let runtime_dir = temp.path().join("runtime");
        let scenario_dir = temp.path().join("scenarios");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir should create");
        std::fs::create_dir_all(&scenario_dir).expect("scenario dir should create");
        let artifact = scenario_dir.join("dev-wallet.wasm");
        std::fs::write(&artifact, b"wallet").expect("artifact should write");

        let resolved = resolve_dev_wallet_artifact(
            &ActorProvisioningSpec {
                enabled: true,
                dev_wallet_artifact: Some("dev-wallet.wasm".into()),
                dev_wallet_artifact_kind: None,
                airdrop_hog: None,
            },
            &runtime_manifest("missing-runtime-wallet.wasm"),
            &runtime_dir.join("runtime-manifest.json"),
            &scenario_dir.join("scenario.json"),
            None,
        )
        .expect("scenario-relative artifact should resolve");

        assert_eq!(resolved, artifact);
    }

    #[test]
    fn explicit_artifact_kind_overrides_wallet_filename_suffix() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let artifact = temp.path().join("dev-wallet.bin");
        std::fs::write(&artifact, b"rv32 wallet").expect("artifact should write");

        let contract = load_dev_wallet_contract(&artifact, ContractArtifactKind::Rv32)
            .expect("rv32 artifact should load");

        assert!(matches!(contract, Contract::kRv32 { .. }));
    }

    #[test]
    fn account_index_allocation_skips_sparse_runtime_indices() {
        let actors = vec![actor_state(0, 0), actor_state(1, 3)];

        let next = next_unused_account_index(&actors).expect("unused index should exist");

        assert_eq!(next, 1);
    }

    fn runtime_manifest(artifact_hint: &str) -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001"
            },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": "seed"
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": artifact_hint,
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0x646f416374696f6e"
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

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }

    fn actor_state(actor_id: u32, account_index: u32) -> ActorState {
        let address_byte = u8::try_from(account_index).expect("test account index should fit u8");
        ActorState::new(
            actor_id,
            SimAccount {
                index: account_index,
                signer: derive_dev_account_signer("seed", account_index as usize),
                wallet_address: ContractAddress::from([address_byte; 32]),
            },
            NonceNamespace::new(1).expect("namespace should fit"),
        )
    }
}
