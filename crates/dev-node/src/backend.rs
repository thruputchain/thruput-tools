//! Dev node backend — owns `TestEnvironment` and in-memory state.
//!
//! Lives on a dedicated `std::thread`. Never crosses thread boundaries.

use std::collections::{HashMap, VecDeque};

use groundhog_native_exec::test_environment::{TestEnvironment, TestEnvironmentHandle};
use groundhog_native_exec::transaction_handler::{RootInvokeResult, TransactionHandler};
use groundhog_rpc_api_types::BlockHeight;
use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_api_types::dev::{
    DevAccount, StorageItemInfo, TransactionDetail, configured_gas_limit, format_gas_summary,
    format_i128_xdr, int128_to_i128,
};
use groundhog_rpc_api_types::events::{EmittedEvent, IdentifiedEvent};
use groundhog_rpc_api_types::hex::{Hex0x, HexBytes, Topic};
use groundhog_rpc_api_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
use groundhog_signing::{DEFAULT_TX_GAS_LIMIT, WALLET_DEPLOY_ENTRYPOINT, derive_deploy_address};
use groundhog_trace::InvocationFrameTrace;
use groundhog_xdr::event::TxEmittedEvent;
use groundhog_xdr::exec_result::ExecResult;
use groundhog_xdr::storage_item::{FlaggedStorageDelta, StorageDelta, StorageDeltaFlags};
use groundhog_xdr::transaction::{InvocationFrameOutput, SignedTransaction};
use groundhog_xdr::types::{ContractAddress, SHA256Hash, StorageKeyContractData};
use sha2::{Digest, Sha256};

use crate::accounts;
use crate::error::DevNodeError;

#[derive(Debug, Clone)]
struct TransactionRecord {
    receipt: TransactionReceipt,
    gas_consumed: u64,
    gas_limit: Option<u64>,
    storage_deltas: Vec<FlaggedStorageDelta>,
    syscall_trace: Option<InvocationFrameTrace>,
}

/// In-memory dev node state.
pub struct DevBackend {
    env: TestEnvironmentHandle,
    transactions: HashMap<TxHash, TransactionRecord>,
    committed_heights: HashMap<TxHash, BlockHeight>,
    transaction_order: VecDeque<TxHash>,
    events: Vec<IdentifiedEvent>,
    tip_height: i64,
    block_txs: Vec<Vec<TxHash>>,
    accounts: Vec<DevAccount>,
    storage_tokens: u64,
    transaction_retention_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendStats {
    pub block_count: u64,
    pub tx_count: usize,
}

impl DevBackend {
    /// Initialize with pre-funded accounts.
    ///
    /// Replicates Groundhog's genesis setup:
    /// 1. Register the dev-wallet WASM contract
    /// 2. Deploy the wallet at each dev account address and grant storage tokens
    /// 3. Initialize each wallet with its owner's ed25519 public key
    pub fn new(
        account_count: usize,
        storage_tokens: u64,
        transaction_retention_limit: usize,
    ) -> Self {
        tracing::info!(
            account_count,
            storage_tokens,
            transaction_retention_limit,
            "initializing dev node backend"
        );
        let env = TestEnvironment::new(None);
        let accounts = accounts::derive_dev_accounts(account_count, storage_tokens);

        // 1. Register the dev-wallet contract (makes WASM available for deployment).
        let wallet_wasm = crate::wallet::DEV_WALLET_WASM;
        let wallet_contract = groundhog_xdr::contract::Contract::kWasmi {
            wasmi_contract: groundhog_xdr::contract::ContractBlob {
                wasm: xdr_common::BytesM(wallet_wasm.to_vec()),
            },
        };
        let _wallet_hash = env.setup_create_contract(&wallet_contract);

        // 2. Deploy the wallet at each account address and grant storage tokens.
        for account in &accounts {
            let addr = account.address.0;
            env.setup_create_deployment(&addr, &wallet_contract);
            #[allow(clippy::cast_possible_wrap)]
            env.setup_grant_storage_tokens(&addr, storage_tokens as i64);
        }

        // Advance past the deployment block so contracts are visible.
        env.advance_block();

        // 3. Initialize each wallet with its owner's ed25519 public key.
        //    This calls the wallet's `initialize` entrypoint which sets up
        //    AddressAuthData so the wallet accepts ed25519 signatures.
        let initialize_entrypoint = groundhog_xdr::contract::ContractEntrypointID::from(
            0xF4F9_E647_4A30_D2CF_u64.to_be_bytes(),
        );

        for account in &accounts {
            let addr = account.address.0;

            // Encode the public key as calldata (XDR Ed25519PublicKey).
            let pubkey_bytes: [u8; 32] = account
                .public_key
                .0
                .clone()
                .try_into()
                .expect("bug: dev account public key is not 32 bytes");
            let pubkey = groundhog_xdr::types::Ed25519PublicKey::from(pubkey_bytes);
            // Use CXS encoding — this is what the #[contract] macro's
            // generated deserialization expects for calldata.
            let calldata =
                xdrs::cxs::encode(&pubkey).expect("bug: failed to CXS-encode public key");

            // Construct a minimal unsigned transaction to call initialize.
            // Auth is not checked because the wallet has no auth data yet.
            let stx = SignedTransaction {
                signatures: xdr_common::VecM(vec![]),
                tx: groundhog_xdr::transaction::Transaction {
                    uniquifier: groundhog_xdr::transaction::Uniquifier {
                        block_expiry_height: groundhog_xdr::types::BlockHeight::from(i64::MAX),
                        nonce: 0,
                    },
                    auth_claims: xdr_common::VecM(vec![]),
                    gas_claims: xdr_common::VecM(vec![]),
                    invocation: groundhog_xdr::transaction::Invocation {
                        invoked_address: addr,
                        entrypoint: initialize_entrypoint,
                        gas_limit: DEFAULT_TX_GAS_LIMIT,
                        calldata: xdr_common::BytesM(calldata),
                    },
                    contracts_to_deploy: xdr_common::VecM(vec![]),
                },
            };

            let mut handler = TransactionHandler::new(env.clone(), &stx);
            let result = handler.root_invoke();

            // The root invocation must succeed (Return, not Abort/Fault).
            assert!(
                matches!(
                    result.output,
                    groundhog_xdr::transaction::InvocationFrameOutput::Return { .. }
                ),
                "bug: wallet initialize invocation failed for account {}: {:?}",
                hex::encode(addr.as_ref()),
                result.output,
            );

            let exec = handler.try_commit_transaction();

            assert!(
                exec.0 == groundhog_xdr::exec_result::ExecResult::ExecResultOK.0,
                "bug: wallet initialize commit failed for account {}: exec={exec:?}",
                hex::encode(addr.as_ref()),
            );

            env.advance_block();
        }

        Self {
            env,
            transactions: HashMap::new(),
            committed_heights: HashMap::new(),
            transaction_order: VecDeque::new(),
            events: Vec::new(),
            #[allow(clippy::cast_possible_wrap)]
            tip_height: 1 + account_count as i64,
            block_txs: {
                let mut v = Vec::new();
                v.resize(2 + account_count, vec![]);
                v
            },
            accounts,
            storage_tokens,
            transaction_retention_limit,
        }
    }

