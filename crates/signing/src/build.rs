//! Transaction and signed-transaction builders.

use crate::{PublicKeyId, Signer, SigningError};
use sha2::{Digest, Sha256};
use thruput_domain_separator::transaction_signature_preimage;
use thruput_xdr::auth_claim::{AuthClaim, AuthClaim_source_address_claim};
use thruput_xdr::contract::{ContractDeploymentHashPreimage, ContractEntrypointID};
use thruput_xdr::transaction::{
    DecoratedSignature, Invocation, SignedTransaction, Transaction, Uniquifier,
};
use thruput_xdr::types::{BlockHeight, ContractAddress, NetworkHash, SHA256Hash};
use xdr_common::{BytesM, VecM};

/// Frozen legacy wallet `deploy` entrypoint for RPC-only profiles without
/// runtime manifest or network descriptor metadata.
///
/// New integrations should use the selector advertised by their target.
pub const WALLET_DEPLOY_ENTRYPOINT: [u8; 8] = 0xADAC_4982_2214_8E4F_u64.to_be_bytes();

/// Default gas limit used by the CLI local-node wallet invocation flow.
pub const DEFAULT_TX_GAS_LIMIT: u64 = 10_000_000;

/// Parameters controlling how a signature is annotated in a signed
/// transaction.
#[derive(Clone, Copy, Debug, Default)]
pub struct SignTransactionParams {
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
    /// Auth claim attached to the transaction.
    pub auth_claim: AuthClaim,
}

/// Inputs for constructing a transaction through a dev-wallet `doAction`
/// wrapper.
#[derive(Clone, Copy, Debug)]
pub struct BuildDevWalletDoActionTransaction<'a> {
    /// Wallet contract address to invoke.
    pub wallet_address: ContractAddress,
    /// Wallet `doAction` entrypoint.
    pub wallet_entrypoint: ContractEntrypointID,
    /// Inner target contract address.
    pub target_address: ContractAddress,
    /// Inner target entrypoint.
    pub target_entrypoint: ContractEntrypointID,
    /// Inner target calldata bytes.
    pub target_calldata: &'a [u8],
    /// Gas limit used for the outer wallet call.
    pub gas_limit: u64,
    /// Gas limit encoded into the inner invocation.
    pub target_gas_limit: u64,
    /// Transaction expiry height.
    pub block_expiry_height: i64,
    /// Transaction nonce.
    pub nonce: u64,
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
    pub contracts_to_deploy: VecM<thruput_xdr::contract::Contract>,
    /// Auth claim attached to the transaction.
    pub auth_claim: AuthClaim,
}

/// Returns the standard single-key source-address auth claim used by the dev
/// wallet flow.
pub fn source_address_auth_claim() -> AuthClaim {
    AuthClaim::kSourceAddressAuthData {
        source_address_claim: AuthClaim_source_address_claim {
            allowed_keys: 0x01,
            required_weight: 1,
        },
    }
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
        auth_claim: args.auth_claim,
    }
}

