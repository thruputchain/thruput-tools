use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder, submit_and_poll};
use groundhog_json_rpc_types::hex::{Hex0x, HexBytes};
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress as ApiContractAddress, Ed25519PublicKey as ApiEd25519PublicKey,
    SHA256Hash as ApiSha256Hash,
};
use groundhog_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams,
    build_deploy_transaction, derive_deploy_address, sign_and_encode_transaction_hex,
    source_address_auth_claim, wallet_create_proof_message,
};
use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
use groundhog_xdr::types::{Ed25519PublicKey, SHA256Hash};
use serde::{Deserialize, Serialize};
use xdr_common::{BytesM, VecM};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PrepareWalletCreateRequest {
    owner_public_key: ApiEd25519PublicKey,
    deploy_nonce: ApiSha256Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateWalletRequest {
    owner_public_key: ApiEd25519PublicKey,
    deploy_nonce: ApiSha256Hash,
    owner_signature: HexBytes,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareWalletCreateResponse {
    owner_public_key: ApiEd25519PublicKey,
    deploy_nonce: ApiSha256Hash,
    wallet_address: ApiContractAddress,
    prefund_raw_amount: String,
    wallet_contract_hash: ApiSha256Hash,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateWalletResponse {
    owner_public_key: ApiEd25519PublicKey,
    deploy_nonce: ApiSha256Hash,
    wallet_address: ApiContractAddress,
    tx_hash: TxHash,
    height: BlockHeight,
    status: ExecStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: &'static str,
}

pub(crate) async fn prepare_wallet_create(
    State(submitter): State<Arc<dyn WalletCreateSubmitter>>,
    Json(request): Json<PrepareWalletCreateRequest>,
) -> Response {
    match submitter.prepare_wallet_create(request.owner_public_key.0, request.deploy_nonce.0) {
        Ok(prepared) => Json(PrepareWalletCreateResponse {
            owner_public_key: Hex0x(request.owner_public_key.0),
            deploy_nonce: Hex0x(request.deploy_nonce.0),
            wallet_address: prepared.wallet_address,
            prefund_raw_amount: prepared.prefund_raw_amount,
            wallet_contract_hash: prepared.wallet_contract_hash,
        })
        .into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn create_wallet(
    State(submitter): State<Arc<dyn WalletCreateSubmitter>>,
    Json(request): Json<CreateWalletRequest>,
) -> Response {
    match submitter
        .create_wallet(
            request.owner_public_key.0,
            request.deploy_nonce.0,
            request.owner_signature.0,
        )
        .await
    {
        Ok(submission) => Json(CreateWalletResponse {
            owner_public_key: Hex0x(request.owner_public_key.0),
            deploy_nonce: Hex0x(request.deploy_nonce.0),
            wallet_address: submission.wallet_address,
            tx_hash: submission.tx_hash,
            height: submission.receipt.height,
            status: submission.receipt.status,
        })
        .into_response(),
        Err(error) => error.into_response(),
    }
}

/// Prepares and submits hosted wallet creation transactions.
#[async_trait::async_trait]
pub trait WalletCreateSubmitter: Send + Sync + 'static {
    /// Derive the future wallet address for a wallet-create request.
    fn prepare_wallet_create(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
    ) -> Result<PreparedWalletCreate, WalletCreateError>;

    /// Submit a sponsored `create_wallet` transaction.
    async fn create_wallet(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
        owner_signature: Vec<u8>,
    ) -> Result<WalletCreateSubmission, WalletCreateError>;
}

/// Prepared wallet-create metadata returned before any transaction is submitted.
#[derive(Debug, Clone)]
pub struct PreparedWalletCreate {
    /// Future wallet address derived from faucet wallet, nonce, and contract hash.
    pub wallet_address: ApiContractAddress,
    /// Raw HOG units the future wallet address must hold before creation.
    pub prefund_raw_amount: String,
    /// Dev-wallet contract hash used for deterministic address derivation.
    pub wallet_contract_hash: ApiSha256Hash,
}

/// Successful faucet wallet-create submission.
#[derive(Debug, Clone)]
pub struct WalletCreateSubmission {
    /// Created wallet address.
    pub wallet_address: ApiContractAddress,
    /// Submitted transaction hash.
    pub tx_hash: TxHash,
    /// Committed transaction receipt.
    pub receipt: TransactionReceipt,
}

/// Errors produced while preparing or submitting hosted wallet creation.
#[derive(Debug, thiserror::Error)]
pub enum WalletCreateError {
    /// The owner proof signature was malformed.
    #[error("invalid owner proof signature")]
    InvalidOwnerSignature,
    /// The owner proof signature did not verify against the requested owner key.
    #[error("owner proof signature did not verify")]
    OwnerSignatureMismatch,
    /// The transaction committed but did not execute successfully.
    #[error("wallet-create transaction {tx_hash:?} did not commit successfully")]
    TransactionFailed { tx_hash: TxHash },
    /// The future wallet address could not be derived.
    #[error("failed to derive wallet address: {0}")]
    DeriveAddress(String),
    /// The chain tip could not be fetched for block-expiry calculation.
    #[error("failed to fetch chain tip: {0}")]
    ChainTip(String),
    /// The RPC client could not be constructed.
    #[error("failed to connect to RPC endpoint: {0}")]
    RpcConnect(String),
    /// The wallet-create calldata could not be encoded.
    #[error("failed to encode wallet-create calldata: {0}")]
    EncodeCreateCalldata(String),
    /// The wallet artifact could not be loaded.
    #[error("failed to load wallet artifact: {0}")]
    LoadWalletArtifact(String),
    /// The wallet-create transaction could not be built.
    #[error("failed to build wallet-create transaction: {0}")]
    BuildTransaction(String),
    /// The wallet-create transaction could not be signed.
    #[error("failed to sign wallet-create transaction: {0}")]
    SignTransaction(String),
    /// The signed wallet-create transaction could not be submitted or confirmed.
    #[error("failed to submit or confirm wallet-create transaction: {0}")]
    SubmitAndPoll(#[from] groundhog_json_rpc_client::SubmitAndPollError),
}

impl WalletCreateError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidOwnerSignature | Self::OwnerSignatureMismatch => error_response(
                StatusCode::UNAUTHORIZED,
                "wallet_create_owner_proof_invalid",
                "wallet-create owner proof is invalid",
            ),
            Self::TransactionFailed { .. } => error_response(
                StatusCode::BAD_GATEWAY,
                "wallet_create_transaction_failed",
                "wallet-create transaction did not commit successfully",
            ),
            Self::DeriveAddress(_)
            | Self::ChainTip(_)
            | Self::RpcConnect(_)
            | Self::EncodeCreateCalldata(_)
            | Self::LoadWalletArtifact(_)
            | Self::BuildTransaction(_)
            | Self::SignTransaction(_)
            | Self::SubmitAndPoll(_) => error_response(
                StatusCode::BAD_GATEWAY,
                "wallet_create_submit_failed",
                "failed to submit wallet-create transaction",
            ),
        }
    }
}

fn error_response(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail { code, message },
        }),
    )
        .into_response()
}