    /// Submit, execute, and commit a signed transaction.
    pub fn send_raw_transaction(&mut self, tx_bytes: &[u8]) -> Result<TxHash, DevNodeError> {
        // 1. Deserialize
        let signed_tx: SignedTransaction = xdrs::txs::decode(tx_bytes)
            .map_err(|e| DevNodeError::TxInvalid(format!("invalid XDR: {e}")))?;

        // 2. Compute canonical hash (over the inner Transaction, not SignedTransaction)
        let tx_xdr = xdrs::txs::encode(&signed_tx.tx)
            .map_err(|e| DevNodeError::TxInvalid(format!("cannot re-encode tx: {e}")))?;
        let hash_bytes: [u8; 32] = Sha256::digest(&tx_xdr).into();
        let tx_hash = TxHash(Hex0x(SHA256Hash::from(hash_bytes)));
        tracing::info!(tx_hash = %format_tx_hash(&tx_hash), "received transaction");

        // 3. Verify auth — 0 means success, any other value is an error.
        let auth_result = self.env.verify_signed_transaction_auth(&signed_tx);
        if auth_result.0 != 0 {
            tracing::warn!(
                tx_hash = %format_tx_hash(&tx_hash),
                auth_result = auth_result.0,
                "transaction auth failed"
            );
            return Err(DevNodeError::AuthFailed(format!("{auth_result}")));
        }

        // 4. Register contracts to deploy
        for contract in signed_tx.tx.contracts_to_deploy.iter() {
            self.env.setup_create_contract(contract);
        }

        // Local-dev semantics: wallet deploys should "just work" without
        // separately pre-funding the derived contract address.
        if let Some(addr) = predicted_wallet_deploy_address(&signed_tx) {
            #[allow(clippy::cast_possible_wrap)]
            self.env
                .setup_grant_storage_tokens(&addr, self.storage_tokens as i64);
        }

        // 5. Execute
        let mut handler = TransactionHandler::new(self.env.clone(), &signed_tx);
        let invoke_result = handler.root_invoke();
        let exec_result = handler.try_commit_transaction();

        // 6. Build receipt
        let new_height = self.tip_height + 1;

        let (status, return_data) = map_exec_result(exec_result, &invoke_result);
        let receipt_events: Vec<EmittedEvent> = invoke_result
            .events
            .iter()
            .filter_map(parse_event)
            .collect();

        let receipt = TransactionReceipt {
            tx_hash,
            height: BlockHeight::from(new_height),
            status,
            events: receipt_events.clone(),
            return_data,
        };

        let gas_limit = configured_gas_limit(signed_tx.tx.invocation.gas_limit);
        let storage_deltas = invoke_result
            .trace
            .as_ref()
            .map(|trace| trace.storage_writes.iter().cloned().collect())
            .unwrap_or_default();
        let record = TransactionRecord {
            receipt: receipt.clone(),
            gas_consumed: invoke_result.gas_consumed,
            gas_limit,
            storage_deltas,
            syscall_trace: invoke_result.trace.clone(),
        };

        let deployed_contract = predicted_wallet_deploy_address(&signed_tx);
        let event_count = receipt.events.len();
        let summary = format_transaction_summary(&tx_hash, &record, deployed_contract.as_ref());
        tracing::info!(
            tx_hash = %format_tx_hash(&tx_hash),
            height = new_height,
            status = %format_status_summary(&record.receipt.status),
            event_count,
            storage_delta_count = record.storage_deltas.len(),
            syscall_count = record
                .syscall_trace
                .as_ref()
                .map_or(0, |trace| trace.system_calls.len()),
            "\n{summary}"
        );

        // 7. Store
        self.transactions.insert(tx_hash, record);
        self.committed_heights
            .insert(tx_hash, BlockHeight::from(new_height));
        self.transaction_order.push_back(tx_hash);

        for (idx, event) in receipt_events.into_iter().enumerate() {
            self.events.push(IdentifiedEvent {
                tx_hash,
                block_height: BlockHeight::from(new_height),
                #[allow(clippy::cast_possible_truncation)]
                sequence_number: idx as u32,
                event,
            });
        }

        // 8. Advance block
        self.env.advance_block();
        self.tip_height = new_height;
        let height_idx = usize::try_from(new_height).expect("bug: negative block height");
        while self.block_txs.len() <= height_idx {
            self.block_txs.push(vec![]);
        }
        self.block_txs[height_idx].push(tx_hash);
        self.evict_old_transactions();

        Ok(tx_hash)
    }

