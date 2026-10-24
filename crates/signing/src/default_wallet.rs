//! Native `DefaultWallet` transactions for pubkey accounts.
//!
//! Deployment atomically transfers an explicit amount, installs attached code,
//! and optionally initializes that code. Callers must select a runtime supporting
//! this deployment ABI; these helpers do not discover runtime capabilities.
//! Legacy deployed wallets have their own transaction format.

use thruput_xdr::contract::{Contract, ContractEntrypointID};
use thruput_xdr::transaction::Transaction;
use thruput_xdr::types::{ContractAddress, SHA256Hash};
use xdr_common::VecM;

use crate::{
    BuildCallTransaction, BuildDeployTransaction, PubkeyAccount, SigningError,
    build_call_transaction, build_deploy_transaction, wallet_contract_hash,
};

/// Canonical `DefaultWallet.execute` selector.
pub const DEFAULT_WALLET_EXECUTE_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new(0xc3ed_b11f_dd6d_5d58_u64.to_be_bytes());
/// Canonical `DefaultWallet.deploy` selector.
pub const DEFAULT_WALLET_DEPLOY_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new(0x6be0_cb0f_6de0_917a_u64.to_be_bytes());

/// Optional initialization of the newly deployed contract, called by the wallet.
/// A failed invocation rolls back funding and deployment; return bytes are discarded.
#[derive(Debug, Clone, Copy)]
pub struct WalletInitCall<'a> {
    /// Initializer method on the new contract.
    pub entrypoint: ContractEntrypointID,
    /// Gas budget for the initializer invocation.
    pub gas_limit: u64,
    /// Encoded initializer arguments.
    pub calldata: &'a [u8],
}

/// One child invocation wrapped in the native wallet's `execute` call.
#[derive(Debug, Clone, Copy)]
pub struct BuildDefaultWalletCallTransaction<'a> {
    /// Determines both the root address and its pubkey authentication claim.
    pub account: &'a PubkeyAccount,
    pub target_address: ContractAddress,
    pub target_entrypoint: ContractEntrypointID,
    pub target_calldata: &'a [u8],
    pub target_gas_limit: u64,
    pub gas_limit: u64,
    pub block_expiry_height: i64,
    /// Transaction uniquifier, independent of the pubkey account nonce.
    pub nonce: u64,
}

/// Build one native child invocation without submitting or signing it.
pub fn build_default_wallet_call_transaction(
    args: BuildDefaultWalletCallTransaction<'_>,
) -> Result<Transaction, SigningError> {
    let call = (
        args.target_address,
        args.target_entrypoint,
        args.target_gas_limit,
        args.target_calldata,
    );
    let calldata = xdrs::cxs::encode(&[call].as_slice())
        .map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    Ok(build_call_transaction(BuildCallTransaction {
        invoked_address: args.account.address(),
        entrypoint: DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
        calldata,
        gas_limit: args.gas_limit,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
        auth_claim: args.account.auth_claim(),
    }))
}

/// Code deployed either from this transaction or from an earlier registration.
#[derive(Debug, Clone)]
pub enum DeploymentCode {
    /// Register this binary and deploy it in the same transaction.
    Attached(Contract),
    /// Deploy a binary that is already registered on the network.
    Registered(SHA256Hash),
}

/// Native deployment with explicit code identity.
#[derive(Debug, Clone)]
pub struct BuildDefaultWalletDeployTransaction<'a> {
    pub account: &'a PubkeyAccount,
    /// Stable 32-byte deployment salt, separate from the transaction uniquifier.
    pub deployment_nonce: SHA256Hash,
    pub code: DeploymentCode,
    /// Exact raw native token amount to transfer. Zero skips the transfer.
    pub amount: u128,
    /// Initialization at the newly deployed address within the same root frame.
    pub initializer: Option<WalletInitCall<'a>>,
    pub gas_limit: u64,
    pub block_expiry_height: i64,
    pub nonce: u64,
}

/// Build an atomic deployment with optional funding and initialization.
/// Zero skips funding. The initializer targets only the newly deployed contract.
/// This performs no submission or capability discovery and has no multi-transaction fallback.
pub fn build_default_wallet_deploy_transaction(
    args: BuildDefaultWalletDeployTransaction<'_>,
) -> Result<Transaction, SigningError> {
    let (hash, contracts_to_deploy) = match args.code {
        DeploymentCode::Attached(contract) => {
            (wallet_contract_hash(&contract)?, VecM(vec![contract]))
        }
        DeploymentCode::Registered(hash) => (hash, VecM(vec![])),
    };
    let initializer = args
        .initializer
        .map(|call| (call.entrypoint, call.gas_limit, call.calldata));
    let calldata = xdrs::cxs::encode(&(args.deployment_nonce, hash, args.amount, initializer))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    Ok(build_deploy_transaction(BuildDeployTransaction {
        invoked_address: args.account.address(),
        entrypoint: DEFAULT_WALLET_DEPLOY_ENTRYPOINT,
        calldata,
        gas_limit: args.gas_limit,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
        contracts_to_deploy,
        auth_claim: args.account.auth_claim(),
    }))
}

const DEFAULT_WALLET_RETURN_DECODE_LIMIT: usize = 1024;

/// Decode a successful native `execute` result for a single child call.
/// Pass return data only after checking the root receipt's success status.
/// Accepts at most 1 KiB, including collection framing. This is a local decoding
/// limit; the chain's return-buffer limit is configured independently.
pub fn decode_default_wallet_call_return(data: &[u8]) -> Result<Vec<u8>, SigningError> {
    if data.len() > DEFAULT_WALLET_RETURN_DECODE_LIMIT {
        return Err(SigningError::XdrDecode(
            "native wallet return exceeds decoder buffer limit".into(),
        ));
    }
    let mut returns: Vec<Vec<u8>> =
        xdrs::cxs::decode(data).map_err(|error| SigningError::XdrDecode(error.to_string()))?;
    if returns.len() != 1 {
        return Err(SigningError::UnexpectedWalletReturnCount(returns.len()));
    }
    Ok(returns.remove(0))
}