/// Runtime-only wallet-create execution config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletCreateRuntimeConfig {
    /// Faucet wallet `create_wallet` entrypoint.
    pub create_wallet_entrypoint: ContractEntrypointID,
    /// Dev-wallet artifact included with each create-wallet transaction.
    pub wallet_artifact_path: PathBuf,
}

/// Errors returned while validating runtime wallet-create config.
#[derive(Debug, thiserror::Error)]
pub enum WalletCreateRuntimeConfigError {
    /// The configured wallet `create_wallet` entrypoint was not 8 bytes.
    #[error("wallet-create entrypoint must be 8 bytes, got {0}")]
    InvalidCreateWalletEntrypoint(usize),
    /// The configured wallet artifact path was empty.
    #[error("wallet-create artifact path must not be empty")]
    EmptyArtifactPath,
}

impl WalletCreateRuntimeConfig {
    /// Validate wire-format config values into runtime wallet-create config.
    pub fn from_wire(
        create_wallet_entrypoint: HexBytes,
        wallet_artifact_path: PathBuf,
    ) -> Result<Self, WalletCreateRuntimeConfigError> {
        if wallet_artifact_path.as_os_str().is_empty() {
            return Err(WalletCreateRuntimeConfigError::EmptyArtifactPath);
        }
        let entrypoint_len = create_wallet_entrypoint.0.len();
        let create_wallet_entrypoint: [u8; 8] =
            create_wallet_entrypoint.0.try_into().map_err(|_| {
                WalletCreateRuntimeConfigError::InvalidCreateWalletEntrypoint(entrypoint_len)
            })?;
        Ok(Self {
            create_wallet_entrypoint: ContractEntrypointID::from(create_wallet_entrypoint),
            wallet_artifact_path,
        })
    }
}

