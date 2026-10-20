//! Per-agent account and the chain-writing operations it performs.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use thruput_client::{CallRequest, DeployRequest, Wallet};
use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use thruput_json_rpc_types::transaction::TxHash;
use thruput_signing::default_wallet::DeploymentCode;
use thruput_signing::{DEFAULT_TX_GAS_LIMIT, PubkeyAccount};
use thruput_xdr::contract::{Contract, ContractEntrypointID};
use thruput_xdr::types::{ContractAddress, NetworkHash, SHA256Hash};

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
    pub initializer: Option<thruput_signing::default_wallet::WalletInitCall<'a>>,
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
    pub(crate) fn from_registration(
        network: Network,
        attempt: &ArtifactPendingRegistration,
    ) -> Self {
        Self {
            network,
            owner: attempt.owner,
            address: attempt.address,
            tx_hash: attempt.tx_hash,
            registered: true,
            tx_hex: attempt.tx_hex.clone(),
            effects: attempt.effects.clone(),
        }
    }

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
                status: thruput_json_rpc_types::transaction::TransactionStatus::Committed {
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
///
/// Clones share the signer and the sequence, so every clone draws distinct
/// nonces.
#[derive(Debug, Clone)]
pub(crate) struct Account {
    dev: Arc<TestAccount>,
    namespace: NonceNamespace,
    next_sequence: Arc<AtomicU32>,
}

impl Account {
    pub(crate) fn new(dev: TestAccount, namespace: NonceNamespace) -> Self {
        Self {
            dev: Arc::new(dev),
            namespace,
            next_sequence: Arc::new(AtomicU32::new(0)),
        }
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

    /// Claim the next nonce in this account's sequence.
    ///
    /// A nonce is consumed even if the transaction using it then fails to
    /// build or submit; nonces need only be unique, not contiguous.
    fn reserve_nonce(&self) -> Result<u64> {
        let sequence = self
            .next_sequence
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |seq| {
                seq.checked_add(1)
            })
            .map_err(|_| TestkitError::NonceExhausted {
                index: self.dev.index,
            })?;
        Ok(self.namespace.make_nonce(sequence))
    }

    pub(crate) async fn submit_deploy(
        &self,
        network: &Network,
        artifact: &[u8],
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let contract = contract_from_bytes(artifact.to_vec());
        let hash = contract_hash(&contract)?;
        let prepared = self.build_deploy(
            network,
            hash,
            Some(contract),
            self.reserve_nonce()?,
            options,
        )?;
        prepared.submit().await
    }

    pub(crate) async fn submit_deploy_entry(
        &self,
        network: &Network,
        entry: &ArtifactEntry,
        include_binary: bool,
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let contract = include_binary.then(|| entry.contract());
        let prepared = self.build_deploy(
            network,
            entry.hash(),
            contract,
            self.reserve_nonce()?,
            options,
        )?;
        prepared.submit().await
    }

    pub(crate) fn prepare_deploy_entry(
        &self,
        network: &Network,
        entry: &ArtifactEntry,
        include_binary: bool,
        options: DeployOptions<'_>,
    ) -> Result<PreparedDeploy> {
        let contract = include_binary.then(|| entry.contract());
        let prepared = self.build_deploy(
            network,
            entry.hash(),
            contract,
            self.reserve_nonce()?,
            options,
        )?;
        Ok(prepared)
    }

    /// Build, sign, and submit a deploy transaction. `contract` present registers
    /// the binary in this transaction; absent deploys by hash only.
    fn build_deploy(
        &self,
        network: &Network,
        hash: SHA256Hash,
        contract: Option<Contract>,
        tx_nonce: u64,
        options: DeployOptions<'_>,
    ) -> Result<PreparedDeploy> {
        let registered = contract.is_some();
        let account = native_account(&self.dev);
        let gas_limit = DEFAULT_TX_GAS_LIMIT
            .checked_add(options.initializer.map_or(0, |init| init.gas_limit))
            .ok_or_else(|| TestkitError::signing("initializer gas overflows deployment budget"))?;
        let signed = thruput_client::prepare_deploy(
            DeployRequest {
                account: &account,
                code: contract.map_or(DeploymentCode::Registered(hash), DeploymentCode::Attached),
                amount: options.amount,
                initializer: options.initializer,
                gas_limit,
                block_expiry_height: EXPIRY_HEIGHT,
                nonce: tx_nonce,
            },
            network_hash(&self.dev, "deploy")?,
        )
        .and_then(|plan| plan.sign(&self.dev.signer))
        .map_err(TestkitError::signing)?;
        let (address, tx_hash) = (signed.address(), signed.tx_hash());
        let tx_hex = signed.tx_hex().to_owned();
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
        &self,
        network: &Network,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<SubmittedTransaction> {
        let (tx_hash, tx_hex) = prepare_call(
            &self.dev,
            target,
            entrypoint,
            calldata,
            self.reserve_nonce()?,
        )?;
        submit_signed(network, tx_hash, &tx_hex).await?;
        Ok(SubmittedTransaction::new(network.clone(), tx_hash, tx_hex))
    }
}

/// The account's native nonce-zero `DefaultWallet` identity.
fn native_account(dev: &TestAccount) -> PubkeyAccount {
    PubkeyAccount::from_public_key_bytes(dev.signer.verifying_key_bytes(), 0)
}

fn network_hash(dev: &TestAccount, operation: &'static str) -> Result<NetworkHash> {
    dev.network_hash
        .clone()
        .ok_or(TestkitError::Offline { operation })
}

pub(crate) fn prepare_call(
    dev: &TestAccount,
    target: ContractAddress,
    entrypoint: ContractEntrypointID,
    calldata: &[u8],
    nonce: u64,
) -> Result<(TxHash, String)> {
    let account = native_account(dev);
    let signed = thruput_client::prepare_call(
        CallRequest {
            wallet: Wallet::Native(&account),
            target_address: target,
            target_entrypoint: entrypoint,
            target_calldata: calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT * 2,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: EXPIRY_HEIGHT,
            nonce,
        },
        network_hash(dev, "call")?,
    )
    .and_then(|plan| plan.sign(&dev.signer))
    .map_err(TestkitError::signing)?;
    Ok((signed.tx_hash(), signed.tx_hex().to_owned()))
}

pub(crate) async fn submit_signed(network: &Network, tx_hash: TxHash, tx_hex: &str) -> Result<()> {
    let client = network.require_client("sendRawTransaction")?;
    thruput_client::submit_signed_transaction(client, tx_hash, tx_hex)
        .await
        .map_err(|error| TestkitError::Submission {
            tx_hash,
            signed_transaction: tx_hex.to_owned(),
            detail: error.to_string(),
        })
}

pub(crate) fn verify_deployment_receipt(
    tx_hash: TxHash,
    address: ContractAddress,
    tx_hex: &str,
    receipt: &TransactionReceipt,
) -> Result<()> {
    thruput_client::SubmittedDeploy { tx_hash, address }
        .verify(receipt)
        .map_err(|error| TestkitError::UnverifiedDeployment {
            tx_hash,
            predicted_address: address,
            signed_transaction: tx_hex.to_owned(),
            detail: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use thruput_json_rpc_types::BlockHeight;
    use thruput_json_rpc_types::base58::Base58;
    use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
    use thruput_json_rpc_types::transaction::{TransactionStatus, TxHash};
    use thruput_xdr::types::{ContractAddress, SHA256Hash};

    use crate::artifact::{PendingRegistration, RegistrationOwner};
    use crate::error::TestkitError;
    use crate::manifest::derive_native_accounts;
    use crate::network::Network;
    use crate::nonce::NonceNamespace;

    use super::{Account, SubmittedDeploy};

    #[tokio::test]
    async fn recovered_committed_deploy_does_not_satisfy_pending_barrier() {
        let tx_hash = TxHash(Base58(SHA256Hash::from([0x11; 32])));
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

    #[test]
    fn clones_share_one_nonce_sequence() {
        let namespace = NonceNamespace::for_run("seed", 0);
        let dev = derive_native_accounts(1, "seed", None).remove(0);
        let account = Account::new(dev, namespace);
        let clone = account.clone();

        assert_eq!(account.reserve_nonce().unwrap(), namespace.make_nonce(0));
        assert_eq!(clone.reserve_nonce().unwrap(), namespace.make_nonce(1));
        assert_eq!(account.reserve_nonce().unwrap(), namespace.make_nonce(2));
    }

    #[test]
    fn exhausted_sequence_is_an_error() {
        let dev = derive_native_accounts(1, "seed", None).remove(0);
        let account = Account::new(dev, NonceNamespace::for_run("seed", 0));
        account
            .next_sequence
            .store(u32::MAX, std::sync::atomic::Ordering::Relaxed);

        assert!(matches!(
            account.reserve_nonce(),
            Err(TestkitError::NonceExhausted { index: 0 })
        ));
    }
}
