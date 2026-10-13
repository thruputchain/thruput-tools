//! Derive missing native actor accounts and explicitly fund them from the bootstrap faucet.
use crate::util::json_string;
use crate::{
    actor::{ActorState, required_actor_count},
    error::SimError,
    event::{ActorProvisionEvent, EventWriter, ResultEvent},
    manifest::SimulationManifest,
    rpc::{ReceiptWaitFailure, SimReceiptSource, SimRpcClient, receipt_poll_config},
    runtime::{SimAccount, derive_provisioned_account_signer},
};
use std::collections::HashSet;
use std::time::Duration;
use thruput_json_rpc_client::poll_receipt;
use thruput_json_rpc_types::{
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    transaction::TxHash,
};
use thruput_signing::{
    DEFAULT_TX_GAS_LIMIT, PubkeyAccount, SignTransactionParams, sign_and_encode_transaction_hex,
};
use thruput_xdr::types::ContractAddress;
const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const PROVISION_RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) struct ActorProvisionInputs<'a> {
    pub(crate) manifest: &'a SimulationManifest,
    pub(crate) runtime_manifest: &'a RuntimeManifest,
}
struct ProvisionPlan {
    airdrop_raw_amount: Option<u128>,
    airdrop_hog: Option<String>,
}
pub(crate) async fn provision_missing_actor_accounts<C>(
    client: C,
    inputs: ActorProvisionInputs<'_>,
    sponsor: Option<&mut ActorState>,
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
    let sponsor = sponsor.ok_or_else(|| {
        SimError::Executor(
            "actor provisioning requires runtime bootstrap account 0 with role `faucet`".into(),
        )
    })?;

    let plan = ProvisionPlan {
        airdrop_raw_amount: spec
            .airdrop_hog
            .as_deref()
            .map(parse_hog_amount)
            .transpose()?,
        airdrop_hog: spec.airdrop_hog.clone(),
    };
    while actor_states.len() < required {
        provision_one_actor(
            client.clone(),
            &inputs,
            &plan,
            sponsor,
            actor_states,
            event_writer,
        )
        .await?;
    }

    Ok(())
}

async fn provision_one_actor<C>(
    client: C,
    inputs: &ActorProvisionInputs<'_>,
    plan: &ProvisionPlan,
    sponsor: &mut ActorState,
    actor_states: &mut Vec<ActorState>,
    event_writer: &mut EventWriter,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let actor_id = u32::try_from(actor_states.len())
        .map_err(|_| SimError::Executor("provisioned actor id does not fit in u32".into()))?;
    let account_index = next_unused_account_index(actor_states, sponsor.account_index)?;
    let signer = derive_provisioned_account_signer(inputs.runtime_manifest, account_index)?;
    let wallet_address =
        PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address();
    let mut event = actor_provision_event(
        actor_id,
        account_index,
        sponsor.account_index,
        plan,
        wallet_address,
    );
    event_writer
        .write(&ResultEvent::ActorProvisionStarted {
            event: event.clone(),
        })
        .await?;
    let nonce_namespace = sponsor.nonce_namespace;
    actor_states.push(ActorState::new(
        actor_id,
        SimAccount {
            index: account_index,
            signer,
            wallet_address,
            network_hash: sponsor.network_hash.clone(),
        },
        nonce_namespace,
    ));
    submit_airdrop_if_configured(
        client,
        sponsor,
        plan,
        wallet_address,
        &mut event,
        event_writer,
    )
    .await?;
    event_writer
        .write(&ResultEvent::ActorProvisionCompleted { event })
        .await?;
    Ok(())
}

fn next_unused_account_index(
    actor_states: &[ActorState],
    sponsor_account_index: u32,
) -> Result<u32, SimError> {
    let mut used = actor_states
        .iter()
        .map(|actor| actor.account_index)
        .collect::<HashSet<_>>();
    used.insert(sponsor_account_index);
    let mut index = 0_u32;
    while used.contains(&index) {
        index = index.checked_add(1).ok_or_else(|| {
            SimError::Executor("no unused runtime derivation account index is available".into())
        })?;
    }
    Ok(index)
}