pub struct RpcWalletCreateSubmitter {
    rpc_url: String,
    faucet_wallet_address: groundhog_xdr::types::ContractAddress,
    faucet_signer: LocalSigner,
    create_wallet_entrypoint: ContractEntrypointID,
    prefund_raw_amount: String,
    wallet_contract_hash: groundhog_xdr::types::SHA256Hash,
    wallet_artifact_path: PathBuf,
    genesis_hash: groundhog_xdr::types::SHA256Hash,
}

impl RpcWalletCreateSubmitter {
    /// Construct an RPC-backed wallet-create submitter from public descriptor and private config.
    pub fn new(
        descriptor: &NetworkDescriptor,
        config: WalletCreateRuntimeConfig,
        faucet_secret_key: [u8; 32],
    ) -> Self {
        Self {
            rpc_url: descriptor.endpoints.rpc_url.clone(),
            faucet_wallet_address: descriptor.faucet.wallet_address.0,
            faucet_signer: LocalSigner::from_secret_key(faucet_secret_key),
            create_wallet_entrypoint: config.create_wallet_entrypoint,
            prefund_raw_amount: descriptor.wallet_create.prefund_raw_amount.clone(),
            wallet_contract_hash: descriptor.wallet_create.wallet_contract_hash.0,
            wallet_artifact_path: config.wallet_artifact_path,
            genesis_hash: descriptor.genesis_hash.0,
        }
    }

    fn proof_message(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
    ) -> Result<Vec<u8>, WalletCreateError> {
        wallet_create_proof_message(
            self.genesis_hash,
            self.faucet_wallet_address,
            self.wallet_contract_hash,
            owner_public_key,
            deploy_nonce,
        )
        .map_err(|error| WalletCreateError::EncodeCreateCalldata(error.to_string()))
    }

    async fn block_expiry_height(&self) -> Result<i64, WalletCreateError> {
        let client = HttpClientBuilder::default()
            .build(&self.rpc_url)
            .map_err(|error| WalletCreateError::RpcConnect(error.to_string()))?;
        let tip = client
            .chain_tip()
            .await
            .map_err(|error| WalletCreateError::ChainTip(error.to_string()))?;
        Ok((*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET))
    }
}

#[async_trait::async_trait]
impl WalletCreateSubmitter for RpcWalletCreateSubmitter {
    fn prepare_wallet_create(
        &self,
        _owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
    ) -> Result<PreparedWalletCreate, WalletCreateError> {
        let wallet_address = derive_deploy_address(
            &self.faucet_wallet_address,
            &deploy_nonce,
            &self.wallet_contract_hash,
        )
        .map_err(|error| WalletCreateError::DeriveAddress(error.to_string()))?;
        Ok(PreparedWalletCreate {
            wallet_address: Hex0x(wallet_address),
            prefund_raw_amount: self.prefund_raw_amount.clone(),
            wallet_contract_hash: Hex0x(self.wallet_contract_hash),
        })
    }

    async fn create_wallet(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
        owner_signature: Vec<u8>,
    ) -> Result<WalletCreateSubmission, WalletCreateError> {
        let prepared = self.prepare_wallet_create(owner_public_key, deploy_nonce)?;
        let message = self.proof_message(owner_public_key, deploy_nonce)?;
        verify_owner_signature(owner_public_key, &message, &owner_signature)?;

        let artifact_bytes = tokio::fs::read(&self.wallet_artifact_path)
            .await
            .map_err(|error| WalletCreateError::LoadWalletArtifact(error.to_string()))?;
        let contract = wallet_contract(&self.wallet_artifact_path, artifact_bytes);
        let block_expiry_height = self.block_expiry_height().await?;
        let create_calldata =
            create_wallet_calldata(&deploy_nonce, &self.wallet_contract_hash, owner_public_key)?;
        let tx = build_deploy_transaction(BuildDeployTransaction {
            invoked_address: self.faucet_wallet_address,
            entrypoint: self.create_wallet_entrypoint,
            calldata: create_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce: nonce_from_timestamp(),
            contracts_to_deploy: VecM(vec![contract]),
            auth_claim: source_address_auth_claim(),
        });
        let tx_hex = sign_and_encode_transaction_hex(
            tx,
            &self.faucet_signer,
            SignTransactionParams::default(),
        )
        .map_err(|error| WalletCreateError::SignTransaction(error.to_string()))?;
        let (tx_hash, receipt) = submit_and_poll(&self.rpc_url, &tx_hex).await?;
        if !matches!(receipt.status, ExecStatus::Ok) {
            return Err(WalletCreateError::TransactionFailed { tx_hash });
        }
        Ok(WalletCreateSubmission {
            wallet_address: prepared.wallet_address,
            tx_hash,
            receipt,
        })
    }
}

