//! Per-agent account and the chain-writing operations it performs.

use std::sync::Arc;

use groundhog_json_rpc_types::hex::Hex0x;
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::default_wallet::{
    BuildDefaultWalletCallTransaction, BuildDefaultWalletDeployTransaction, DeploymentCode,
    build_default_wallet_call_transaction, build_default_wallet_deploy_transaction,
};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    build_dev_wallet_do_action_transaction, derive_deploy_address, sign_and_encode_transaction_hex,
    transaction_hash,
};
use groundhog_xdr::contract::{Contract, ContractEntrypointID};
use groundhog_xdr::types::{ContractAddress, SHA256Hash};
use sha2::{Digest, Sha256};

use crate::artifact::{
    ArtifactEntry, PendingRegistration as ArtifactPendingRegistration, RegistrationOwner,
    contract_from_bytes, contract_hash,
};
use crate::error::{Result, TestkitError};
use crate::manifest::TestAccount;
use crate::network::{Network, SubmittedTransaction};
use crate::nonce::NonceNamespace;

/// Absolute block height at which harness-built transactions expire. Chosen far
/// enough ahead that a short scenario never reaches it.
const EXPIRY_HEIGHT: i64 = 10_000_000;

/// Explicit funding and optional initialization in the deployment transaction.
#[derive(Debug, Clone, Copy)]
pub struct DeployOptions<'a> {
    /// Exact raw HOG amount transferred to the new contract. Zero skips funding.
    pub amount: u128,
    /// Optional invocation on the new contract, called by its deploying wallet.
    pub initializer: Option<groundhog_signing::default_wallet::WalletInitCall<'a>>,
}

impl DeployOptions<'_> {
    pub(crate) fn effects(self) -> Result<Vec<u8>> {
        xdrs::cxs::encode(&(
            self.amount,
            self.initializer
                .map(|init| (init.entrypoint, init.gas_limit, init.calldata)),
        ))
        .map_err(TestkitError::encode)
    }

    /// Deploy with an exact amount and no initializer.
    #[must_use]
    pub const fn funded(amount: u128) -> Self {
        Self {
            amount,
            initializer: None,
        }
    }
}

/// Result of a contract deploy.
#[derive(Debug)]
pub struct Deployed {
    /// Address of the deployed contract instance.
    pub address: ContractAddress,
    /// Hash of the deploy transaction.
    pub tx_hash: TxHash,
    /// Deploy transaction receipt.
    pub receipt: TransactionReceipt,
    /// Whether this transaction carried the contract binary (registered it).
    pub registered: bool,
}

/// A submitted deployment whose receipt can be awaited after manual block production.
#[derive(Debug)]
#[must_use = "submitted deployments should be awaited or intentionally retained"]
pub struct SubmittedDeploy {
    network: Network,
    address: ContractAddress,
    tx_hash: TxHash,
    registered: bool,
    tx_hex: String,
    cached_receipt: Option<TransactionReceipt>,
    registration: Option<PendingRegistration>,
}

#[derive(Debug)]
struct PendingRegistration {
    entry: Arc<ArtifactEntry>,
}

#[derive(Debug)]
pub(crate) struct PreparedDeploy {
    network: Network,
    owner: RegistrationOwner,
    address: ContractAddress,
    tx_hash: TxHash,
    registered: bool,
    tx_hex: String,
    effects: Vec<u8>,
}

impl PreparedDeploy {
    pub(crate) fn pending_registration(&self) -> ArtifactPendingRegistration {
        ArtifactPendingRegistration {
            tx_hash: self.tx_hash,
            tx_hex: self.tx_hex.clone(),
            owner: self.owner,
            effects: self.effects.clone(),
            address: self.address,
        }
    }

    pub(crate) async fn submit(self) -> Result<SubmittedDeploy> {
        submit_signed(&self.network, self.tx_hash, &self.tx_hex).await?;
        Ok(SubmittedDeploy {
            network: self.network,
            address: self.address,
            tx_hash: self.tx_hash,
            registered: self.registered,
            tx_hex: self.tx_hex,
            cached_receipt: None,
            registration: None,
        })
    }
}

