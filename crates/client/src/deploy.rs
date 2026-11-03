//! Native contract deployment through an account's `DefaultWallet`.
//!
//! Deployment follows the same prepare, sign, submit, and wait sequence as calls. The new
//! contract's address is derived before signing, so callers can record it before submission
//! and verify it against the receipt afterwards.

use jsonrpsee::core::client::ClientT;
use sha2::{Digest, Sha256};
use thruput_json_rpc_client::native::{
    NativeClient,
    receipt::{ReceiptPollConfig, ReceiptPollError},
};
use thruput_json_rpc_types::{
    responses::{ReceiptExecution, TransactionReceipt},
    transaction::TxHash,
};
use thruput_signing::default_wallet::{
    BuildDefaultWalletDeployTransaction, DeploymentCode, WalletInitCall,
    build_default_wallet_deploy_transaction,
};
use thruput_signing::{PubkeyAccount, Signer, SigningError};
use thruput_xdr::{
    contract::Contract,
    transaction::Transaction,
    types::{ContractAddress, NetworkHash, SHA256Hash},
};

use crate::{SubmitCallError, SubmittedCall, sign_transaction};

/// A native deployment from `account`'s `DefaultWallet`, with optional funding and
/// initialization in the same transaction.
#[derive(Debug, Clone)]
pub struct DeployRequest<'a> {
    /// Deploying native account.
    pub account: &'a PubkeyAccount,
    /// Binary attached to this transaction, or the hash of an already registered binary.
    pub code: DeploymentCode,
    /// Exact raw native token amount transferred to the new contract. Zero skips funding.
    pub amount: u128,
    /// Invocation of the new contract by the deploying wallet.
    pub initializer: Option<WalletInitCall<'a>>,
    /// Root gas budget, which must cover any initializer's gas limit.
    pub gas_limit: u64,
    /// Transaction expiration height.
    pub block_expiry_height: i64,
    /// Transaction uniquifier. The deployment salt is [`deployment_nonce`] of this value.
    pub nonce: u64,
}

/// Hash identifying a contract binary on the network.
pub fn contract_hash(contract: &Contract) -> Result<SHA256Hash, SigningError> {
    thruput_signing::wallet_contract_hash(contract)
}

/// Deployment salt derived from a transaction nonce: SHA-256 of its little-endian bytes.
pub fn deployment_nonce(nonce: u64) -> SHA256Hash {
    SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(nonce.to_le_bytes())))
}

/// Prepare a native deployment without submitting it, deriving the new contract's address.
pub fn prepare_deploy(
    request: DeployRequest<'_>,
    network_hash: NetworkHash,
) -> Result<UnsignedDeploy, SigningError> {
    let code_hash = match &request.code {
        DeploymentCode::Attached(contract) => contract_hash(contract)?,
        DeploymentCode::Registered(hash) => *hash,
    };
    let salt = deployment_nonce(request.nonce);
    let address =
        thruput_signing::derive_deploy_address(&request.account.address(), &salt, &code_hash)?;
    let transaction =
        build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
            account: request.account,
            deployment_nonce: salt,
            code: request.code,
            amount: request.amount,
            initializer: request.initializer,
            gas_limit: request.gas_limit,
            block_expiry_height: request.block_expiry_height,
            nonce: request.nonce,
        })?;
    Ok(UnsignedDeploy {
        transaction,
        network_hash,
        address,
        code_hash,
    })
}

/// An inspectable unsigned deployment, bound to the intended network.
#[derive(Debug, Clone)]
pub struct UnsignedDeploy {
    transaction: Transaction,
    network_hash: NetworkHash,
    address: ContractAddress,
    code_hash: SHA256Hash,
}

impl UnsignedDeploy {
    /// The exact transaction body that will be signed.
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    /// Address the contract will have if this transaction succeeds.
    pub fn address(&self) -> ContractAddress {
        self.address
    }

    /// Hash of the deployed binary.
    pub fn code_hash(&self) -> SHA256Hash {
        self.code_hash
    }

