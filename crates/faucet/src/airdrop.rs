use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder, submit_and_poll};
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::utils::nonce_from_timestamp;

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct AirdropRequest {
    recipient: ContractAddress,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AirdropResponse {
    recipient: ContractAddress,
    raw_amount: String,
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

pub(crate) async fn request_airdrop(
    State(submitter): State<Arc<dyn AirdropSubmitter>>,
    Json(request): Json<AirdropRequest>,
) -> Response {
    match submitter.submit_airdrop(request.recipient).await {
        Ok(submission) => Json(AirdropResponse {
            recipient: request.recipient,
            raw_amount: submission.raw_amount,
            tx_hash: submission.tx_hash,
            height: submission.receipt.height,
            status: submission.receipt.status,
        })
        .into_response(),
        Err(error) => error.into_response(),
    }
}

/// Submits faucet airdrops for validated recipient wallet addresses.
#[async_trait::async_trait]
pub trait AirdropSubmitter: Send + Sync + 'static {
    /// Submit an airdrop to `recipient` and return the committed transaction receipt.
    async fn submit_airdrop(
        &self,
        recipient: ContractAddress,
    ) -> Result<AirdropSubmission, AirdropSubmitError>;
}

/// Successful faucet airdrop submission.
#[derive(Debug, Clone)]
pub struct AirdropSubmission {
    /// Raw HOG units transferred to the recipient.
    pub raw_amount: String,
    /// Submitted transaction hash.
    pub tx_hash: TxHash,
    /// Committed transaction receipt.
    pub receipt: TransactionReceipt,
}

/// Errors produced while submitting a faucet airdrop.
#[derive(Debug, thiserror::Error)]
pub enum AirdropSubmitError {
    /// The transaction committed but did not execute successfully.
    #[error("airdrop transaction {tx_hash:?} did not commit successfully")]
    TransactionFailed { tx_hash: TxHash },
    /// The RPC client could not be constructed.
    #[error("failed to connect to RPC endpoint: {0}")]
    RpcConnect(String),
    /// The chain tip could not be fetched for block-expiry calculation.
    #[error("failed to fetch chain tip: {0}")]
    ChainTip(String),
    /// The HOG transfer calldata could not be encoded.
    #[error("failed to encode HOG transfer calldata: {0}")]
    EncodeTransferCalldata(String),
    /// The faucet transaction could not be built.
    #[error("failed to build airdrop transaction: {0}")]
    BuildTransaction(String),
    /// The faucet transaction could not be signed.
    #[error("failed to sign airdrop transaction: {0}")]
    SignTransaction(String),
    /// The signed faucet transaction could not be submitted or confirmed.
    #[error("failed to submit or confirm airdrop transaction: {0}")]
    SubmitAndPoll(#[from] groundhog_json_rpc_client::SubmitAndPollError),
}

impl AirdropSubmitError {
    fn into_response(self) -> Response {
        match self {
            Self::TransactionFailed { .. } => error_response(
                StatusCode::BAD_GATEWAY,
                "airdrop_transaction_failed",
                "airdrop transaction did not commit successfully",
            ),
            Self::RpcConnect(_)
            | Self::ChainTip(_)
            | Self::EncodeTransferCalldata(_)
            | Self::BuildTransaction(_)
            | Self::SignTransaction(_)
            | Self::SubmitAndPoll(_) => error_response(
                StatusCode::BAD_GATEWAY,
                "airdrop_submit_failed",
                "failed to submit airdrop transaction",
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

/// Runtime-only airdrop execution config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirdropRuntimeConfig {
    /// Raw HOG units transferred for each airdrop request.
    pub raw_amount: u128,
    /// Faucet wallet `doAction` entrypoint.
    pub wallet_do_action_entrypoint: ContractEntrypointID,
    /// 32-byte faucet signing secret key.
    pub faucet_secret_key: Zeroizing<[u8; 32]>,
}

/// Errors returned while validating runtime airdrop config.
#[derive(Debug, thiserror::Error)]
pub enum AirdropRuntimeConfigError {
    /// The configured raw airdrop amount was missing, invalid, or zero.
    #[error("airdrop raw amount must be greater than zero")]
    EmptyAmount,
    /// The configured wallet `doAction` entrypoint was not 8 bytes.
    #[error("airdrop wallet doAction entrypoint must be 8 bytes, got {0}")]
    InvalidDoActionEntrypoint(usize),
    /// The configured faucet secret key was not 32 bytes.
    #[error("airdrop faucet secret key must be 32 bytes, got {0}")]
    InvalidSecretKey(usize),
}

impl AirdropRuntimeConfig {
    /// Validate wire-format config values into runtime airdrop config.
    pub fn from_wire(
        raw_amount: &str,
        wallet_do_action_entrypoint: HexBytes,
        faucet_secret_key: HexBytes,
    ) -> Result<Self, AirdropRuntimeConfigError> {
        let raw_amount = raw_amount
            .parse::<u128>()
            .ok()
            .filter(|amount| *amount > 0)
            .ok_or(AirdropRuntimeConfigError::EmptyAmount)?;
        let entrypoint_len = wallet_do_action_entrypoint.0.len();
        let wallet_do_action_entrypoint: [u8; 8] = wallet_do_action_entrypoint
            .0
            .try_into()
            .map_err(|_| AirdropRuntimeConfigError::InvalidDoActionEntrypoint(entrypoint_len))?;
        let secret_key_len = faucet_secret_key.0.len();
        let faucet_secret_key: [u8; 32] = faucet_secret_key
            .0
            .try_into()
            .map_err(|_| AirdropRuntimeConfigError::InvalidSecretKey(secret_key_len))?;
        Ok(Self {
            raw_amount,
            wallet_do_action_entrypoint: ContractEntrypointID::from(wallet_do_action_entrypoint),
            faucet_secret_key: Zeroizing::new(faucet_secret_key),
        })
    }
}

pub struct RpcAirdropSubmitter {
    rpc_url: String,
    raw_amount: u128,
    faucet_wallet_address: groundhog_xdr::types::ContractAddress,
    faucet_signer: LocalSigner,
    wallet_do_action_entrypoint: ContractEntrypointID,
    hog_token_address: groundhog_xdr::types::ContractAddress,
    hog_transfer_entrypoint: ContractEntrypointID,
}

impl RpcAirdropSubmitter {
    /// Construct an RPC-backed airdrop submitter from public descriptor and private config.
    pub fn new(
        descriptor: &NetworkDescriptor,
        config: &AirdropRuntimeConfig,
    ) -> Result<Self, AirdropRuntimeConfigError> {
        Ok(Self {
            rpc_url: descriptor.endpoints.rpc_url.clone(),
            raw_amount: config.raw_amount,
            faucet_wallet_address: descriptor.faucet.wallet_address.0,
            faucet_signer: LocalSigner::from_secret_key(*config.faucet_secret_key),
            wallet_do_action_entrypoint: config.wallet_do_action_entrypoint,
            hog_token_address: descriptor.native_hog.token_address.0,
            hog_transfer_entrypoint: ContractEntrypointID::from(entrypoint_bytes(
                &descriptor.native_hog.transfer_entrypoint,
            )?),
        })
    }

    async fn block_expiry_height(&self) -> Result<i64, AirdropSubmitError> {
        let client = HttpClientBuilder::default()
            .build(&self.rpc_url)
            .map_err(|error| AirdropSubmitError::RpcConnect(error.to_string()))?;
        let tip = client
            .chain_tip()
            .await
            .map_err(|error| AirdropSubmitError::ChainTip(error.to_string()))?;
        Ok((*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET))
    }
}

#[async_trait::async_trait]
impl AirdropSubmitter for RpcAirdropSubmitter {
    async fn submit_airdrop(
        &self,
        recipient: ContractAddress,
    ) -> Result<AirdropSubmission, AirdropSubmitError> {
        let block_expiry_height = self.block_expiry_height().await?;
        let transfer_calldata = encode_transfer_calldata(&recipient.0, self.raw_amount)?;
        let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
            wallet_address: self.faucet_wallet_address,
            wallet_entrypoint: self.wallet_do_action_entrypoint,
            target_address: self.hog_token_address,
            target_entrypoint: self.hog_transfer_entrypoint,
            target_calldata: transfer_calldata.as_slice(),
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce: nonce_from_timestamp(),
        })
        .map_err(|error| AirdropSubmitError::BuildTransaction(error.to_string()))?;
        let tx_hex = sign_and_encode_transaction_hex(
            tx,
            &self.faucet_signer,
            SignTransactionParams::default(),
        )
        .map_err(|error| AirdropSubmitError::SignTransaction(error.to_string()))?;
        let (tx_hash, receipt) = submit_and_poll(&self.rpc_url, &tx_hex).await?;
        if !matches!(receipt.status, ExecStatus::Ok) {
            return Err(AirdropSubmitError::TransactionFailed { tx_hash });
        }
        Ok(AirdropSubmission {
            raw_amount: self.raw_amount.to_string(),
            tx_hash,
            receipt,
        })
    }
}

fn entrypoint_bytes(entrypoint: &HexBytes) -> Result<[u8; 8], AirdropRuntimeConfigError> {
    let len = entrypoint.0.len();
    entrypoint
        .0
        .clone()
        .try_into()
        .map_err(|_| AirdropRuntimeConfigError::InvalidDoActionEntrypoint(len))
}

fn encode_transfer_calldata(
    recipient: &groundhog_xdr::types::ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, AirdropSubmitError> {
    xdrs::cxs::encode(&(recipient, raw_amount))
        .map_err(|error| AirdropSubmitError::EncodeTransferCalldata(error.to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::AirdropRequest;

    #[test]
    fn airdrop_request_accepts_future_optional_fields() {
        let request: AirdropRequest = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "future-client-nonce",
            "networkId": "future-network",
            "genesisHash": format!("0x{}", "22".repeat(32)),
        }))
        .expect("bug: public API requests should ignore unknown future fields");

        assert_eq!(request.recipient.0.as_ref(), &[0x11; 32]);
    }
}