    /// Look up a receipt by hash.
    pub fn get_transaction_receipt(&self, tx_hash: &TxHash) -> Option<TransactionReceipt> {
        self.transactions
            .get(tx_hash)
            .map(|record| record.receipt.clone())
    }

    /// Full retained execution detail.
    pub fn get_transaction_detail(&self, tx_hash: &TxHash) -> Option<TransactionDetail> {
        transaction_detail_lookup(&self.transactions, tx_hash)
    }

    /// Latest-state point read for a contract storage key.
    pub fn get_storage_at(
        &self,
        address: &ContractAddress,
        key: &[u8; 32],
    ) -> Option<StorageItemInfo> {
        let key = StorageKeyContractData::new(*key);
        self.env
            .get_snapshot_storage_item(address, &key)
            .map(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn seed_bytestring(&self, address: &ContractAddress, key: &[u8; 32], value: &[u8]) {
        let key = StorageKeyContractData::new(*key);

        #[allow(clippy::cast_possible_wrap)]
        self.env
            .setup_grant_storage_tokens(address, self.storage_tokens as i64);

        let result = self.env.transact(|tx| {
            tx.call(address).run(|| {
                groundhog_xdr::syscalls::raw_BytestringSet(&key, value, StorageDeltaFlags::None.0)
                    .expect("bug: bytestring write should succeed");
            })
        });
        result.assert_success();
        self.env.advance_block();
    }

    #[cfg(test)]
    pub(crate) fn seed_deployed_bytestring(
        &self,
        address: &ContractAddress,
        key: &[u8; 32],
        value: &[u8],
    ) {
        self.env.setup_create_deployment(
            address,
            &groundhog_xdr::contract::Contract::kNative {
                native_contract_id:
                    groundhog_xdr::precompiles::HogexecPrecompiledNativeContracts::kNativeContractHogToken,
            },
        );
        self.seed_bytestring(address, key, value);
    }

    /// Lightweight status check.
    pub fn get_transaction_status(&self, tx_hash: &TxHash) -> TransactionStatus {
        status_from_index(&self.committed_heights, tx_hash)
    }

    /// Contract events filtered by address and inclusive block-height range.
    pub fn get_contract_events(
        &self,
        address: &ContractAddress,
        from_height: &BlockHeight,
        to_height: &BlockHeight,
    ) -> Vec<IdentifiedEvent> {
        filter_contract_events(&self.events, address, from_height, to_height)
    }

    /// Current chain tip.
    pub fn chain_tip(&self) -> ChainTip {
        let hash_bytes: [u8; 32] =
            Sha256::digest(format!("dev-block-{}", self.tip_height).as_bytes()).into();

        ChainTip {
            height: BlockHeight::from(self.tip_height),
            block_hash: Hex0x(SHA256Hash::from(hash_bytes)),
        }
    }

    /// Pre-funded dev accounts.
    pub fn accounts(&self) -> &[DevAccount] {
        &self.accounts
    }

    /// Reset all state and re-initialize accounts.
    pub fn reset(&mut self) {
        tracing::info!("resetting dev node backend");
        let count = self.accounts.len();
        *self = Self::new(count, self.storage_tokens, self.transaction_retention_limit);
    }

    pub fn stats(&self) -> BackendStats {
        let block_count = u64::try_from(self.tip_height + 1)
            .expect("bug: dev node block count should not be negative");
        BackendStats {
            block_count,
            tx_count: self.committed_heights.len(),
        }
    }

    fn evict_old_transactions(&mut self) {
        evict_transactions(
            &mut self.transactions,
            &mut self.transaction_order,
            &mut self.events,
            self.transaction_retention_limit,
        );
    }
}

fn format_tx_hash(tx_hash: &TxHash) -> String {
    format!("0x{}", hex::encode(tx_hash.0.0.as_ref()))
}

fn transaction_detail_from_record(record: &TransactionRecord) -> TransactionDetail {
    TransactionDetail {
        receipt: record.receipt.clone(),
        gas_consumed: record.gas_consumed,
        gas_limit: record.gas_limit,
        storage_deltas: record.storage_deltas.iter().map(Into::into).collect(),
        trace: record.syscall_trace.as_ref().map(Into::into),
    }
}

fn transaction_detail_lookup(
    transactions: &HashMap<TxHash, TransactionRecord>,
    tx_hash: &TxHash,
) -> Option<TransactionDetail> {
    transactions
        .get(tx_hash)
        .map(transaction_detail_from_record)
}

fn status_from_index(
    committed_heights: &HashMap<TxHash, BlockHeight>,
    tx_hash: &TxHash,
) -> TransactionStatus {
    match committed_heights.get(tx_hash) {
        Some(height) => TransactionStatus::Committed {
            height: height.clone(),
        },
        None => TransactionStatus::Unknown,
    }
}

fn filter_contract_events(
    events: &[IdentifiedEvent],
    address: &ContractAddress,
    from_height: &BlockHeight,
    to_height: &BlockHeight,
) -> Vec<IdentifiedEvent> {
    events
        .iter()
        .filter(|identified| {
            identified.event.contract_address.0 == *address
                && identified.block_height >= *from_height
                && identified.block_height <= *to_height
        })
        .cloned()
        .collect()
}

fn evict_transactions(
    transactions: &mut HashMap<TxHash, TransactionRecord>,
    transaction_order: &mut VecDeque<TxHash>,
    events: &mut Vec<IdentifiedEvent>,
    transaction_retention_limit: usize,
) {
    while transactions.len() > transaction_retention_limit {
        let Some(evicted_hash) = transaction_order.pop_front() else {
            break;
        };
        transactions.remove(&evicted_hash);
        events.retain(|event| event.tx_hash != evicted_hash);
    }
}

fn status_label(status: &ExecStatus) -> &'static str {
    match status {
        ExecStatus::Ok => "ok",
        ExecStatus::Reverted { .. } => "reverted",
        ExecStatus::DeterministicError { .. } => "deterministic_error",
        ExecStatus::NonDeterministicError => "nondeterministic_error",
        ExecStatus::FailedGasSponsorship => "failed_gas_sponsorship",
        _ => "other",
    }
}

fn format_status_summary(status: &ExecStatus) -> String {
    match status {
        ExecStatus::Reverted {
            message: Some(message),
        } if !message.is_empty() => format!("reverted ({message})"),
        ExecStatus::DeterministicError { kind } => format!("deterministic_error ({kind})"),
        other => status_label(other).to_string(),
    }
}

fn format_transaction_summary(
    tx_hash: &TxHash,
    record: &TransactionRecord,
    deployed_contract: Option<&ContractAddress>,
) -> String {
    let mut lines = vec![
        format!("Transaction: {}", format_tx_hash(tx_hash)),
        format!(
            "  Status: {}",
            format_status_summary(&record.receipt.status)
        ),
        format!("  Block: {}", *record.receipt.height),
    ];

    if let Some(address) = deployed_contract {
        lines.push(format!("  Contract: {}", format_contract_address(address)));
    }

    if !record.storage_deltas.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "  Storage deltas ({}):",
            record.storage_deltas.len()
        ));
        lines.extend(
            record
                .storage_deltas
                .iter()
                .map(|delta| format!("    {}", format_storage_delta(delta))),
        );
    }

    if !record.receipt.events.is_empty() {
        lines.push(String::new());
        lines.push(format!("  Events ({}):", record.receipt.events.len()));
        lines.extend(
            record
                .receipt
                .events
                .iter()
                .enumerate()
                .map(|(index, event)| format!("    [{}] {}", index, format_event(event))),
        );
    }

    if let Some(return_data) = record.receipt.return_data.as_ref()
        && !return_data.0.is_empty()
    {
        lines.push(String::new());
        lines.push(format!("  Return: 0x{}", hex::encode(&return_data.0)));
    }

    lines.push(format!(
        "  Gas: {}",
        format_gas_summary(record.gas_consumed, record.gas_limit)
    ));

    lines.join("\n")
}