impl SubmittedDeploy {
    /// Address derived for the contract instance before submission.
    pub fn address(&self) -> ContractAddress {
        self.address
    }

    /// Original signed bytes retained for reconciliation.
    pub fn signed_transaction(&self) -> &str {
        &self.tx_hex
    }

    /// Canonical deployment transaction hash.
    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    /// Wait until the scheduler reports this deployment as pending.
    pub async fn await_pending(&self) -> Result<()> {
        if let Some(receipt) = &self.cached_receipt {
            return Err(TestkitError::TransactionNotPending {
                tx_hash: format!("{:?}", self.tx_hash),
                status: groundhog_json_rpc_types::transaction::TransactionStatus::Committed {
                    height: receipt.height.clone(),
                },
            });
        }
        self.network.await_pending(self.tx_hash).await
    }

    /// Wait for the deployment receipt and require successful execution.
    ///
    /// For a registry artifact carrying its binary, successful completion marks
    /// the retained registration attempt as committed.
    pub async fn await_receipt(&self) -> Result<Deployed> {
        let receipt = match &self.cached_receipt {
            Some(receipt) => receipt.clone(),
            None => self
                .network
                .await_receipt(self.tx_hash)
                .await
                .map_err(|error| TestkitError::UnverifiedDeployment {
                    tx_hash: self.tx_hash,
                    predicted_address: self.address,
                    signed_transaction: self.tx_hex.clone(),
                    detail: error.to_string(),
                })?,
        };
        verify_deployment_receipt(self.tx_hash, self.address, &self.tx_hex, &receipt)?;
        if !matches!(receipt.status, ExecStatus::Ok) {
            self.finish_registration(false).await;
            return Err(TestkitError::Deploy {
                detail: format!("{:?}", receipt.status),
            });
        }
        self.finish_registration(true).await;
        Ok(Deployed {
            address: self.address,
            tx_hash: self.tx_hash,
            receipt,
            registered: self.registered,
        })
    }

    pub(crate) fn with_registration(mut self, entry: Arc<ArtifactEntry>) -> Self {
        self.registration = Some(PendingRegistration { entry });
        self
    }

    pub(crate) fn recovered(
        network: Network,
        attempt: &ArtifactPendingRegistration,
        receipt: TransactionReceipt,
    ) -> Self {
        Self {
            network,
            address: attempt.address,
            tx_hash: attempt.tx_hash,
            registered: true,
            tx_hex: attempt.tx_hex.clone(),
            cached_receipt: Some(receipt),
            registration: None,
        }
    }

    async fn finish_registration(&self, succeeded: bool) {
        let Some(registration) = &self.registration else {
            return;
        };
        registration
            .entry
            .finish_registration(self.tx_hash, succeeded)
            .await;
    }
}

/// An account assigned to one agent, tracking its own nonce sequence.
#[derive(Debug)]
pub(crate) struct Account {
    dev: TestAccount,
    namespace: NonceNamespace,
    next_sequence: u32,
}

impl Account {
    pub(crate) fn new(dev: TestAccount, namespace: NonceNamespace) -> Self {
        Self {
            dev,
            namespace,
            next_sequence: 0,
        }
    }

    pub(crate) fn is_native(&self) -> bool {
        self.dev.native
    }

    pub(crate) fn index(&self) -> u32 {
        self.dev.index
    }

    pub(crate) fn address(&self) -> ContractAddress {
        self.dev.wallet_address
    }

    pub(crate) fn registration_owner(&self) -> RegistrationOwner {
        RegistrationOwner {
            address: self.dev.wallet_address,
            namespace: self.namespace,
        }
    }

    fn next_nonce(&self) -> u64 {
        self.namespace.make_nonce(self.next_sequence)
    }

    fn advance(&mut self) -> Result<()> {
        self.next_sequence =
            self.next_sequence
                .checked_add(1)
                .ok_or(TestkitError::NonceExhausted {
                    index: self.dev.index,
                })?;
        Ok(())
    }