/// Encodes the calldata accepted by the dev-wallet `doAction` entrypoint.
pub fn dev_wallet_do_action_calldata(
    target_address: ContractAddress,
    target_entrypoint: ContractEntrypointID,
    target_gas_limit: u64,
    target_calldata: &[u8],
) -> Result<Vec<u8>, SigningError> {
    let inner_invocation = xdrs::txs::encode(&(
        target_address,
        target_entrypoint,
        target_gas_limit,
        target_calldata,
    ))
    .map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    xdrs::cxs::encode(&inner_invocation.as_slice())
        .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Builds a source-address-authenticated call through the dev-wallet
/// `doAction` entrypoint.
pub fn build_dev_wallet_do_action_transaction(
    args: BuildDevWalletDoActionTransaction<'_>,
) -> Result<Transaction, SigningError> {
    let calldata = dev_wallet_do_action_calldata(
        args.target_address,
        args.target_entrypoint,
        args.target_gas_limit,
        args.target_calldata,
    )?;
    Ok(build_call_transaction(BuildCallTransaction {
        invoked_address: args.wallet_address,
        entrypoint: args.wallet_entrypoint,
        calldata,
        gas_limit: args.gas_limit,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
        auth_claim: source_address_auth_claim(),
    }))
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
        auth_claim: args.auth_claim,
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

/// Message signed by a wallet owner to authorize hosted faucet wallet creation.
pub fn wallet_create_proof_message(
    genesis_hash: SHA256Hash,
    faucet_wallet_address: ContractAddress,
    wallet_contract_hash: SHA256Hash,
    owner_public_key: thruput_xdr::types::Ed25519PublicKey,
    deploy_nonce: SHA256Hash,
) -> Result<Vec<u8>, SigningError> {
    const DOMAIN: &str = "groundhog-faucet-create-wallet-v1";
    xdrs::cxs::encode(&(
        DOMAIN,
        genesis_hash,
        faucet_wallet_address,
        wallet_contract_hash,
        owner_public_key,
        deploy_nonce,
    ))
    .map(|bytes| Sha256::digest(bytes).to_vec())
    .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Parameters for the hardened hosted faucet wallet-create owner proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletCreateProofV2<'a> {
    pub network_id: &'a str,
    pub genesis_hash: SHA256Hash,
    pub faucet_wallet_address: ContractAddress,
    pub wallet_contract_hash: SHA256Hash,
    pub owner_public_key: thruput_xdr::types::Ed25519PublicKey,
    pub deploy_nonce: SHA256Hash,
    pub wallet_address: ContractAddress,
    pub client_nonce: &'a [u8],
}

/// Message signed by a wallet owner to authorize hardened hosted faucet wallet creation.
pub fn wallet_create_proof_message_v2(
    proof: &WalletCreateProofV2<'_>,
) -> Result<Vec<u8>, SigningError> {
    const DOMAIN: &str = "groundhog-faucet-create-wallet-v2";
    xdrs::cxs::encode(&(
        DOMAIN,
        proof.network_id,
        proof.genesis_hash,
        proof.faucet_wallet_address,
        proof.wallet_contract_hash,
        proof.owner_public_key,
        proof.deploy_nonce,
        proof.wallet_address,
        BytesM::<{ u32::MAX }>(proof.client_nonce.to_vec()),
    ))
    .map(|bytes| Sha256::digest(bytes).to_vec())
    .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Signs a transaction and returns the canonical signed transaction.
///
/// `network_hash` identifies the network the transaction is for. It is part of
/// what gets signed, so a transaction signed for one chain is rejected by
/// another — see
/// [`thruput_domain_separator::transaction_signature_preimage`].
pub fn sign_transaction(
    tx: Transaction,
    signer: &impl Signer,
    params: SignTransactionParams,
    network_hash: &NetworkHash,
) -> Result<SignedTransaction, SigningError> {
    let preimage = transaction_signature_preimage(network_hash, &tx)
        .map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    let signature = signer.sign(&preimage)?;
    let decorated_signature = DecoratedSignature {
        public_key_id: params.public_key_id.0,
        signature,
    };

    Ok(SignedTransaction {
        tx,
        signatures: VecM(vec![decorated_signature]),
    })
}

/// Sign a transaction and return its 0x-prefixed signed-transaction XDR bytes.
///
/// See [`sign_transaction`] for what `network_hash` binds.
pub fn sign_and_encode_transaction_hex(
    tx: Transaction,
    signer: &impl Signer,
    params: SignTransactionParams,
    network_hash: &NetworkHash,
) -> Result<String, SigningError> {
    let signed_tx = sign_transaction(tx, signer, params, network_hash)?;
    let tx_xdr = crate::encode_signed_transaction_xdr(&signed_tx)?;
    Ok(format!("0x{}", hex::encode(tx_xdr)))
}
