//! Transaction and signed-transaction builders.

use crate::{AuthClaimIndex, PublicKeyId, Signer, SigningError};
use groundhog_xdr::auth_claim::AuthClaimData;
use groundhog_xdr::contract::{ContractDeploymentHashPreimage, ContractEntrypointID};
use groundhog_xdr::transaction::{
    DecoratedSignature, Invocation, SignedTransaction, Transaction, Uniquifier,
};
use groundhog_xdr::types::{BlockHeight, ContractAddress, SHA256Hash};
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

/// Wallet `deploy` entrypoint ID (from dev-wallet WASM exports).
pub const WALLET_DEPLOY_ENTRYPOINT: [u8; 8] = 0xADAC_4982_2214_8E4F_u64.to_be_bytes();

/// Default gas limit used by the CLI/dev-node wallet invocation flow.
pub const DEFAULT_TX_GAS_LIMIT: u64 = 10_000_000;

/// Parameters controlling how a signature is annotated in a signed
/// transaction.
#[derive(Clone, Copy, Debug, Default)]
pub struct SignTransactionParams {
    /// Auth-claim index associated with the produced signature.
    pub auth_claim_idx: AuthClaimIndex,
    /// Public-key identifier associated with the produced signature.
    pub public_key_id: PublicKeyId,
}

/// Inputs for constructing a contract invocation transaction.
#[derive(Clone, Debug)]
pub struct BuildCallTransaction {
    /// Address being invoked.
    pub invoked_address: ContractAddress,
    /// Entry point to invoke.
    pub entrypoint: ContractEntrypointID,
    /// Encoded calldata bytes.
    pub calldata: Vec<u8>,
    /// Transaction gas limit.
    pub gas_limit: u64,
    /// Transaction expiry height.
    pub block_expiry_height: i64,
    /// Transaction nonce.
    pub nonce: u64,
    /// Auth claims attached to the transaction.
    pub auth_claims: AuthClaimData,
}

/// Inputs for constructing a deployment transaction.
#[derive(Clone, Debug)]
pub struct BuildDeployTransaction {
    /// Address being invoked.
    pub invoked_address: ContractAddress,
    /// Entry point to invoke.
    pub entrypoint: ContractEntrypointID,
    /// Encoded calldata bytes.
    pub calldata: Vec<u8>,
    /// Transaction gas limit.
    pub gas_limit: u64,
    /// Transaction expiry height.
    pub block_expiry_height: i64,
    /// Transaction nonce.
    pub nonce: u64,
    /// Contracts to deploy inline with the transaction.
    pub contracts_to_deploy: VecM<groundhog_xdr::contract::Contract>,
    /// Auth claims attached to the transaction.
    pub auth_claims: AuthClaimData,
}

/// Builds a call transaction with no contracts-to-deploy or gas claims.
pub fn build_call_transaction(args: BuildCallTransaction) -> Transaction {
    let invocation = Invocation {
        invoked_address: args.invoked_address,
        entrypoint: args.entrypoint,
        gas_limit: args.gas_limit,
        calldata: BytesM(args.calldata),
    };

    let uniquifier = Uniquifier {
        block_expiry_height: BlockHeight::from(args.block_expiry_height),
        nonce: args.nonce,
    };

    Transaction {
        invocation,
        uniquifier,
        contracts_to_deploy: VecM(vec![]),
        auth_claims: args.auth_claims,
        gas_claims: VecM(vec![]),
    }
}

/// Builds a deployment transaction.
pub fn build_deploy_transaction(args: BuildDeployTransaction) -> Transaction {
    let invocation = Invocation {
        invoked_address: args.invoked_address,
        entrypoint: args.entrypoint,
        gas_limit: args.gas_limit,
        calldata: BytesM(args.calldata),
    };

    let uniquifier = Uniquifier {
        block_expiry_height: BlockHeight::from(args.block_expiry_height),
        nonce: args.nonce,
    };

    Transaction {
        invocation,
        uniquifier,
        contracts_to_deploy: args.contracts_to_deploy,
        auth_claims: args.auth_claims,
        gas_claims: VecM(vec![]),
    }
}

/// Compute the deployed contract address from the canonical deployment preimage.
pub fn derive_deploy_address(
    deployer_address: &ContractAddress,
    deploy_nonce: &SHA256Hash,
    contract_hash: &SHA256Hash,
) -> Result<ContractAddress, SigningError> {
    let preimage = ContractDeploymentHashPreimage {
        deployer_address: *deployer_address,
        nonce: *deploy_nonce,
        contract_hash: *contract_hash,
    };
    let preimage =
        xdrs::txs::encode(&preimage).map_err(|e| SigningError::XdrEncode(e.to_string()))?;
    let addr: [u8; 32] = Sha256::digest(&preimage).into();
    Ok(ContractAddress::from(addr))
}

/// Signs a transaction and returns the canonical signed transaction.
pub fn sign_transaction(
    tx: Transaction,
    signer: &impl Signer,
    params: SignTransactionParams,
) -> Result<SignedTransaction, SigningError> {
    let tx_bytes = crate::encode_transaction_xdr(&tx)?;
    let signature = signer.sign(&tx_bytes)?;
    let decorated_signature = DecoratedSignature {
        auth_claim_idx: params.auth_claim_idx.0,
        public_key_id: params.public_key_id.0,
        signature,
    };

    Ok(SignedTransaction {
        tx,
        signatures: VecM(vec![decorated_signature]),
    })
}
