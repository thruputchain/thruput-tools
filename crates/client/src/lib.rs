//! Contract invocation without CLI configuration, terminal output, or runtime ownership.
//!
//! Callers resolve addresses, wallet entrypoints, and calldata, verify network identity,
//! then prepare, inspect, sign, submit, and wait as separate operations.

pub mod codec;
pub use jsonrpsee;
pub use thruput_contract_spec as spec;
pub use thruput_json_rpc_client as rpc;
pub use thruput_json_rpc_types as types;
pub use thruput_signing as signing;
/// Shared C/G/S text encodings; independent of either CLI.
pub use thruput_signing::strkey;
pub use thruput_xdr as xdr;

use std::time::Duration;

use jsonrpsee::core::client::{ClientT, Error as RpcError};
use thruput_json_rpc_client::{GroundhogApiClient, PollTransactionReceiptError};
use thruput_json_rpc_types::{
    network_info::NetworkInfo, receipt::TransactionReceipt, transaction::TxHash,
};
use thruput_signing::{SignTransactionParams, Signer, SigningError};
use thruput_xdr::{transaction::Transaction, types::NetworkHash};

/// An inspectable unsigned wallet invocation, bound to the intended network.
#[derive(Debug, Clone)]
pub struct UnsignedCall {
    transaction: Transaction,
    network_hash: NetworkHash,
    native_return: bool,
}

/// Explicit wallet identity and invocation format. Legacy addresses are never
/// reinterpreted as native accounts based on their owner key.
#[derive(Debug, Clone, Copy)]
pub enum Wallet<'a> {
    /// Root pubkey account using the native `DefaultWallet` ABI.
    Native(&'a signing::PubkeyAccount),
    /// A deployed legacy wallet with its configured call entrypoint.
    Legacy {
        /// Deployed wallet address.
        address: xdr::types::ContractAddress,
        /// Legacy `do_action` entrypoint from the network's wallet interface.
        entrypoint: xdr::contract::ContractEntrypointID,
    },
}

/// A single child call through an explicitly selected wallet adapter.
#[derive(Debug, Clone, Copy)]
pub struct CallRequest<'a> {
    /// Native or legacy caller identity.
    pub wallet: Wallet<'a>,
    /// Child contract address.
    pub target_address: xdr::types::ContractAddress,
    /// Child contract method selector.
    pub target_entrypoint: xdr::contract::ContractEntrypointID,
    /// Encoded child arguments.
    pub target_calldata: &'a [u8],
    /// Wallet root gas budget.
    pub gas_limit: u64,
    /// Child gas budget.
    pub target_gas_limit: u64,
    /// Transaction expiration height.
    pub block_expiry_height: i64,
    /// Transaction uniquifier, independent of native account derivation.
    pub nonce: u64,
}

/// Prepare a single native or legacy child call without submitting it.
pub fn prepare_call(
    request: CallRequest<'_>,
    network_hash: NetworkHash,
) -> Result<UnsignedCall, SigningError> {
    let transaction = match request.wallet {
        Wallet::Native(account) => signing::default_wallet::build_default_wallet_call_transaction(
            signing::default_wallet::BuildDefaultWalletCallTransaction {
                account,
                target_address: request.target_address,
                target_entrypoint: request.target_entrypoint,
                target_calldata: request.target_calldata,
                target_gas_limit: request.target_gas_limit,
                gas_limit: request.gas_limit,
                block_expiry_height: request.block_expiry_height,
                nonce: request.nonce,
            },
        )?,
        Wallet::Legacy {
            address,
            entrypoint,
        } => signing::build_dev_wallet_do_action_transaction(
            signing::BuildDevWalletDoActionTransaction {
                wallet_address: address,
                wallet_entrypoint: entrypoint,
                target_address: request.target_address,
                target_entrypoint: request.target_entrypoint,
                target_calldata: request.target_calldata,
                target_gas_limit: request.target_gas_limit,
                gas_limit: request.gas_limit,
                block_expiry_height: request.block_expiry_height,
                nonce: request.nonce,
            },
        )?,
    };
    Ok(UnsignedCall {
        transaction,
        network_hash,
        native_return: matches!(request.wallet, Wallet::Native(_)),
    })
}

impl UnsignedCall {
    /// The exact transaction body that will be signed.
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    /// Sign this prepared invocation without submitting it or consulting configuration.
    /// Reject a signer whose public key differs from the native account's auth claim.
    pub fn sign(self, signer: &impl Signer) -> Result<SignedCall, SigningError> {
        if let xdr::auth_claim::AuthClaim::kPubkeySignature { pubkey_preimage } =
            &self.transaction.auth_claim
            && pubkey_preimage.pubkey != signer.public_key()
        {
            return Err(SigningError::SignerMismatch);
        }
        let tx_hash = TxHash(thruput_signing::transaction_hash(&self.transaction)?.into());
        let tx_hex = thruput_signing::sign_and_encode_transaction_hex(
            self.transaction,
            signer,
            SignTransactionParams::default(),
            &self.network_hash,
        )?;
        Ok(SignedCall {
            tx_hash,
            tx_hex,
            native_return: self.native_return,
        })
    }
}