fn format_storage_delta(delta: &FlaggedStorageDelta) -> String {
    let address = format_contract_address(&delta.addrkey.address);
    let key = format_storage_key(&delta.addrkey.key);
    let body = match &delta.delta {
        StorageDelta::kDeltaTypeBytestringSet { bytestring } => {
            format!("BytestringSet -> 0x{}", hex::encode(&bytestring.0))
        }
        StorageDelta::kDeltaTypeNonnegativeIntSetAdd { nnint } => format!(
            "NonnegativeIntSetAdd set={} add={}",
            format_i128_xdr(&nnint.set_value),
            format_i128_xdr(&nnint.add_value),
        ),
        StorageDelta::kDeltaTypeNonnegativeIntAdd { nnint_add } => {
            format!("NonnegativeIntAdd {:+}", int128_to_i128(nnint_add))
        }
        StorageDelta::kDeltaTypeDelete => "Delete".to_string(),
        StorageDelta::kDeltaTypeRetrocausalQueue { retrocausal_queue } => {
            format!("RetrocausalQueue -> {}", format_xdr_hex(retrocausal_queue))
        }
        StorageDelta::kDeltaTypeAuthDataChange { auth_data_change } => {
            format!("AuthDataChange -> {}", format_xdr_hex(auth_data_change))
        }
        StorageDelta::kDeltaTypeDeployContract {
            deployed_contract_hash,
        } => format!(
            "DeployContract -> 0x{}",
            hex::encode(deployed_contract_hash.hash.as_ref())
        ),
        StorageDelta::kDeltaTypeConsumeUniquifier {
            consume_uniquifier_args,
        } => format!(
            "ConsumeUniquifier nonce={} expiry={}",
            consume_uniquifier_args.uniquifier.nonce,
            *consume_uniquifier_args.uniquifier.block_expiry_height,
        ),
        other => format!("{} -> {}", other.type_(), format_xdr_hex(other)),
    };

    let flags = format_storage_delta_flags(delta.flags);
    format!("{address}:{key}  {body}{flags}")
}

fn format_storage_delta_flags(flags: i32) -> String {
    if flags == StorageDeltaFlags::None.0 {
        String::new()
    } else {
        format!(" [{}]", StorageDeltaFlags(flags))
    }
}

fn format_storage_key(key: &groundhog_xdr::types::StorageKey) -> String {
    format_xdr_hex(key)
}

fn format_event(event: &EmittedEvent) -> String {
    let mut parts = vec![format_contract_address(&event.contract_address.0)];
    parts.extend(
        event
            .topics
            .iter()
            .enumerate()
            .map(|(index, topic)| format!("topic{index}=0x{}", hex::encode(topic.0))),
    );
    parts.push(format!("data=0x{}", hex::encode(&event.data.0)));
    parts.join(" ")
}

fn format_contract_address(address: &ContractAddress) -> String {
    format!("0x{}", hex::encode(address.as_ref()))
}

fn format_xdr_hex<T>(value: &T) -> String
where
    T: xdrs::WriteXDR,
{
    match xdrs::txs::encode(value) {
        Ok(bytes) => format!("0x{}", hex::encode(bytes)),
        Err(err) => format!("<xdr encode error: {err}>"),
    }
}

