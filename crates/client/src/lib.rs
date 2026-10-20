//! Contract invocation without CLI configuration, terminal output, or runtime ownership.
//!
//! Callers resolve addresses, wallet entrypoints, and calldata, verify network identity,
//! then prepare, inspect, sign, submit, and wait as separate operations. Calls go through
//! [`prepare_call`]; native deployments through [`prepare_deploy`].

pub mod codec;
mod deploy;
mod native;
pub use native::{
    NativeRpcError, SubmitCallError, SubmittedCall, check_endpoint, resolve_block_expiry_height,
    submit_signed_transaction, validate_network,
};
pub use thruput_json_rpc_client::native::NativeClient;
pub use thruput_json_rpc_client::native::receipt::{ReceiptPollConfig, ReceiptPollError};

pub use deploy::{
    DeployRequest, DeploymentVerificationError, SignedDeploy, SubmittedDeploy, UnsignedDeploy,
    contract_hash, deployment_nonce, prepare_deploy,
};
pub use jsonrpsee;
pub use thruput_contract_spec as spec;
pub use thruput_json_rpc_client as rpc;
pub use thruput_json_rpc_types as types;
pub use thruput_signing as signing;
/// Shared C/G/S text encodings; independent of either CLI.
pub use thruput_signing::strkey;
pub use thruput_xdr as xdr;

use thruput_json_rpc_types::transaction::TxHash;
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
        let (tx_hash, tx_hex) = sign_transaction(self.transaction, &self.network_hash, signer)?;
        Ok(SignedCall {
            tx_hash,
            tx_hex,
            native_return: self.native_return,
        })
    }
}

/// Sign a prepared transaction, rejecting a signer whose public key differs from
/// a native account's auth claim. Returns the canonical hash and signed hex.
fn sign_transaction(
    transaction: Transaction,
    network_hash: &NetworkHash,
    signer: &impl Signer,
) -> Result<(TxHash, String), SigningError> {
    let tx_hash = TxHash(thruput_signing::transaction_hash(&transaction)?.into());
    let tx_hex = thruput_signing::sign_and_encode_transaction_hex(
        transaction,
        signer,
        SignTransactionParams::default(),
        network_hash,
    )?;
    Ok((tx_hash, tx_hex))
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
    /// Canonical signed transaction encoded for `sendTransaction`.
    pub fn tx_hex(&self) -> &str {
        &self.tx_hex
    }
    /// Canonical transaction hash, also available if submission has an ambiguous outcome.
    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
