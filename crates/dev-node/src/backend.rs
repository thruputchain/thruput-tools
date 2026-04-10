//! Dev node backend — owns `TestEnvironment` and in-memory state.
//!
//! Lives on a dedicated `std::thread`. Never crosses thread boundaries.

use std::collections::HashMap;

use groundhog_native_exec::test_environment::{TestEnvironment, TestEnvironmentHandle};
use groundhog_native_exec::transaction_handler::{RootInvokeResult, TransactionHandler};
use groundhog_rpc_api_types::BlockHeight;
use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_api_types::dev::DevAccount;
use groundhog_rpc_api_types::events::{EmittedEvent, IdentifiedEvent};
use groundhog_rpc_api_types::hex::{Hex0x, HexBytes, Topic};
use groundhog_rpc_api_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
use groundhog_signing::{DEFAULT_TX_GAS_LIMIT, WALLET_DEPLOY_ENTRYPOINT, derive_deploy_address};
use groundhog_xdr::event::TxEmittedEvent;
use groundhog_xdr::exec_result::ExecResult;
use groundhog_xdr::transaction::{InvocationFrameOutput, SignedTransaction};
use groundhog_xdr::types::{ContractAddress, SHA256Hash};
use sha2::{Digest, Sha256};

use crate::accounts;
use crate::error::DevNodeError;

/// In-memory dev node state.
pub struct DevBackend {
    env: TestEnvironmentHandle,
    receipts: HashMap<TxHash, TransactionReceipt>,
    events: Vec<IdentifiedEvent>,
    tip_height: i64,
    block_txs: Vec<Vec<TxHash>>,
    accounts: Vec<DevAccount>,
    storage_tokens: u64,
}

impl DevBackend {
    /// Initialize with pre-funded accounts.
    ///
    /// Replicates Groundhog's genesis setup:
    /// 1. Register the dev-wallet WASM contract
    /// 2. Deploy the wallet at each dev account address and grant storage tokens
    /// 3. Initialize each wallet with its owner's ed25519 public key
    pub fn new(account_count: usize, storage_tokens: u64) -> Self {
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
            receipts: HashMap::new(),
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

        // 3. Verify auth — 0 means success, any other value is an error.
        let auth_result = self.env.verify_signed_transaction_auth(&signed_tx);
        if auth_result.0 != 0 {
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

        let receipt_events: Vec<EmittedEvent> =
            invoke_result.events.iter().map(parse_event).collect();

        let receipt = TransactionReceipt {
            tx_hash,
            height: BlockHeight::from(new_height),
            status,
            events: receipt_events.clone(),
            return_data,
        };

        // 7. Store
        self.receipts.insert(tx_hash, receipt);

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

        Ok(tx_hash)
    }

    /// Look up a receipt by hash.
    pub fn get_transaction_receipt(&self, tx_hash: &TxHash) -> Option<TransactionReceipt> {
        self.receipts.get(tx_hash).cloned()
    }

    /// Lightweight status check.
    pub fn get_transaction_status(&self, tx_hash: &TxHash) -> TransactionStatus {
        match self.receipts.get(tx_hash) {
            Some(receipt) => {
                let block_hash_bytes: [u8; 32] =
                    Sha256::digest(format!("dev-block-{}", *receipt.height).as_bytes()).into();
                TransactionStatus::Committed {
                    height: receipt.height.clone(),
                    block_hash: Hex0x(SHA256Hash::from(block_hash_bytes)),
                }
            }
            None => TransactionStatus::Unknown,
        }
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
        let count = self.accounts.len();
        *self = Self::new(count, self.storage_tokens);
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
fn parse_event(e: &TxEmittedEvent) -> EmittedEvent {
    let num_topics = e.num_topics as usize;
    let bytes = &e.serialized_event.0;
    let topic_bytes = num_topics * 32;
    let topics: Vec<Topic> = (0..num_topics)
        .map(|i| {
            let start = i * 32;
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes[start..start + 32]);
            Topic(arr)
        })
        .collect();
    let data = HexBytes(bytes[topic_bytes..].to_vec());
    EmittedEvent {
        contract_address: Hex0x(e.contract_address),
        topics,
        data,
    }
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
    use super::DevBackend;
    use crate::accounts::DEFAULT_STORAGE_TOKENS;
    use crate::wallet::DEV_WALLET_WASM;
    use groundhog_rpc_api_types::receipt::ExecStatus;
    use groundhog_signing::{
        BuildDeployTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE, DEFAULT_TX_GAS_LIMIT,
        PubkeyAccount, SignTransactionParams, Signer, build_deploy_transaction,
        encode_signed_transaction_xdr, sign_transaction,
    };
    use groundhog_xdr::auth_claim::{AuthClaim, AuthClaimData};
    use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
    use groundhog_xdr::types::SHA256Hash;
    use sha2::{Digest, Sha256};
    use xdr_common::{BytesM, VecM};

    #[test]
    fn initializes_wallet_auth_data_for_dev_accounts() {
        let backend = DevBackend::new(1, DEFAULT_STORAGE_TOKENS);
        assert_eq!(backend.accounts().len(), 1);
    }

    #[test]
    fn deploy_transaction_prefunds_predicted_address() {
        let mut backend = DevBackend::new(1, DEFAULT_STORAGE_TOKENS);
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
}