fn predicted_wallet_deploy_address(signed_tx: &SignedTransaction) -> Option<ContractAddress> {
    if signed_tx.tx.invocation.entrypoint.into_inner() != WALLET_DEPLOY_ENTRYPOINT {
        return None;
    }

    let (deploy_nonce, contract_hash): (SHA256Hash, SHA256Hash) =
        xdrs::cxs::decode(&signed_tx.tx.invocation.calldata.0).ok()?;

    Some(compute_deploy_address(
        &signed_tx.tx.invocation.invoked_address,
        &deploy_nonce,
        &contract_hash,
    ))
}

fn compute_deploy_address(
    deployer_address: &ContractAddress,
    deploy_nonce: &SHA256Hash,
    contract_hash: &SHA256Hash,
) -> ContractAddress {
    derive_deploy_address(deployer_address, deploy_nonce, contract_hash)
        .expect("bug: deploy preimage should encode")
}

/// Parse a `TxEmittedEvent` into a gateway `EmittedEvent`.
///
/// The `serialized_event` field packs topics and data:
/// first `num_topics * 32` bytes are topics, remaining bytes are data.
fn parse_event(e: &TxEmittedEvent) -> Option<EmittedEvent> {
    let num_topics = e.num_topics as usize;
    let bytes = &e.serialized_event.0;
    let Some(topic_bytes) = num_topics.checked_mul(32) else {
        tracing::warn!(
            num_topics,
            "event topic size overflow, skipping malformed event"
        );
        return None;
    };
    if bytes.len() < topic_bytes {
        tracing::warn!(
            num_topics,
            actual_bytes = bytes.len(),
            "event serialized_event too short for claimed num_topics, skipping malformed event"
        );
        return None;
    }
    let topics: Vec<Topic> = (0..num_topics)
        .map(|i| {
            let start = i * 32;
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes[start..start + 32]);
            Topic(arr)
        })
        .collect();
    let data = HexBytes(bytes[topic_bytes..].to_vec());
    Some(EmittedEvent {
        contract_address: Hex0x(e.contract_address),
        topics,
        data,
    })
}

/// Map `ExecResult` and `RootInvokeResult` to gateway status and return data.
fn map_exec_result(
    exec_result: ExecResult,
    invoke_result: &RootInvokeResult,
) -> (ExecStatus, Option<HexBytes>) {
    let return_data = match &invoke_result.output {
        InvocationFrameOutput::Return { return_buffer } => Some(HexBytes(return_buffer.0.clone())),
        _ => None,
    };

    let status = if exec_result.0 == ExecResult::ExecResultOK.0 {
        ExecStatus::Ok
    } else if exec_result.0 == ExecResult::ExecResultTxRootFrameReverted.0 {
        let message = match &invoke_result.output {
            InvocationFrameOutput::Abort { abort_buffer } => {
                String::from_utf8(abort_buffer.0.clone()).ok()
            }
            _ => None,
        };
        ExecStatus::Reverted { message }
    } else if exec_result.0 == ExecResult::ExecResultTxDeterministicError.0 {
        ExecStatus::DeterministicError {
            kind: "deterministic_error".to_string(),
        }
    } else if exec_result.0 == ExecResult::ExecResultTxNonDeterministicError.0 {
        ExecStatus::NonDeterministicError
    } else if exec_result.0 == ExecResult::ExecResultFailedGasSponsorship.0 {
        ExecStatus::FailedGasSponsorship
    } else {
        ExecStatus::Other
    };

    (status, return_data)
}

#[cfg(test)]
mod tests {
    use super::{DevBackend, TransactionRecord, format_transaction_summary};
    use crate::DEFAULT_TRANSACTION_RETENTION_LIMIT;
    use crate::accounts::DEFAULT_STORAGE_TOKENS;
    use crate::wallet::DEV_WALLET_WASM;
    use groundhog_rpc_api_types::BlockHeight;
    use groundhog_rpc_api_types::events::{EmittedEvent, IdentifiedEvent};
    use groundhog_rpc_api_types::hex::{Hex0x, HexBytes, Topic};
    use groundhog_rpc_api_types::receipt::ExecStatus;
    use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
    use groundhog_signing::{
        BuildCallTransaction, BuildDeployTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE,
        DEFAULT_TX_GAS_LIMIT, PubkeyAccount, SignTransactionParams, Signer, build_call_transaction,
        build_deploy_transaction, encode_signed_transaction_xdr, sign_transaction,
    };
    use groundhog_xdr::auth_claim::{AuthClaim, AuthClaimData};
    use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
    use groundhog_xdr::event::TxEmittedEvent;
    use groundhog_xdr::storage_item::{
        FlaggedStorageDelta, StorageDelta, StorageDeltaFlags, WireStorageItem,
    };
    use groundhog_xdr::types::{
        AddressAndKey, ContractAddress, SHA256Hash, StorageKey, StorageKeyContractData, int128_xdr,
    };
    use sha2::{Digest, Sha256};
    use std::collections::{HashMap, VecDeque};
    use xdr_common::{BytesM, VecM};

    #[test]
    fn initializes_wallet_auth_data_for_dev_accounts() {
        let backend = DevBackend::new(
            1,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        );
        assert_eq!(backend.accounts().len(), 1);
    }

    #[test]
    fn parse_event_skips_malformed_topic_payload() {
        let raw = TxEmittedEvent {
            contract_address: groundhog_xdr::types::ContractAddress::from([0xAA; 32]),
            num_topics: 2,
            serialized_event: BytesM(vec![0x11; 32]),
        };

        assert!(super::parse_event(&raw).is_none());
    }

    #[test]
    fn deploy_transaction_prefunds_predicted_address() {
        let mut backend = DevBackend::new(
            1,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        );
        let signer = crate::accounts::derive_dev_signer(0);
        let wallet_address = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE)
            .address()
            .expect("bug: dev wallet address derivation should succeed");