fn verify_owner_signature(
    owner_public_key: Ed25519PublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), WalletCreateError> {
    let public_key_bytes: [u8; 32] = owner_public_key.into_inner();
    let signature_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| WalletCreateError::InvalidOwnerSignature)?;
    let verifying_key = VerifyingKey::from_bytes(&public_key_bytes)
        .map_err(|_| WalletCreateError::InvalidOwnerSignature)?;
    verifying_key
        .verify(message, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| WalletCreateError::OwnerSignatureMismatch)
}

fn create_wallet_calldata(
    deploy_nonce: &groundhog_xdr::types::SHA256Hash,
    contract_hash: &groundhog_xdr::types::SHA256Hash,
    owner: Ed25519PublicKey,
) -> Result<Vec<u8>, WalletCreateError> {
    xdrs::cxs::encode(&(deploy_nonce, contract_hash, &owner))
        .map_err(|error| WalletCreateError::EncodeCreateCalldata(error.to_string()))
}

fn wallet_contract(artifact_path: &std::path::Path, artifact_bytes: Vec<u8>) -> Contract {
    let blob = ContractBlob {
        data: BytesM(artifact_bytes),
    };
    match artifact_path
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rv32a") => Contract::kRv32 {
            rv32_contract: blob,
        },
        _ => Contract::kWasmi {
            wasmi_contract: blob,
        },
    }
}

fn nonce_from_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use groundhog_signing::{LocalSigner, Signer};

    use super::*;

    #[test]
    fn owner_proof_verifies_for_matching_key_and_message() {
        let signer = LocalSigner::from_secret_key([0x11; 32]);
        let owner = Ed25519PublicKey::from(signer.verifying_key_bytes());
        let message = wallet_create_proof_message(
            groundhog_xdr::types::SHA256Hash::from([0x22; 32]),
            groundhog_xdr::types::ContractAddress::from([0x33; 32]),
            groundhog_xdr::types::SHA256Hash::from([0x44; 32]),
            owner,
            groundhog_xdr::types::SHA256Hash::from([0x55; 32]),
        )
        .expect("bug: proof message should encode");
        let signature = owner_signature_bytes(&signer, &message);

        verify_owner_signature(owner, &message, &signature)
            .expect("bug: matching owner proof should verify");
    }

    #[test]
    fn owner_proof_rejects_wrong_message() {
        let signer = LocalSigner::from_secret_key([0x11; 32]);
        let owner = Ed25519PublicKey::from(signer.verifying_key_bytes());
        let message = wallet_create_proof_message(
            groundhog_xdr::types::SHA256Hash::from([0x22; 32]),
            groundhog_xdr::types::ContractAddress::from([0x33; 32]),
            groundhog_xdr::types::SHA256Hash::from([0x44; 32]),
            owner,
            groundhog_xdr::types::SHA256Hash::from([0x55; 32]),
        )
        .expect("bug: proof message should encode");
        let wrong_message = wallet_create_proof_message(
            groundhog_xdr::types::SHA256Hash::from([0x99; 32]),
            groundhog_xdr::types::ContractAddress::from([0x33; 32]),
            groundhog_xdr::types::SHA256Hash::from([0x44; 32]),
            owner,
            groundhog_xdr::types::SHA256Hash::from([0x55; 32]),
        )
        .expect("bug: proof message should encode");
        let signature = owner_signature_bytes(&signer, &message);

        let error = verify_owner_signature(owner, &wrong_message, &signature)
            .expect_err("bug: wrong owner proof message should fail");
        assert!(matches!(error, WalletCreateError::OwnerSignatureMismatch));
    }

    fn owner_signature_bytes(signer: &LocalSigner, message: &[u8]) -> [u8; 64] {
        match signer
            .sign(message)
            .expect("bug: proof message should sign")
        {
            groundhog_xdr::types::Signature::Ed25519 { ed25519 } => ed25519.into_inner(),
            _ => panic!("bug: local signer should produce Ed25519 signatures"),
        }
    }
}