/// A signed invocation with its canonical hash available before any network request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCall {
    tx_hash: TxHash,
    tx_hex: String,
    native_return: bool,
}

impl SignedCall {
    /// Whether the root invocation uses the native wallet ABI.
    pub fn is_native(&self) -> bool {
        self.native_return
    }

    /// Extract the one child's return bytes after checking the receipt's success
    /// status. The original wallet receipt remains available to the caller.
    pub fn decode_return(&self, data: &[u8]) -> Result<Vec<u8>, SigningError> {
        if self.native_return {
            signing::default_wallet::decode_default_wallet_call_return(data)
        } else {
            Ok(data.to_vec())
        }
    }
    /// Canonical signed transaction encoded for `sendRawTransaction`.
    pub fn tx_hex(&self) -> &str {
        &self.tx_hex
    }
    /// Canonical transaction hash, also available if submission has an ambiguous outcome.
    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    /// Submit exactly these signed bytes once. Errors retain the original hash for reconciliation.
    pub async fn submit<C: ClientT + Sync>(
        &self,
        client: &C,
    ) -> Result<SubmittedCall, SubmitCallError> {
        let returned =
            thruput_json_rpc_client::submit_transaction_with_client(client, &self.tx_hex)
                .await
                .map_err(|source| SubmitCallError::Rpc {
                    tx_hash: self.tx_hash,
                    source,
                })?;
        if returned != self.tx_hash {
            return Err(SubmitCallError::HashMismatch {
                tx_hash: self.tx_hash,
                returned,
            });
        }
        Ok(SubmittedCall {
            tx_hash: self.tx_hash,
        })
    }
}

/// Submission failure preserving the signed transaction's canonical identity.
#[derive(Debug, thiserror::Error)]
pub enum SubmitCallError {
    /// A transport failure may occur after admission; reconcile this hash before retrying.
    #[error("groundhog_sendRawTransaction failed for {tx_hash:?}: {source}")]
    Rpc {
        tx_hash: TxHash,
        #[source]
        source: RpcError,
    },
    /// The endpoint returned a different hash than the canonical submitted transaction.
    #[error("submitted transaction {tx_hash:?}, but endpoint returned {returned:?}")]
    HashMismatch { tx_hash: TxHash, returned: TxHash },
}

/// An admitted invocation, whose hash can be saved and polled again without resubmission.
#[derive(Debug, Clone, Copy)]
pub struct SubmittedCall {
    pub tx_hash: TxHash,
}

/// Receipt polling bounds chosen by the embedding application.
#[derive(Debug, Clone, Copy)]
pub struct PollOptions {
    pub max_attempts: usize,
    pub interval: Duration,
}

impl Default for PollOptions {
    fn default() -> Self {
        Self {
            max_attempts: thruput_json_rpc_client::DEFAULT_RECEIPT_POLL_ATTEMPTS,
            interval: thruput_json_rpc_client::DEFAULT_RECEIPT_POLL_INTERVAL,
        }
    }
}

impl SubmittedCall {
    /// Wait for the authoritative receipt, including unsuccessful execution receipts.
    /// Every polling error retains the transaction hash. This method never resubmits.
    pub async fn wait<C: ClientT + Sync>(
        &self,
        client: &C,
        options: PollOptions,
    ) -> Result<TransactionReceipt, PollTransactionReceiptError> {
        thruput_json_rpc_client::poll_transaction_receipt_with_client_config(
            client,
            self.tx_hash,
            options.max_attempts,
            options.interval,
        )
        .await
    }
}

/// Failure to verify the endpoint against an independently configured network identity.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("groundhog_networkInfo failed: {0}")]
    Rpc(#[from] RpcError),
    #[error("network identity mismatch: expected {expected:?}, received {actual:?}")]
    Mismatch {
        expected: NetworkHash,
        actual: NetworkHash,
    },
}

/// Require the endpoint's network hash to match the caller's trusted configuration.
pub async fn validate_network<C: ClientT + Sync>(
    client: &C,
    expected: &NetworkHash,
) -> Result<NetworkInfo, NetworkError> {
    let info = client.network_info().await?;
    let actual = NetworkHash::from(info.network_hash.0);
    if &actual != expected {
        return Err(NetworkError::Mismatch {
            expected: expected.clone(),
            actual,
        });
    }
    Ok(info)
}