    pub(crate) async fn submit_deploy(
        &mut self,
        network: &Network,
        artifact: &[u8],
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let contract = contract_from_bytes(artifact.to_vec());
        let hash = contract_hash(&contract)?;
        let prepared = self.build_deploy(network, hash, Some(contract), options)?;
        self.advance()?;
        prepared.submit().await
    }

    pub(crate) async fn submit_deploy_entry(
        &mut self,
        network: &Network,
        entry: &ArtifactEntry,
        include_binary: bool,
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let contract = include_binary.then(|| entry.contract());
        let prepared = self.build_deploy(network, entry.hash(), contract, options)?;
        self.advance()?;
        prepared.submit().await
    }

    pub(crate) fn prepare_deploy_entry(
        &mut self,
        network: &Network,
        entry: &ArtifactEntry,
        include_binary: bool,
        options: DeployOptions<'_>,
    ) -> Result<PreparedDeploy> {
        let contract = include_binary.then(|| entry.contract());
        let prepared = self.build_deploy(network, entry.hash(), contract, options)?;
        self.advance()?;
        Ok(prepared)
    }

    /// Build, sign, and submit a deploy transaction. `contract` present registers
    /// the binary in this transaction; absent deploys by hash only.
    fn build_deploy(
        &self,
        network: &Network,
        hash: SHA256Hash,
        contract: Option<Contract>,
        options: DeployOptions<'_>,
    ) -> Result<PreparedDeploy> {
        if !self.dev.native {
            return Err(TestkitError::LegacyDeploymentUnsupported);
        }
        let tx_nonce = self.next_nonce();
        let deploy_nonce =
            SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(tx_nonce.to_le_bytes())));
        let address = derive_deploy_address(&self.dev.wallet_address, &deploy_nonce, &hash)
            .map_err(TestkitError::signing)?;

        let registered = contract.is_some();
        let account = groundhog_signing::PubkeyAccount::from_public_key_bytes(
            self.dev.signer.verifying_key_bytes(),
            0,
        );
        let gas_limit = DEFAULT_TX_GAS_LIMIT
            .checked_add(options.initializer.map_or(0, |init| init.gas_limit))
            .ok_or_else(|| TestkitError::signing("initializer gas overflows deployment budget"))?;
        let tx = build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
            account: &account,
            deployment_nonce: deploy_nonce,
            code: contract.map_or(DeploymentCode::Registered(hash), DeploymentCode::Attached),
            amount: options.amount,
            initializer: options.initializer,
            gas_limit,
            block_expiry_height: EXPIRY_HEIGHT,
            nonce: tx_nonce,
        })
        .map_err(TestkitError::signing)?;
        let tx_hash = TxHash(Hex0x(transaction_hash(&tx).map_err(TestkitError::signing)?));
        let tx_hex = sign_and_encode_transaction_hex(
            tx,
            &self.dev.signer,
            SignTransactionParams::default(),
            self.dev
                .network_hash
                .as_ref()
                .ok_or(TestkitError::Offline {
                    operation: "deploy",
                })?,
        )
        .map_err(TestkitError::signing)?;
        Ok(PreparedDeploy {
            network: network.clone(),
            owner: self.registration_owner(),
            address,
            tx_hash,
            registered,
            tx_hex,
            effects: options.effects()?,
        })
    }

    pub(crate) async fn submit_call(
        &mut self,
        network: &Network,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<SubmittedTransaction> {
        let (tx_hash, tx_hex) = prepare_call(
            &self.dev,
            network,
            target,
            entrypoint,
            calldata,
            self.next_nonce(),
        )?;
        self.advance()?;
        submit_signed(network, tx_hash, &tx_hex).await?;
        Ok(SubmittedTransaction::new(network.clone(), tx_hash, tx_hex))
    }
}