fn actor_provision_event(
    actor_id: u32,
    account_index: u32,
    sponsor_account_index: u32,
    plan: &ProvisionPlan,
    wallet_address: ContractAddress,
) -> ActorProvisionEvent {
    ActorProvisionEvent {
        actor_id,
        account_index,
        sponsor_account_index,
        airdrop_sponsor_account_index: plan
            .airdrop_raw_amount
            .is_some()
            .then_some(sponsor_account_index),
        wallet_address: format_contract_address(&wallet_address),
        create_tx_hash: None,
        create_height: None,
        airdrop_tx_hash: None,
        airdrop_height: None,
        airdrop_hog: plan.airdrop_hog.clone(),
        error: None,
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
    match submit_airdrop(client, sponsor, wallet_address, raw_amount).await {
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

pub fn actor_provisioning_enabled(manifest: &SimulationManifest) -> bool {
    manifest
        .bootstrap
        .as_ref()
        .and_then(|bootstrap| bootstrap.actor_provisioning.as_ref())
        .is_some_and(|spec| spec.enabled)
}

async fn submit_airdrop<C>(
    client: C,
    sponsor: &mut ActorState,
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
    let account = PubkeyAccount::from_public_key_bytes(sponsor.signer.verifying_key_bytes(), 0);
    let tx = thruput_signing::default_wallet::build_default_wallet_call_transaction(
        thruput_signing::default_wallet::BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: thruput_signing::NATIVE_HOG_ADDRESS,
            target_entrypoint: thruput_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
            target_calldata: &calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT * 2,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce,
        },
    )
    .map_err(|error| {
        ProvisionTxError::without_hash(format!("failed to build airdrop transaction: {error}"))
    })?;
    let tx_hash = TxHash(thruput_json_rpc_types::hex::Hex0x(
        thruput_signing::transaction_hash(&tx)
            .map_err(|error| ProvisionTxError::without_hash(error.to_string()))?,
    ));
    let tx_hex = sign_and_encode_transaction_hex(
        tx,
        &sponsor.signer,
        SignTransactionParams::default(),
        &sponsor.network_hash,
    )
    .map_err(|error| {
        ProvisionTxError::without_hash(format!("failed to sign airdrop transaction: {error}"))
    })?;
    sponsor
        .consume_setup_nonce()
        .map_err(|error| ProvisionTxError::with_hash(tx_hash, error.to_string()))?;
    let returned_hash = client.send_raw_transaction(tx_hex).await.map_err(|error| {
        ProvisionTxError::with_hash(tx_hash, format!("airdrop submit failed: {error}"))
    })?;
    if returned_hash != tx_hash {
        return Err(ProvisionTxError::with_hash(
            tx_hash,
            "airdrop submit returned a different transaction hash".into(),
        ));
    }
    let receipt = poll_receipt(
        &SimReceiptSource(&client),
        tx_hash,
        receipt_poll_config(PROVISION_RECEIPT_TIMEOUT),
    )
    .await
    .map_err(|source| ReceiptWaitFailure::step("airdrop", source))
    .map_err(|error| ProvisionTxError::with_hash(tx_hash, error.to_string()))?;
    if receipt.tx_hash != tx_hash {
        return Err(ProvisionTxError::with_hash(
            tx_hash,
            "airdrop receipt returned a different transaction hash".into(),
        ));
    }
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

fn parse_hog_amount(input: &str) -> Result<u128, SimError> {
    thruput_signing::parse_hog_amount(input).map_err(|error| {
        SimError::Executor(format!(
            "invalid actor provisioning airdropHog amount `{}`: {error}",
            input.trim()
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
    use super::*;
    #[derive(Clone, Default)]
    struct RecordingClient(
        std::sync::Arc<std::sync::Mutex<Vec<thruput_xdr::transaction::SignedTransaction>>>,
        SubmissionOutcome,
    );
    #[derive(Clone, Copy, Default)]
    enum SubmissionOutcome {
        #[default]
        Success,
        TransportError,
        WrongSubmitHash,
        WrongReceiptHash,
    }
    impl SimRpcClient for RecordingClient {
        async fn chain_tip(
            &self,
        ) -> Result<thruput_json_rpc_types::block::ChainTip, crate::rpc::SimRpcError> {
            Ok(thruput_json_rpc_types::block::ChainTip {
                height: 42.into(),
                block_hash: thruput_json_rpc_types::hex::Hex0x([0; 32].into()),
            })
        }
        async fn send_raw_transaction(
            &self,
            tx_hex: String,
        ) -> Result<TxHash, crate::rpc::SimRpcError> {
            let signed: thruput_xdr::transaction::SignedTransaction =
                xdrs::txs::decode(&hex::decode(tx_hex.trim_start_matches("0x")).unwrap()).unwrap();
            let hash = thruput_signing::transaction_hash(&signed.tx).unwrap();
            self.0.lock().unwrap().push(signed);
            match self.1 {
                SubmissionOutcome::TransportError => Err(crate::rpc::SimRpcError::Rpc {
                    method: "sendRawTransaction",
                    source: jsonrpsee::core::client::Error::RequestTimeout,
                }),
                SubmissionOutcome::WrongSubmitHash => Ok(TxHash(
                    thruput_json_rpc_types::hex::Hex0x([0xaa; 32].into()),
                )),
                _ => Ok(TxHash(thruput_json_rpc_types::hex::Hex0x(hash))),
            }
        }
        async fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> Result<Option<TransactionReceipt>, crate::rpc::SimRpcError> {
            let tx_hash = if matches!(self.1, SubmissionOutcome::WrongReceiptHash) {
                TxHash(thruput_json_rpc_types::hex::Hex0x([0xaa; 32].into()))
            } else {
                tx_hash
            };
            Ok(Some(TransactionReceipt {
                tx_hash,
                height: 43.into(),
                status: ExecStatus::Ok,
                events: vec![],
                return_data: None,
                abort_data: None,
                gas_used: 100,
            }))
        }
    }
    fn actor(index: u32) -> ActorState {
        let signer = thruput_signing::derive_default_dev_account_signer(index as usize);
        let wallet_address =
            PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address();
        ActorState::new(
            0,
            SimAccount {
                index,
                signer,
                wallet_address,
                network_hash: thruput_xdr::types::NetworkHash::new([0; 32].into()),
            },
            crate::nonce::NonceNamespace::new(1).unwrap(),
        )
    }
    #[tokio::test]
    async fn funding_reserves_nonce_and_preserves_local_hash_for_uncertain_or_mismatched_results() {
        for outcome in [
            SubmissionOutcome::TransportError,
            SubmissionOutcome::WrongSubmitHash,
            SubmissionOutcome::WrongReceiptHash,
        ] {
            let client = RecordingClient(std::sync::Arc::default(), outcome);
            let mut sponsor = actor(0);
            let error =
                submit_airdrop(client.clone(), &mut sponsor, actor(1).wallet_address, 12345)
                    .await
                    .unwrap_err();
            let submitted = client.0.lock().unwrap();
            assert_eq!(submitted.len(), 1);
            let local_hash = TxHash(thruput_json_rpc_types::hex::Hex0x(
                thruput_signing::transaction_hash(&submitted[0].tx).unwrap(),
            ));
            assert_eq!(error.tx_hash, Some(local_hash));
            assert_eq!(sponsor.next_nonce_sequence, 1);
        }
    }
    #[tokio::test]
    async fn provisioning_derives_native_accounts_and_only_submits_explicit_funding() {
        type Call = (
            ContractAddress,
            thruput_xdr::contract::ContractEntrypointID,
            u64,
            Vec<u8>,
        );

        for funding in [None, Some("1.0000001")] {
            let mut manifest: SimulationManifest = serde_json::from_str(include_str!(
                "../../../examples/sim/local-wallet-submit-smoke.json"
            ))
            .unwrap();
            manifest.load_shape[0].users = 3;
            manifest.bootstrap = Some(serde_json::from_value(serde_json::json!({
                "accounts":1,"accountSeed":"groundhog-dev-accounts","initialStorageTokens":"1000000000","storageCostProfile":"hog-devnode-default",
                "actorProvisioning":{"enabled":true,"airdropHog":funding}
            })).unwrap());
            let runtime: RuntimeManifest = serde_json::from_str(include_str!(
                "../../testkit/tests/fixtures/hosted-runtime.json"
            ))
            .unwrap();
            let client = RecordingClient::default();
            let mut sponsor = actor(0);
            let mut actors = vec![actor(1)];
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut writer = EventWriter::create(file.path()).await.unwrap();
            provision_missing_actor_accounts(
                client.clone(),
                ActorProvisionInputs {
                    manifest: &manifest,
                    runtime_manifest: &runtime,
                },
                Some(&mut sponsor),
                &mut actors,
                &mut writer,
            )
            .await
            .unwrap();
            assert_eq!(actors.len(), 3);
            for (index, actor) in actors.iter().enumerate() {
                assert_eq!(actor.account_index, u32::try_from(index + 1).unwrap());
                assert_eq!(
                    actor.wallet_address,
                    PubkeyAccount::from_public_key_bytes(actor.signer.verifying_key_bytes(), 0)
                        .address()
                );
            }
            let submitted = client.0.lock().unwrap();
            assert_eq!(submitted.len(), if funding.is_some() { 2 } else { 0 });
            assert_eq!(sponsor.next_nonce_sequence as usize, submitted.len());
            for (index, signed) in submitted.iter().enumerate() {
                assert_eq!(signed.tx.invocation.invoked_address, sponsor.wallet_address);
                assert_eq!(
                    signed.tx.invocation.entrypoint,
                    thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT
                );
                let calls: Vec<Call> = xdrs::cxs::decode(&signed.tx.invocation.calldata).unwrap();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].0, thruput_signing::NATIVE_HOG_ADDRESS);
                assert_eq!(calls[0].1, thruput_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT);
                let (recipient, amount): (ContractAddress, u128) =
                    xdrs::cxs::decode(&calls[0].3).unwrap();
                assert_eq!(recipient, actors[index + 1].wallet_address);
                assert_eq!(amount, 10_000_001);
            }
            let events = std::fs::read_to_string(file.path()).unwrap();
            assert_eq!(events.matches("actor-provision-completed").count(), 2);
            assert!(!events.contains("createTxHash"));
        }
    }

    #[test]
    fn funding_amount_is_exact() {
        assert_eq!(parse_hog_amount("1.0000001").unwrap(), 10_000_001);
        assert!(parse_hog_amount("0.00000001").is_err());
    }
}
