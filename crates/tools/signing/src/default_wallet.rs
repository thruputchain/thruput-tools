//! Native `DefaultWallet` transactions for pubkey accounts.
//!
//! Selectors and argument layouts follow `DefaultWallet` 0.1.0 in
//! `integration_test_contracts` at e1b139fb8461e848cb1666b16c5ad0d9f31c45ba.
//! Funded deployment is an additive backend prerequisite that must be released
//! before callers select it for a published runtime.
//! These helpers do not reinterpret deployed legacy wallet addresses.

use groundhog_xdr::contract::{Contract, ContractEntrypointID};
use groundhog_xdr::transaction::Transaction;
use groundhog_xdr::types::{ContractAddress, SHA256Hash};
use xdr_common::VecM;

use crate::{
    BuildCallTransaction, BuildDeployTransaction, PubkeyAccount, SigningError,
    build_call_transaction, build_deploy_transaction, wallet_contract_hash,
};

/// Canonical `DefaultWallet.execute` selector.
pub const DEFAULT_WALLET_EXECUTE_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new(0xc3ed_b11f_dd6d_5d58_u64.to_be_bytes());
/// Canonical `DefaultWallet.deploy` selector (does not fund target storage).
pub const DEFAULT_WALLET_DEPLOY_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new(0x6be0_cb0f_6de0_917a_u64.to_be_bytes());
/// Additive `DefaultWallet.deploy_funded` selector. Requires a runtime that
/// implements atomic funded deployment; older runtimes do not support it.
pub const DEFAULT_WALLET_DEPLOY_FUNDED_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new(0x1f84_3596_f5b5_7b18_u64.to_be_bytes());

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

/// Native deployment with its code attached to the transaction.
#[derive(Debug, Clone)]
pub struct BuildDefaultWalletDeployTransaction<'a> {
    pub account: &'a PubkeyAccount,
    /// Stable 32-byte deployment salt, separate from the transaction uniquifier.
    pub deployment_nonce: SHA256Hash,
    pub contract: Contract,
    pub gas_limit: u64,
    pub block_expiry_height: i64,
    pub nonce: u64,
}

/// Build an unfunded deployment. Callers must arrange sufficient collateral at
/// the predicted target first; this helper never submits a funding transfer.
pub fn build_default_wallet_deploy_transaction(
    args: BuildDefaultWalletDeployTransaction<'_>,
) -> Result<Transaction, SigningError> {
    let hash = wallet_contract_hash(&args.contract)?;
    let calldata = xdrs::cxs::encode(&(args.deployment_nonce, hash))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    Ok(build_native_deploy(
        args,
        DEFAULT_WALLET_DEPLOY_ENTRYPOINT,
        calldata,
    ))
}

/// Build an atomic funding transfer and deployment with an exact raw HOG amount.
/// This performs no submission or capability discovery. Callers must select a
/// supported runtime and authorize the amount. Never retry this as two separate
/// transactions when the runtime does not support the operation.
pub fn build_default_wallet_funded_deploy_transaction(
    args: BuildDefaultWalletDeployTransaction<'_>,
    funding_raw_amount: u128,
) -> Result<Transaction, SigningError> {
    let hash = wallet_contract_hash(&args.contract)?;
    let calldata = xdrs::cxs::encode(&(args.deployment_nonce, hash, funding_raw_amount))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    Ok(build_native_deploy(
        args,
        DEFAULT_WALLET_DEPLOY_FUNDED_ENTRYPOINT,
        calldata,
    ))
}

fn build_native_deploy(
    args: BuildDefaultWalletDeployTransaction<'_>,
    entrypoint: ContractEntrypointID,
    calldata: Vec<u8>,
) -> Transaction {
    build_deploy_transaction(BuildDeployTransaction {
        invoked_address: args.account.address(),
        entrypoint,
        calldata,
        gas_limit: args.gas_limit,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
        contracts_to_deploy: VecM(vec![args.contract]),
        auth_claim: args.account.auth_claim(),
    })
}

/// Decode a successful native `execute` result for a single child call.
/// Pass return data only after checking the root receipt's success status.
pub fn decode_default_wallet_call_return(data: &[u8]) -> Result<Vec<u8>, SigningError> {
    let mut returns: Vec<Vec<u8>> =
        xdrs::cxs::decode(data).map_err(|error| SigningError::XdrDecode(error.to_string()))?;
    if returns.len() != 1 {
        return Err(SigningError::UnexpectedWalletReturnCount(returns.len()));
    }
    Ok(returns.remove(0))
}