pub(crate) fn prepare_call(
    dev: &TestAccount,
    network: &Network,
    target: ContractAddress,
    entrypoint: ContractEntrypointID,
    calldata: &[u8],
    nonce: u64,
) -> Result<(TxHash, String)> {
    let tx = if dev.native {
        let account = groundhog_signing::PubkeyAccount::from_public_key_bytes(
            dev.signer.verifying_key_bytes(),
            0,
        );
        build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: target,
            target_entrypoint: entrypoint,
            target_calldata: calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT * 2,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: EXPIRY_HEIGHT,
            nonce,
        })
    } else {
        build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
            wallet_address: dev.wallet_address,
            wallet_entrypoint: network.do_action_entrypoint()?,
            target_address: target,
            target_entrypoint: entrypoint,
            target_calldata: calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: EXPIRY_HEIGHT,
            nonce,
        })
    }
    .map_err(TestkitError::signing)?;
    let tx_hash = TxHash(Hex0x(transaction_hash(&tx).map_err(TestkitError::signing)?));
    let tx_hex = sign_and_encode_transaction_hex(
        tx,
        &dev.signer,
        SignTransactionParams::default(),
        dev.network_hash
            .as_ref()
            .ok_or(TestkitError::Offline { operation: "call" })?,
    )
    .map_err(TestkitError::signing)?;

    Ok((tx_hash, tx_hex))
}

pub(crate) async fn submit_signed(network: &Network, tx_hash: TxHash, tx_hex: &str) -> Result<()> {
    let detail = match network.submit_raw(tx_hex.to_owned()).await {
        Ok(actual) if actual == tx_hash => return Ok(()),
        Ok(actual) => format!("returned transaction hash {actual:?}"),
        Err(error) => error.to_string(),
    };
    Err(TestkitError::Submission {
        tx_hash,
        signed_transaction: tx_hex.to_owned(),
        detail,
    })
}

pub(crate) fn verify_deployment_receipt(
    tx_hash: TxHash,
    address: ContractAddress,
    tx_hex: &str,
    receipt: &TransactionReceipt,
) -> Result<()> {
    let unverified = |detail: &str| TestkitError::UnverifiedDeployment {
        tx_hash,
        predicted_address: address,
        signed_transaction: tx_hex.to_owned(),
        detail: detail.to_owned(),
    };
    if receipt.tx_hash != tx_hash {
        return Err(unverified("receipt hash mismatch"));
    }
    if matches!(receipt.status, ExecStatus::Ok)
        && receipt
            .return_data
            .as_ref()
            .is_none_or(|data| data.as_ref() != address.as_ref())
    {
        return Err(unverified(
            "receipt did not return the exact predicted address",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::BlockHeight;
    use groundhog_json_rpc_types::hex::Hex0x;
    use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
    use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};

    use crate::artifact::{PendingRegistration, RegistrationOwner};
    use crate::error::TestkitError;
    use crate::network::Network;
    use crate::nonce::NonceNamespace;

    use super::SubmittedDeploy;

    #[tokio::test]
    async fn recovered_committed_deploy_does_not_satisfy_pending_barrier() {
        let tx_hash = TxHash(Hex0x(SHA256Hash::from([0x11; 32])));
        let address = ContractAddress::from([0x22; 32]);
        let attempt = PendingRegistration {
            tx_hash,
            tx_hex: "0x01".to_owned(),
            effects: Vec::new(),
            owner: RegistrationOwner {
                address,
                namespace: NonceNamespace::for_run("seed", 0),
            },
            address,
        };
        let submitted = SubmittedDeploy::recovered(
            Network::offline(),
            &attempt,
            TransactionReceipt {
                tx_hash,
                height: BlockHeight::from(7),
                status: ExecStatus::Ok,
                events: Vec::new(),
                return_data: None,
                abort_data: None,
                gas_used: 1_500,
            },
        );

        let error = submitted
            .await_pending()
            .await
            .expect_err("bug: a committed deployment is no longer pending");
        assert!(matches!(
            error,
            TestkitError::TransactionNotPending {
                status: TransactionStatus::Committed { height },
                ..
            } if height == BlockHeight::from(7)
        ));
    }
}