        let contract = Contract::kWasmi {
            wasmi_contract: ContractBlob {
                wasm: BytesM(DEV_WALLET_WASM.to_vec()),
            },
        };
        let contract_xdr = xdrs::txs::encode(&contract).expect("bug: contract should encode");
        let contract_hash = SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&contract_xdr)));

        let deploy_nonce =
            SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(1234_u64.to_le_bytes())));
        let expected_address =
            super::compute_deploy_address(&wallet_address, &deploy_nonce, &contract_hash);
        let calldata = xdrs::cxs::encode(&(&deploy_nonce, &contract_hash))
            .expect("bug: deploy calldata should encode");
        let auth_claims: AuthClaimData = vec![AuthClaim::kInvokedAddressSignature {
            invoked_address_signature_id: 0,
        }]
        .try_into()
        .expect("bug: auth claim should fit");

        let tx = build_deploy_transaction(BuildDeployTransaction {
            invoked_address: wallet_address,
            entrypoint: ContractEntrypointID::from(super::WALLET_DEPLOY_ENTRYPOINT),
            calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: i64::MAX,
            nonce: 1234,
            contracts_to_deploy: VecM(vec![contract]),
            auth_claims,
        });
        let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
            .expect("bug: deploy tx should sign");
        let tx_bytes =
            encode_signed_transaction_xdr(&signed_tx).expect("bug: deploy tx should encode");

        let tx_hash = backend
            .send_raw_transaction(&tx_bytes)
            .expect("bug: deploy should commit in dev backend");
        let receipt = backend
            .get_transaction_receipt(&tx_hash)
            .expect("bug: committed tx should have receipt");

        assert!(matches!(receipt.status, ExecStatus::Ok));
        assert_eq!(
            receipt
                .return_data
                .expect("bug: deploy should return address")
                .0,
            expected_address.as_ref()
        );
    }

    #[test]
    fn get_storage_at_after_real_wasm_deploy_and_call() {
        let wasm_path = std::env::var("GROUNDHOG_SMOKE_WASM").unwrap_or_else(|_| {
            "/tmp/gh-storage-smoke-target/groundhog/gh_storage_smoke.wasm".into()
        });
        if !std::path::Path::new(&wasm_path).exists() {
            eprintln!("skipping real wasm reproduction; missing {wasm_path}");
            return;
        }

        let mut backend = DevBackend::new(
            1,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        );
        let signer = crate::accounts::derive_dev_signer(0);
        let wallet_address = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE)
            .address()
            .expect("bug: dev wallet address derivation should succeed");
        let wasm_bytes = std::fs::read(&wasm_path).expect("bug: smoke wasm should be readable");

        let contract = Contract::kWasmi {
            wasmi_contract: ContractBlob {
                wasm: BytesM(wasm_bytes),
            },
        };
        let contract_xdr = xdrs::txs::encode(&contract).expect("bug: contract should encode");
        let contract_hash = SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&contract_xdr)));
        let deploy_nonce =
            SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(1234_u64.to_le_bytes())));
        let deployed_address =
            super::compute_deploy_address(&wallet_address, &deploy_nonce, &contract_hash);
        let deploy_calldata = xdrs::cxs::encode(&(&deploy_nonce, &contract_hash))
            .expect("bug: deploy calldata should encode");
        let auth_claims: AuthClaimData = vec![AuthClaim::kInvokedAddressSignature {
            invoked_address_signature_id: 0,
        }]
        .try_into()
        .expect("bug: auth claim should fit");

        let deploy_tx = build_deploy_transaction(BuildDeployTransaction {
            invoked_address: wallet_address,
            entrypoint: ContractEntrypointID::from(super::WALLET_DEPLOY_ENTRYPOINT),
            calldata: deploy_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: i64::MAX,
            nonce: 1234,
            contracts_to_deploy: VecM(vec![contract]),
            auth_claims: auth_claims.clone(),
        });
        let signed_deploy = sign_transaction(deploy_tx, &signer, SignTransactionParams::default())
            .expect("bug: deploy tx should sign");
        let deploy_tx_bytes =
            encode_signed_transaction_xdr(&signed_deploy).expect("bug: deploy tx should encode");

        backend
            .send_raw_transaction(&deploy_tx_bytes)
            .expect("bug: deploy should commit");

        let set_entrypoint = contract_entrypoint_id("SmokeStorage", "0.1.0", "set");
        let set_calldata =
            xdrs::cxs::encode(&"cafebabe".to_string()).expect("bug: set calldata should encode");
        let inner_invocation = xdrs::txs::encode(&(
            deployed_address,
            set_entrypoint,
            DEFAULT_TX_GAS_LIMIT,
            set_calldata.as_slice(),
        ))
        .expect("bug: inner invocation should encode");
        let do_action_calldata = xdrs::cxs::encode(&inner_invocation.as_slice())
            .expect("bug: wallet calldata should encode");

        let set_tx = build_call_transaction(BuildCallTransaction {
            invoked_address: wallet_address,
            entrypoint: ContractEntrypointID::from(0x1984_6D75_0FAD_81D6_u64.to_be_bytes()),
            calldata: do_action_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: i64::MAX,
            nonce: 1235,
            auth_claims,
        });
        let signed_set = sign_transaction(set_tx, &signer, SignTransactionParams::default())
            .expect("bug: set tx should sign");
        let set_tx_bytes =
            encode_signed_transaction_xdr(&signed_set).expect("bug: set tx should encode");

        backend
            .send_raw_transaction(&set_tx_bytes)
            .expect("bug: set should commit");

        let item = backend
            .get_storage_at(&deployed_address, &[0x42; 32])
            .expect("bug: committed bytestring should be readable after real wasm call");

        assert_eq!(item.kind, "bytestring");
        assert_eq!(item.summary, "Bytestring = 0x6361666562616265");
    }

    #[test]
    fn status_survives_heavy_record_eviction() {
        let first = tx_hash(0x11);
        let second = tx_hash(0x22);
        let mut transactions = HashMap::from([
            (first, sample_record(first, 10)),
            (second, sample_record(second, 11)),
        ]);
        let committed_heights = HashMap::from([
            (first, BlockHeight::from(10)),
            (second, BlockHeight::from(11)),
        ]);
        let mut transaction_order = VecDeque::from([first, second]);
        let mut events = Vec::new();

        super::evict_transactions(&mut transactions, &mut transaction_order, &mut events, 1);

        assert!(!transactions.contains_key(&first));
        assert!(transactions.contains_key(&second));
        assert!(matches!(
            super::status_from_index(&committed_heights, &first),
            TransactionStatus::Committed { height } if *height == 10
        ));
        assert!(matches!(
            super::status_from_index(&committed_heights, &second),
            TransactionStatus::Committed { height } if *height == 11
        ));
    }

    #[test]
    fn get_contract_events_filters_by_address_and_height() {
        let address_a = ContractAddress::from([0xAA; 32]);
        let address_b = ContractAddress::from([0xBB; 32]);
        let expected = sample_identified_event(0x33, 12, address_a, 1);
        let events = vec![
            sample_identified_event(0x11, 10, address_a, 0),
            sample_identified_event(0x22, 11, address_b, 0),
            sample_identified_event(0x33, 12, address_a, 1),
        ];

        let filtered = super::filter_contract_events(
            &events,
            &address_a,
            &BlockHeight::from(11),
            &BlockHeight::from(12),
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tx_hash, expected.tx_hash);
        assert_eq!(*filtered[0].block_height, *expected.block_height);
        assert_eq!(filtered[0].sequence_number, expected.sequence_number);
        assert_eq!(
            filtered[0].event.contract_address,
            expected.event.contract_address
        );
        assert_eq!(filtered[0].event.topics, expected.event.topics);
        assert_eq!(filtered[0].event.data, expected.event.data);
    }

    #[test]
    fn evicting_heavy_records_prunes_events() {
        let address = ContractAddress::from([0xAB; 32]);
        let first = tx_hash(0x11);
        let second = tx_hash(0x22);
        let mut transactions = HashMap::from([
            (first, sample_record(first, 10)),
            (second, sample_record(second, 11)),
        ]);
        let mut transaction_order = VecDeque::from([first, second]);
        let mut events = vec![
            sample_identified_event(0x11, 10, address, 0),
            sample_identified_event(0x22, 11, address, 1),
        ];

        super::evict_transactions(&mut transactions, &mut transaction_order, &mut events, 1);
        let filtered = super::filter_contract_events(
            &events,
            &address,
            &BlockHeight::from(0),
            &BlockHeight::from(20),
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tx_hash, tx_hash(0x22));
    }

    #[test]
    fn get_transaction_detail_maps_retained_record() {
        let tx_hash = tx_hash(0x33);
        let mut record = sample_record(tx_hash, 12);
        record.storage_deltas = vec![FlaggedStorageDelta {
            addrkey: AddressAndKey {
                address: ContractAddress::from([0xAA; 32]),
                key: StorageKey::ContractData {
                    contract_data_key: StorageKeyContractData::from([0xBB; 32]),
                },
            },
            delta: StorageDelta::kDeltaTypeBytestringSet {
                bytestring: BytesM(vec![0xCC]),
            },
            flags: 0,
        }];

        let detail =
            super::transaction_detail_lookup(&HashMap::from([(tx_hash, record)]), &tx_hash)
                .expect("bug: retained record should map to detail");

        assert_eq!(detail.gas_consumed, 7);
        assert_eq!(detail.gas_limit, Some(100));
        assert_eq!(detail.storage_deltas.len(), 1);
        assert_eq!(detail.storage_deltas[0].kind, "bytestringSet");
        assert!(detail.trace.is_none());
    }

    #[test]
    fn get_transaction_detail_returns_none_after_eviction() {
        let first = tx_hash(0x11);
        let second = tx_hash(0x22);
        let mut transactions = HashMap::from([
            (first, sample_record(first, 10)),
            (second, sample_record(second, 11)),
        ]);
        let mut transaction_order = VecDeque::from([first, second]);
        let mut events = Vec::new();

        super::evict_transactions(&mut transactions, &mut transaction_order, &mut events, 1);

        assert!(super::transaction_detail_lookup(&transactions, &first).is_none());
        assert!(super::transaction_detail_lookup(&transactions, &second).is_some());
    }

    #[test]
    fn get_storage_at_reads_latest_committed_bytestring() {
        let backend = DevBackend::new(
            0,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        );
        let address = ContractAddress::from([0x11; 32]);
        let key_bytes = [0x22; 32];
        let key = StorageKeyContractData::new(key_bytes);

        #[allow(clippy::cast_possible_wrap)]
        backend
            .env
            .setup_grant_storage_tokens(&address, DEFAULT_STORAGE_TOKENS as i64);

        let result = backend.env.transact(|tx| {
            tx.call(&address).run(|| {
                groundhog_xdr::syscalls::raw_BytestringSet(
                    &key,
                    b"hello",
                    StorageDeltaFlags::None.0,
                )
                .expect("bug: bytestring write should succeed");
            })
        });
        result.assert_success();
        backend.env.advance_block();

        let item = backend
            .get_storage_at(&address, &key_bytes)
            .expect("bug: committed bytestring should be readable");
        let expected = WireStorageItem::kItemTypeBytestring {
            bytestring: BytesM(b"hello".to_vec()),
        };

        assert_eq!(item.kind, "bytestring");
        assert_eq!(item.summary, "Bytestring = 0x68656c6c6f");
        assert_eq!(
            item.item_xdr,
            HexBytes(xdrs::txs::encode(&expected).unwrap())
        );
    }

    #[test]
    fn get_storage_at_reads_latest_committed_bytestring_on_wasmi_wallet_address() {
        let backend = DevBackend::new(
            1,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        );
        let address = backend.accounts()[0].address.0;
        let key_bytes = [0x24; 32];
        let key = StorageKeyContractData::new(key_bytes);

        let result = backend.env.transact(|tx| {
            tx.call(&address).run(|| {
                groundhog_xdr::syscalls::raw_BytestringSet(
                    &key,
                    b"hello",
                    StorageDeltaFlags::None.0,
                )
                .expect("bug: bytestring write should succeed");
            })
        });
        result.assert_success();
        backend.env.advance_block();

        let item = backend
            .get_storage_at(&address, &key_bytes)
            .expect("bug: committed bytestring on wasmi wallet should be readable");
        let expected = WireStorageItem::kItemTypeBytestring {
            bytestring: BytesM(b"hello".to_vec()),
        };

        assert_eq!(item.kind, "bytestring");
        assert_eq!(item.summary, "Bytestring = 0x68656c6c6f");
        assert_eq!(
            item.item_xdr,
            HexBytes(xdrs::txs::encode(&expected).unwrap())
        );
    }

    #[test]
    fn get_storage_at_returns_none_for_missing_key() {
        let backend = DevBackend::new(
            0,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        );
        let address = ContractAddress::from([0x33; 32]);
        let key = [0x44; 32];

        assert!(backend.get_storage_at(&address, &key).is_none());
    }

    #[test]
    fn transaction_summary_includes_key_sections() {
        let tx_hash = tx_hash(0x44);
        let storage_address = ContractAddress::from([0x99; 32]);
        let receipt = receipt(tx_hash, 7);
        let record = TransactionRecord {
            receipt: groundhog_rpc_api_types::receipt::TransactionReceipt {
                events: vec![EmittedEvent {
                    contract_address: Hex0x(ContractAddress::from([0x55; 32])),
                    topics: vec![Topic([0x66; 32])],
                    data: HexBytes(vec![0x77, 0x88]),
                }],
                return_data: Some(HexBytes(vec![0x01, 0x02])),
                ..receipt
            },
            gas_consumed: 12_450,
            gas_limit: Some(1_000_000_000),
            storage_deltas: vec![FlaggedStorageDelta {
                addrkey: AddressAndKey {
                    address: storage_address,
                    key: StorageKey::ContractData {
                        contract_data_key: StorageKeyContractData::from([0xAB; 32]),
                    },
                },
                delta: StorageDelta::kDeltaTypeNonnegativeIntAdd {
                    nnint_add: int128_xdr {
                        highbits: 0,
                        lowbits: 42,
                    },
                },
                flags: 0,
            }],
            syscall_trace: None,
        };

        let summary =
            format_transaction_summary(&tx_hash, &record, Some(&ContractAddress::from([0x88; 32])));

        assert!(summary.contains("Transaction: 0x"));
        assert!(summary.contains("Contract: 0x"));
        assert!(summary.contains("Storage deltas (1):"));
        assert!(summary.contains(&format!(
            "{}:",
            super::format_contract_address(&storage_address)
        )));
        assert!(summary.contains("NonnegativeIntAdd +42"));
        assert!(summary.contains("Events (1):"));
        assert!(summary.contains("Return: 0x0102"));
        assert!(summary.contains("Gas: 12,450 / 1,000,000,000"));
    }

    fn sample_record(tx_hash: TxHash, height: i64) -> TransactionRecord {
        TransactionRecord {
            receipt: receipt(tx_hash, height),
            gas_consumed: 7,
            gas_limit: Some(100),
            storage_deltas: Vec::new(),
            syscall_trace: None,
        }
    }

    fn receipt(
        tx_hash: TxHash,
        height: i64,
    ) -> groundhog_rpc_api_types::receipt::TransactionReceipt {
        groundhog_rpc_api_types::receipt::TransactionReceipt {
            tx_hash,
            height: BlockHeight::from(height),
            status: ExecStatus::Ok,
            events: Vec::new(),
            return_data: None,
        }
    }

    fn tx_hash(discriminator: u8) -> TxHash {
        TxHash(Hex0x(SHA256Hash::from([discriminator; 32])))
    }

    fn contract_entrypoint_id(
        interface_name: &str,
        version_str: &str,
        method: &str,
    ) -> ContractEntrypointID {
        let mut hasher = Sha256::new();
        hasher.update(interface_name.as_bytes());
        hasher.update(major_version(version_str).as_bytes());
        hasher.update(method.as_bytes());
        let hash = hasher.finalize();
        let mut id = [0u8; 8];
        id.copy_from_slice(&hash[..8]);
        ContractEntrypointID::from(id)
    }

    fn major_version(version: &str) -> &str {
        let normalized = version.split_once('+').map_or(version, |(base, _)| base);
        let normalized = normalized
            .split_once('-')
            .map_or(normalized, |(base, _)| base);

        let mut parts = normalized.split('.');
        let major = parts.next().unwrap_or(normalized);
        let minor = parts.next().unwrap_or("0");
        let patch = parts.next().unwrap_or("0");

        if major != "0" {
            major
        } else if minor != "0" {
            &normalized[..major.len() + 1 + minor.len()]
        } else if patch != "0" {
            &normalized[..major.len() + 1 + minor.len() + 1 + patch.len()]
        } else {
            normalized
        }
    }

    fn sample_identified_event(
        discriminator: u8,
        height: i64,
        address: ContractAddress,
        sequence_number: u32,
    ) -> IdentifiedEvent {
        IdentifiedEvent {
            tx_hash: tx_hash(discriminator),
            block_height: BlockHeight::from(height),
            sequence_number,
            event: EmittedEvent {
                contract_address: Hex0x(address),
                topics: vec![Topic([discriminator; 32])],
                data: HexBytes(vec![discriminator]),
            },
        }
    }
}