    /// Sign this deployment without submitting it. Reject a signer whose public key
    /// differs from the deploying account's auth claim.
    pub fn sign(self, signer: &impl Signer) -> Result<SignedDeploy, SigningError> {
        let (tx_hash, tx_hex) = sign_transaction(self.transaction, &self.network_hash, signer)?;
        Ok(SignedDeploy {
            tx_hash,
            tx_hex,
            address: self.address,
        })
    }
}

/// A signed deployment with its canonical hash and predicted address available before
/// any network request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedDeploy {
    tx_hash: TxHash,
    tx_hex: String,
    address: ContractAddress,
}

impl SignedDeploy {
    /// Canonical signed transaction encoded for `sendTransaction`.
    pub fn tx_hex(&self) -> &str {
        &self.tx_hex
    }

    /// Canonical transaction hash, also available if submission has an ambiguous outcome.
    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    /// Address the contract will have if this transaction succeeds.
    pub fn address(&self) -> ContractAddress {
        self.address
    }

    /// Submit exactly these signed bytes once. Errors retain the original hash for reconciliation.
    pub async fn submit<C: ClientT + Sync>(
        &self,
        client: &NativeClient<C>,
    ) -> Result<SubmittedDeploy, SubmitCallError> {
        crate::submit_signed_transaction(client, self.tx_hash, &self.tx_hex).await?;
        Ok(SubmittedDeploy {
            tx_hash: self.tx_hash,
            address: self.address,
        })
    }
}

/// An admitted deployment, whose hash and predicted address can be saved and checked
/// again without resubmission.
#[derive(Debug, Clone, Copy)]
pub struct SubmittedDeploy {
    pub tx_hash: TxHash,
    pub address: ContractAddress,
}

impl SubmittedDeploy {
    /// Wait for the authoritative receipt, including unsuccessful execution receipts.
    /// Check it with [`verify`](Self::verify). This method never resubmits.
    pub async fn wait<C: ClientT + Sync, F: std::future::Future<Output = ()>>(
        &self,
        client: &NativeClient<C>,
        network_hash: thruput_json_rpc_types::NetworkHash,
        config: ReceiptPollConfig,
        cancelled: F,
    ) -> Result<TransactionReceipt, ReceiptPollError> {
        SubmittedCall {
            tx_hash: self.tx_hash,
        }
        .wait(client, network_hash, config, cancelled)
        .await
    }

    /// Require `receipt` to belong to this deployment and, when execution succeeded, to
    /// return exactly the predicted address. Unsuccessful execution is not an error here;
    /// callers inspect `receipt.execution`.
    pub fn verify(&self, receipt: &TransactionReceipt) -> Result<(), DeploymentVerificationError> {
        if receipt.transaction_hash != self.tx_hash {
            return Err(DeploymentVerificationError::ReceiptHashMismatch {
                tx_hash: self.tx_hash,
                receipt: receipt.transaction_hash,
            });
        }
        if let ReceiptExecution::Return { return_data } = &receipt.execution
            && return_data.as_ref() != self.address.as_ref()
        {
            return Err(DeploymentVerificationError::AddressMismatch {
                tx_hash: self.tx_hash,
                predicted: self.address,
            });
        }
        Ok(())
    }
}

