//! Per-agent funded account and the chain-writing operations it performs.

use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::{
    BuildDeployTransaction, BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT,
    SignTransactionParams, build_deploy_transaction, build_dev_wallet_do_action_transaction,
    derive_deploy_address, sign_and_encode_transaction_hex, source_address_auth_claim,
};
use groundhog_xdr::contract::{Contract, ContractEntrypointID};
use groundhog_xdr::types::{ContractAddress, SHA256Hash};
use sha2::{Digest, Sha256};
use xdr_common::VecM;

use crate::artifact::{ArtifactEntry, ArtifactKind, contract_from_bytes, contract_hash};
use crate::error::{Result, TestkitError};
use crate::manifest::DevAccount;
use crate::network::Network;
use crate::nonce::NonceNamespace;

/// Absolute block height at which harness-built transactions expire. Chosen far
/// enough ahead that a short scenario never reaches it. Mirrors `hogsim`.
const EXPIRY_HEIGHT: i64 = 10_000_000;

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

/// A funded dev account assigned to one agent, tracking its own nonce sequence.
#[derive(Debug)]
pub(crate) struct Account {
    dev: DevAccount,
    namespace: NonceNamespace,
    next_sequence: u32,
}

impl Account {
    pub(crate) fn new(dev: DevAccount, namespace: NonceNamespace) -> Self {
        Self {
            dev,
            namespace,
            next_sequence: 0,
        }
    }

    pub(crate) fn index(&self) -> u32 {
        self.dev.index
    }

    pub(crate) fn address(&self) -> ContractAddress {
        self.dev.wallet_address
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

    /// Deploy raw artifact bytes, always carrying the binary (self-registering).
    pub(crate) async fn deploy(
        &mut self,
        network: &Network,
        artifact: &[u8],
        kind: ArtifactKind,
    ) -> Result<Deployed> {
        let contract = contract_from_bytes(kind, artifact.to_vec());
        let hash = contract_hash(&contract)?;
        self.deploy_with(network, hash, Some(contract)).await
    }

    /// Deploy an instance of a registry artifact. When `include_binary` is set,
    /// the transaction carries the binary (registering it in the same tx);
    /// otherwise it references the contract by hash only.
    pub(crate) async fn deploy_entry(
        &mut self,
        network: &Network,
        entry: &ArtifactEntry,
        include_binary: bool,
    ) -> Result<Deployed> {
        let contract = include_binary.then(|| entry.contract());
        self.deploy_with(network, entry.hash(), contract).await
    }

    /// Build, sign, submit, and await a deploy transaction. `contract` present
    /// registers the binary in this transaction; absent deploys by hash only.
    async fn deploy_with(
        &mut self,
        network: &Network,
        hash: SHA256Hash,
        contract: Option<Contract>,
    ) -> Result<Deployed> {
        let entrypoint = network.deploy_entrypoint()?;
        let tx_nonce = self.next_nonce();
        let deploy_nonce =
            SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(tx_nonce.to_le_bytes())));
        let address = derive_deploy_address(&self.dev.wallet_address, &deploy_nonce, &hash)
            .map_err(TestkitError::signing)?;

        // Calldata is CXS-encoded (nonce, contract_hash): the wallet deploy
        // entrypoint decodes calldata with the contract (CXS) codec.
        let calldata = xdrs::cxs::encode(&(&deploy_nonce, &hash)).map_err(TestkitError::encode)?;

        let registered = contract.is_some();
        let contracts_to_deploy: VecM<Contract> = VecM(contract.into_iter().collect());

        let tx = build_deploy_transaction(BuildDeployTransaction {
            invoked_address: self.dev.wallet_address,
            entrypoint,
            calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: EXPIRY_HEIGHT,
            nonce: tx_nonce,
            contracts_to_deploy,
            auth_claim: source_address_auth_claim(),
        });
        let tx_hex =
            sign_and_encode_transaction_hex(tx, &self.dev.signer, SignTransactionParams::default())
                .map_err(TestkitError::signing)?;

        let tx_hash = network.submit_raw(tx_hex).await?;
        self.advance()?;
        let receipt = network.await_receipt(tx_hash).await?;
        if !matches!(receipt.status, ExecStatus::Ok) {
            return Err(TestkitError::Deploy {
                detail: format!("{:?}", receipt.status),
            });
        }

        Ok(Deployed {
            address,
            tx_hash,
            receipt,
            registered,
        })
    }

    /// Call a deployed contract method through this account's dev wallet,
    /// waiting for the receipt. Non-`Ok` execution is returned in the receipt,
    /// not raised as an error, so callers can assert on reverts.
    pub(crate) async fn call(
        &mut self,
        network: &Network,
        target: ContractAddress,
        entrypoint: [u8; 8],
        calldata: &[u8],
    ) -> Result<TransactionReceipt> {
        let wallet_entrypoint = network.do_action_entrypoint()?;
        let nonce = self.next_nonce();

        let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
            wallet_address: self.dev.wallet_address,
            wallet_entrypoint,
            target_address: target,
            target_entrypoint: ContractEntrypointID::from(entrypoint),
            target_calldata: calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: EXPIRY_HEIGHT,
            nonce,
        })
        .map_err(TestkitError::signing)?;
        let tx_hex =
            sign_and_encode_transaction_hex(tx, &self.dev.signer, SignTransactionParams::default())
                .map_err(TestkitError::signing)?;

        let tx_hash = network.submit_raw(tx_hex).await?;
        self.advance()?;
        network.await_receipt(tx_hash).await
    }
}