/// Resolve transaction expiry relative to the current chain tip using a caller-selected offset.
pub async fn resolve_block_expiry_height<C: ClientT + Sync>(
    client: &C,
    offset: i64,
) -> Result<i64, RpcError> {
    Ok((*client.chain_tip().await?.height).saturating_add(offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{RpcModule, server::ServerBuilder};
    use thruput_signing::LocalSigner;
    use thruput_xdr::{
        contract::ContractEntrypointID,
        types::{ContractAddress, SHA256Hash},
    };

    fn prepared() -> UnsignedCall {
        prepared_with_wallet(Wallet::Legacy {
            address: ContractAddress::from([1; 32]),
            entrypoint: ContractEntrypointID::from([2; 8]),
        })
    }

    fn prepared_with_wallet(wallet: Wallet<'_>) -> UnsignedCall {
        prepare_call(
            CallRequest {
                wallet,
                target_address: ContractAddress::from([3; 32]),
                target_entrypoint: ContractEntrypointID::from([4; 8]),
                target_calldata: &[5, 6],
                gas_limit: 100,
                target_gas_limit: 90,
                block_expiry_height: 1000,
                nonce: 7,
            },
            NetworkHash::from(SHA256Hash::from([8; 32])),
        )
        .unwrap()
    }

    #[test]
    fn native_signer_mismatch_is_rejected_before_signing() {
        struct WrongSigner;
        impl Signer for WrongSigner {
            fn public_key(&self) -> xdr::types::PublicKey {
                LocalSigner::from_secret_key([10; 32]).public_key()
            }

            fn sign(&self, _: &[u8]) -> Result<xdr::types::Signature, SigningError> {
                panic!("mismatching signer must not be invoked")
            }
        }

        let owner = LocalSigner::from_secret_key([9; 32]);
        for nonce in [0, 17] {
            let account = signing::PubkeyAccount::new(owner.public_key(), nonce);
            let plan = prepared_with_wallet(Wallet::Native(&account));
            assert!(matches!(
                plan.sign(&WrongSigner),
                Err(SigningError::SignerMismatch)
            ));
        }
    }

    #[test]
    fn matching_native_signer_preserves_canonical_bytes() {
        let owner = LocalSigner::from_secret_key([9; 32]);
        let account = signing::PubkeyAccount::new(owner.public_key(), 17);
        let plan = prepared_with_wallet(Wallet::Native(&account));
        let expected = signing::sign_and_encode_transaction_hex(
            plan.transaction().clone(),
            &owner,
            SignTransactionParams::default(),
            &NetworkHash::from(SHA256Hash::from([8; 32])),
        )
        .unwrap();
        assert_eq!(plan.sign(&owner).unwrap().tx_hex(), expected);
    }

    #[test]
    fn signed_bytes_match_canonical_signing_and_remain_network_bound() {
        let key = LocalSigner::from_secret_key([9; 32]);
        let plan = prepared();
        let expected = thruput_signing::sign_and_encode_transaction_hex(
            plan.transaction().clone(),
            &key,
            SignTransactionParams::default(),
            &NetworkHash::from(SHA256Hash::from([8; 32])),
        )
        .unwrap();
        let signed = plan.clone().sign(&key).unwrap();
        assert_eq!(signed.tx_hex(), expected);
        assert_eq!(
            signed.tx_hash(),
            TxHash(
                thruput_signing::transaction_hash(plan.transaction())
                    .unwrap()
                    .into()
            )
        );
        let mut elsewhere = plan;
        elsewhere.network_hash = NetworkHash::from(SHA256Hash::from([10; 32]));
        let elsewhere = elsewhere.sign(&key).unwrap();
        assert_eq!(elsewhere.tx_hash(), signed.tx_hash());
        assert_ne!(elsewhere.tx_hex(), signed.tx_hex());
    }

    #[test]
    fn wallet_dispatch_preserves_legacy_bytes_and_unwraps_native_returns() {
        let key = LocalSigner::from_secret_key([9; 32]);
        let account = signing::PubkeyAccount::from_public_key_bytes(key.verifying_key_bytes(), 17);
        let request = CallRequest {
            wallet: Wallet::Legacy {
                address: ContractAddress::from([1; 32]),
                entrypoint: ContractEntrypointID::from([2; 8]),
            },
            target_address: ContractAddress::from([3; 32]),
            target_entrypoint: ContractEntrypointID::from([4; 8]),
            target_calldata: &[5, 6],
            gas_limit: 100,
            target_gas_limit: 90,
            block_expiry_height: 1000,
            nonce: 7,
        };
        let network = NetworkHash::from(SHA256Hash::from([8; 32]));
        let legacy = prepare_call(request, network.clone()).unwrap();
        assert_eq!(legacy.transaction(), prepared().transaction());
        let legacy = legacy.sign(&key).unwrap();
        assert!(!legacy.is_native());
        assert_eq!(legacy.decode_return(&[42]).unwrap(), [42]);
        let native = prepare_call(
            CallRequest {
                wallet: Wallet::Native(&account),
                ..request
            },
            network,
        )
        .unwrap();
        assert_eq!(
            native.transaction().invocation.invoked_address,
            account.address()
        );
        let signed = native.sign(&key).unwrap();
        assert!(signed.is_native());
        let bytes = xdrs::cxs::encode(&vec![vec![42u8]]).unwrap();
        assert_eq!(signed.decode_return(&bytes).unwrap(), [42]);
        for returns in [vec![], vec![vec![1u8], vec![2u8]]] {
            assert!(
                signed
                    .decode_return(&xdrs::cxs::encode(&returns).unwrap())
                    .is_err()
            );
        }
        assert!(signed.decode_return(&[255]).is_err());
    }

    async fn client(module: RpcModule<()>) -> (rpc::HttpClient, jsonrpsee::server::ServerHandle) {
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let handle = server.start(module);
        (
            rpc::HttpClientBuilder::default()
                .build(format!("http://{address}"))
                .unwrap(),
            handle,
        )
    }

    #[tokio::test]
    async fn submission_error_and_poll_timeout_preserve_canonical_hash() {
        let signed = prepared()
            .sign(&LocalSigner::from_secret_key([9; 32]))
            .unwrap();
        let (client, handle) = client(RpcModule::new(())).await;
        match signed.submit(&client).await.unwrap_err() {
            SubmitCallError::Rpc { tx_hash, .. } => assert_eq!(tx_hash, signed.tx_hash()),
            other @ SubmitCallError::HashMismatch { .. } => panic!("unexpected {other:?}"),
        }
        let submitted = SubmittedCall {
            tx_hash: signed.tx_hash(),
        };
        match submitted
            .wait(
                &client,
                PollOptions {
                    max_attempts: 0,
                    interval: Duration::ZERO,
                },
            )
            .await
            .unwrap_err()
        {
            PollTransactionReceiptError::ReceiptTimeout { tx_hash, .. } => {
                assert_eq!(tx_hash, signed.tx_hash());
            }
            other => panic!("unexpected {other:?}"),
        }
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn mismatching_network_and_returned_submission_hash_are_rejected() {
        let mut module = RpcModule::new(());
        module
            .register_method("groundhog_networkInfo", |_, (), _| {
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(NetworkInfo::new(
                    types::NetworkHash::from(SHA256Hash::from([11; 32])),
                ))
            })
            .unwrap();
        module
            .register_method("groundhog_sendRawTransaction", |_, (), _| {
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(TxHash(
                    SHA256Hash::from([12; 32]).into(),
                ))
            })
            .unwrap();
        let (client, handle) = client(module).await;
        assert!(matches!(
            validate_network(&client, &NetworkHash::from(SHA256Hash::from([8; 32]))).await,
            Err(NetworkError::Mismatch { .. })
        ));
        let signed = prepared()
            .sign(&LocalSigner::from_secret_key([9; 32]))
            .unwrap();
        match signed.submit(&client).await.unwrap_err() {
            SubmitCallError::HashMismatch { tx_hash, returned } => {
                assert_eq!(tx_hash, signed.tx_hash());
                assert_eq!(returned, TxHash(SHA256Hash::from([12; 32]).into()));
            }
            other @ SubmitCallError::Rpc { .. } => panic!("unexpected {other:?}"),
        }
        handle.stop().unwrap();
    }
    #[tokio::test]
    async fn admitted_call_returns_failed_execution_receipt_without_resubmitting() {
        let signed = prepared()
            .sign(&LocalSigner::from_secret_key([9; 32]))
            .unwrap();
        let hash = signed.tx_hash();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_count = count.clone();
        let mut module = RpcModule::new(());
        module
            .register_method("groundhog_sendRawTransaction", move |_, (), _| {
                server_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(hash)
            })
            .unwrap();
        module
            .register_method("groundhog_getTransactionReceipt", move |_, (), _| {
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(Some(TransactionReceipt {
                    tx_hash: hash,
                    height: types::BlockHeight::from(42),
                    status: types::receipt::ExecStatus::Reverted {
                        message: Some("denied".into()),
                    },
                    events: vec![],
                    return_data: None,
                    abort_data: Some(types::hex::HexBytes(vec![1, 2, 3])),
                    gas_used: 1_500,
                }))
            })
            .unwrap();
        let (client, handle) = client(module).await;
        let admitted = signed.submit(&client).await.unwrap();
        let receipt = admitted
            .wait(&client, PollOptions::default())
            .await
            .unwrap();
        assert_eq!(receipt.tx_hash, hash);
        assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(matches!(
            receipt.status,
            types::receipt::ExecStatus::Reverted { .. }
        ));
        assert_eq!(receipt.abort_data.unwrap().0, vec![1, 2, 3]);
        handle.stop().unwrap();
    }
}