/// A receipt that does not confirm the predicted deployment.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeploymentVerificationError {
    /// The receipt belongs to a different transaction.
    #[error("deployment {tx_hash:?} received a receipt for {receipt:?}")]
    ReceiptHashMismatch { tx_hash: TxHash, receipt: TxHash },
    /// Successful execution did not return the predicted address.
    #[error("deployment {tx_hash:?} did not return the predicted address {predicted:?}")]
    AddressMismatch {
        tx_hash: TxHash,
        predicted: ContractAddress,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::{BlockHeight, hex::HexBytes};
    use thruput_signing::LocalSigner;
    use thruput_xdr::contract::ContractBlob;
    use thruput_xdr::contract::ContractEntrypointID;
    use xdrs::BytesM;

    fn network() -> NetworkHash {
        NetworkHash::from(SHA256Hash::from([8; 32]))
    }

    fn contract() -> Contract {
        Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(vec![1, 2, 3]),
            },
        }
    }

    fn request(account: &PubkeyAccount, code: DeploymentCode) -> DeployRequest<'_> {
        DeployRequest {
            account,
            code,
            amount: 5,
            initializer: Some(WalletInitCall {
                entrypoint: ContractEntrypointID::from([4; 8]),
                gas_limit: 50,
                calldata: &[6],
            }),
            gas_limit: 100,
            block_expiry_height: 1000,
            nonce: 7,
        }
    }

    fn receipt(tx_hash: TxHash, return_data: Option<Vec<u8>>) -> TransactionReceipt {
        TransactionReceipt {
            invoked_addresses: vec![],
            transaction_hash: tx_hash,
            block_height: BlockHeight::from(1),
            execution: ReceiptExecution::Return {
                return_data: HexBytes(return_data.unwrap_or_default()),
            },
            events: vec![],
            gas_used: 1,
        }
    }

    #[test]
    fn prepared_deploy_matches_canonical_builder_and_address() {
        let key = LocalSigner::from_secret_key([9; 32]);
        let account = PubkeyAccount::new(key.public_key(), 0);
        let plan = prepare_deploy(
            request(&account, DeploymentCode::Attached(contract())),
            network(),
        )
        .unwrap();

        let hash = contract_hash(&contract()).unwrap();
        let salt = SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(7u64.to_le_bytes())));
        assert_eq!(deployment_nonce(7), salt);
        assert_eq!(plan.code_hash(), hash);
        assert_eq!(
            plan.address(),
            thruput_signing::derive_deploy_address(&account.address(), &salt, &hash).unwrap()
        );
        let expected =
            build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
                account: &account,
                deployment_nonce: salt,
                code: DeploymentCode::Attached(contract()),
                amount: 5,
                initializer: request(&account, DeploymentCode::Registered(hash)).initializer,
                gas_limit: 100,
                block_expiry_height: 1000,
                nonce: 7,
            })
            .unwrap();
        assert_eq!(plan.transaction(), &expected);

        // Registering and deploying by hash share the address.
        let registered = prepare_deploy(
            request(&account, DeploymentCode::Registered(hash)),
            network(),
        )
        .unwrap();
        assert_eq!(registered.address(), plan.address());
        assert!(registered.transaction().contracts_to_deploy.0.is_empty());

        let signed = plan.sign(&key).unwrap();
        assert_eq!(
            signed.tx_hash(),
            TxHash(thruput_signing::transaction_hash(&expected).unwrap().into())
        );
        assert_eq!(signed.address(), registered.address());
    }

    #[test]
    fn deploy_signer_mismatch_is_rejected() {
        let owner = LocalSigner::from_secret_key([9; 32]);
        let account = PubkeyAccount::new(owner.public_key(), 0);
        let plan = prepare_deploy(
            request(&account, DeploymentCode::Attached(contract())),
            network(),
        )
        .unwrap();
        assert!(matches!(
            plan.sign(&LocalSigner::from_secret_key([10; 32])),
            Err(SigningError::SignerMismatch)
        ));
    }

    #[test]
    fn verification_requires_matching_hash_and_exact_address() {
        let tx_hash = TxHash(SHA256Hash::from([1; 32]).into());
        let address = ContractAddress::from([2; 32]);
        let submitted = SubmittedDeploy { tx_hash, address };

        submitted
            .verify(&receipt(tx_hash, Some(vec![2; 32])))
            .unwrap();
        let other = TxHash(SHA256Hash::from([3; 32]).into());
        assert!(matches!(
            submitted.verify(&receipt(other, Some(vec![2; 32]))),
            Err(DeploymentVerificationError::ReceiptHashMismatch { .. })
        ));
        for returned in [None, Some(vec![3; 32]), Some(vec![2; 31])] {
            assert!(matches!(
                submitted.verify(&receipt(tx_hash, returned)),
                Err(DeploymentVerificationError::AddressMismatch { .. })
            ));
        }
        let reverted = TransactionReceipt {
            execution: ReceiptExecution::Abort {
                abort_data: HexBytes(vec![]),
            },
            ..receipt(tx_hash, None)
        };
        submitted.verify(&reverted).unwrap();
    }
}
