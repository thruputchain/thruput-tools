use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use groundhog_json_rpc_client::{
    GroundhogApiClient, HttpClientBuilder, poll_transaction_receipt, submit_transaction,
};
use groundhog_json_rpc_types::airdrop::{AirdropAttemptResponse, HostedAirdropRequest};
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::{ContractAddress, SHA256Hash};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

use crate::{
    config::{MIN_AIRDROP_TERMINAL_RETENTION_SECS, PolicyRuntimeConfig},
    error::{error_response, error_response_with_retry_after},
    policy::{
        AdmitAirdropAttemptResult, AirdropAttempt, AirdropAttemptKey, AirdropAttemptStatus,
        ClientIdentity, FaucetPolicyStore, PolicyStoreError, QuotaCheck, QuotaKey, client_identity,
    },
    utils::nonce_from_timestamp,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const AIRDROP_SUBMIT_AMOUNT_MISMATCH: &str = "airdrop_submit_amount_mismatch";
const MAX_RETAINED_AIRDROP_ATTEMPTS: usize = 10_000;
const MAX_CONCURRENT_AIRDROP_EXECUTIONS: usize = 64;
#[derive(Clone)]
pub(crate) struct AirdropService {
    network_descriptor: NetworkDescriptor,
    submitter: Arc<dyn AirdropSubmitter>,
    policy_store: Arc<dyn FaucetPolicyStore>,
    policy_config: PolicyRuntimeConfig,
    execution: Arc<Semaphore>,
}

impl AirdropService {
    pub(crate) fn new(
        network_descriptor: NetworkDescriptor,
        submitter: Arc<dyn AirdropSubmitter>,
        policy_store: Arc<dyn FaucetPolicyStore>,
        policy_config: PolicyRuntimeConfig,
    ) -> Self {
        Self {
            network_descriptor,
            submitter,
            policy_store,
            policy_config,
            execution: Arc::new(Semaphore::new(MAX_CONCURRENT_AIRDROP_EXECUTIONS)),
        }
    }

    async fn request_airdrop(
        &self,
        headers: HeaderMap,
        peer: Option<SocketAddr>,
        request: HostedAirdropRequest,
    ) -> Response {
        if request.network_id != self.network_descriptor.network_id
            || request.genesis_hash.0 != self.network_descriptor.genesis_hash.0
        {
            return error_response(
                StatusCode::BAD_REQUEST,
                "airdrop_network_mismatch",
                "airdrop request does not match this faucet network",
            );
        }
        let Ok(execution_permit) = self.execution.clone().try_acquire_owned() else {
            return error_response_with_retry_after(
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_capacity_exceeded",
                "airdrop execution capacity reached",
                Some(1),
            );
        };
        let key = AirdropAttemptKey {
            network_id: request.network_id.clone(),
            genesis_hash: hex_string(request.genesis_hash.0),
            recipient: hex_string(request.recipient.0),
            client_nonce: hex_encode(request.client_nonce.as_bytes()),
        };
        let now = SystemTime::now();
        if let Err(error) = self.policy_store.expire(now) {
            return policy_store_error_response(&error);
        }
        let (client_identity, client_quota_max, client_quota_window) =
            self.airdrop_client_quota(&headers, peer);
        let attempt = AirdropAttempt {
            key: key.clone(),
            client_identity: client_identity.clone(),
            raw_amount: self.submitter.raw_amount().to_string(),
            status: AirdropAttemptStatus::Pending,
            tx_hash: None,
            height: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            // Unresolved attempts must never expire: removing one can let the same
            // idempotency key submit a second transaction.
            expires_at: None,
        };
        let quota_checks = [
            QuotaCheck {
                key: QuotaKey::AirdropRecipient(hex_string(request.recipient.0)),
                max: 1,
                window: Duration::from_secs(self.policy_config.airdrop.recipient_cooldown_secs),
            },
            QuotaCheck {
                key: QuotaKey::AirdropClient(client_identity),
                max: client_quota_max,
                window: Duration::from_secs(client_quota_window),
            },
        ];
        match self.policy_store.admit_airdrop_attempt(
            attempt.clone(),
            MAX_RETAINED_AIRDROP_ATTEMPTS,
            self.policy_config.airdrop.max_pending_per_recipient,
            &quota_checks,
            now,
        ) {
            Ok(AdmitAirdropAttemptResult::Inserted) => {
                self.spawn_submit_airdrop_attempt(request, attempt, execution_permit)
                    .await
            }
            Ok(AdmitAirdropAttemptResult::Existing(attempt)) => {
                self.existing_airdrop_attempt(&request, *attempt, execution_permit)
                    .await
            }
            Ok(AdmitAirdropAttemptResult::PendingLimitReached) => error_response_with_retry_after(
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_recipient_pending_limit_exceeded",
                "recipient already has an airdrop in progress",
                Some(5),
            ),
            Ok(AdmitAirdropAttemptResult::CapacityReached) => error_response_with_retry_after(
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_capacity_exceeded",
                "airdrop attempt capacity reached",
                Some(1),
            ),
            Ok(AdmitAirdropAttemptResult::QuotaExceeded(decision)) => {
                error_response_with_retry_after(
                    StatusCode::TOO_MANY_REQUESTS,
                    "airdrop_quota_exceeded",
                    "airdrop quota exceeded",
                    decision.retry_after.map(|duration| duration.as_secs()),
                )
            }
            Err(error) => policy_store_error_response(&error),
        }
    }

    async fn existing_airdrop_attempt(
        &self,
        request: &HostedAirdropRequest,
        mut attempt: AirdropAttempt,
        execution_permit: OwnedSemaphorePermit,
    ) -> Response {
        if attempt.error_code.as_deref() == Some(AIRDROP_SUBMIT_AMOUNT_MISMATCH) {
            return submit_amount_mismatch_response();
        }
        match attempt.status {
            AirdropAttemptStatus::Succeeded => {
                response_from_attempt(StatusCode::OK, request.recipient, attempt)
            }
            AirdropAttemptStatus::Pending => {
                if attempt.tx_hash.is_some() {
                    self.poll_airdrop_attempt(request.recipient, attempt).await
                } else {
                    response_from_attempt(StatusCode::ACCEPTED, request.recipient, attempt)
                }
            }
            AirdropAttemptStatus::FailedRetryable => {
                attempt = match self.policy_store.claim_airdrop_transition(
                    &attempt.key,
                    &[AirdropAttemptStatus::FailedRetryable],
                    AirdropAttemptStatus::Pending,
                    SystemTime::now(),
                ) {
                    Ok(attempt) => attempt,
                    Err(error) => return policy_store_error_response(&error),
                };
                attempt.error_code = None;
                self.spawn_submit_airdrop_attempt(request.clone(), attempt, execution_permit)
                    .await
            }
            AirdropAttemptStatus::FailedTerminal => error_response(
                StatusCode::CONFLICT,
                "airdrop_attempt_terminal",
                "airdrop attempt cannot be retried",
            ),
        }
    }

    fn airdrop_client_quota(
        &self,
        headers: &HeaderMap,
        peer: Option<SocketAddr>,
    ) -> (ClientIdentity, u32, u64) {
        if let Some(peer_ip) = peer.map(|peer| peer.ip())
            && let Some(client_override) = self
                .policy_config
                .airdrop
                .client_overrides
                .iter()
                .find(|client_override| client_override.ip == peer_ip)
        {
            tracing::info!(client_ip = %peer_ip, "using configured faucet airdrop client quota");
            return (
                ClientIdentity::Ip(peer_ip),
                client_override.quota_max,
                client_override.quota_window_secs,
            );
        }
        let identity = client_identity(
            headers,
            peer,
            self.policy_config.trust_proxy_client_ip_headers,
        );
        (
            identity,
            self.policy_config.airdrop.ip_quota_max,
            self.policy_config.airdrop.ip_quota_window_secs,
        )
    }

    async fn submit_airdrop_attempt(
        &self,
        request: HostedAirdropRequest,
        mut attempt: AirdropAttempt,
    ) -> Response {
        match self.submitter.submit_airdrop(request.recipient).await {
            Ok(accepted) => {
                let amount_matches =
                    accepted.raw_amount.parse::<u128>().ok() == Some(self.submitter.raw_amount());
                attempt.tx_hash = Some(hex_string(accepted.tx_hash.0.0));
                // An accepted transaction must not age out while its receipt is unresolved;
                // otherwise the same idempotency key could submit a second transaction.
                attempt.expires_at = None;
                if !amount_matches {
                    attempt.error_code = Some(AIRDROP_SUBMIT_AMOUNT_MISMATCH.into());
                }
                let now = SystemTime::now();
                attempt.updated_at = now;
                if let Err(error) = self.policy_store.put_airdrop_attempt(attempt.clone()) {
                    return policy_store_error_response(&error);
                }
                if !amount_matches {
                    return submit_amount_mismatch_response();
                }
                self.poll_airdrop_attempt(request.recipient, attempt).await
            }
            Err(error) => {
                attempt.status = AirdropAttemptStatus::FailedRetryable;
                attempt.error_code = Some(airdrop_error_code(&error).into());
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = Some(
                    now.checked_add(Duration::from_secs(
                        self.policy_config.airdrop.recipient_cooldown_secs,
                    ))
                    .unwrap_or(now),
                );
                let _ = self.policy_store.put_airdrop_attempt(attempt);
                error.into_response()
            }
        }
    }

    async fn spawn_submit_airdrop_attempt(
        &self,
        request: HostedAirdropRequest,
        attempt: AirdropAttempt,
        execution_permit: OwnedSemaphorePermit,
    ) -> Response {
        let service = self.clone();
        let recovery_service = self.clone();
        let recovery_key = attempt.key.clone();
        let worker =
            tokio::spawn(async move { service.submit_airdrop_attempt(request, attempt).await });
        // Keep panic recovery alive even if the client disconnects while the worker runs.
        let supervisor = tokio::spawn(async move {
            let _execution_permit = execution_permit;
            match worker.await {
                Ok(response) => response,
                Err(error) => recovery_service.failed_submission_task(&recovery_key, &error),
            }
        });
        match supervisor.await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(%error, "airdrop submission supervisor failed");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "airdrop_submit_failed",
                    "failed to submit airdrop transaction",
                )
            }
        }
    }

    fn failed_submission_task(
        &self,
        key: &AirdropAttemptKey,
        error: &tokio::task::JoinError,
    ) -> Response {
        tracing::error!(%error, "airdrop submission task failed");
        let mut attempt = match self.policy_store.get_airdrop_attempt(key) {
            Ok(Some(attempt)) => attempt,
            Ok(None) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "airdrop_submit_failed",
                    "failed to submit airdrop transaction",
                );
            }
            Err(error) => return policy_store_error_response(&error),
        };
        if !matches!(attempt.status, AirdropAttemptStatus::Pending) || attempt.tx_hash.is_some() {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "airdrop_submit_failed",
                "failed to submit airdrop transaction",
            );
        }
        attempt.status = AirdropAttemptStatus::FailedTerminal;
        attempt.error_code = Some("airdrop_submit_failed".into());
        let now = SystemTime::now();
        attempt.updated_at = now;
        attempt.expires_at =
            terminal_attempt_expiry(now, self.policy_config.airdrop.recipient_cooldown_secs);
        if let Err(error) = self.policy_store.put_airdrop_attempt(attempt) {
            return policy_store_error_response(&error);
        }
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "airdrop_submit_failed",
            "failed to submit airdrop transaction",
        )
    }

    async fn poll_airdrop_attempt(
        &self,
        recipient: ContractAddress,
        mut attempt: AirdropAttempt,
    ) -> Response {
        let Some(tx_hash) = attempt.tx_hash.as_deref().and_then(parse_tx_hash) else {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "airdrop_attempt_state_invalid",
                "airdrop attempt has an invalid accepted transaction hash",
            );
        };
        match self.submitter.poll_airdrop(tx_hash).await {
            Ok(receipt) if matches!(receipt.status, ExecStatus::Ok) => {
                attempt.status = AirdropAttemptStatus::Succeeded;
                attempt.height = Some(*receipt.height);
                attempt.error_code = None;
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = terminal_attempt_expiry(
                    now,
                    self.policy_config.airdrop.recipient_cooldown_secs,
                );
                if let Err(error) = self.policy_store.put_airdrop_attempt(attempt.clone()) {
                    return policy_store_error_response(&error);
                }
                response_from_attempt(StatusCode::OK, recipient, attempt)
            }
            Ok(receipt) => {
                attempt.status = AirdropAttemptStatus::FailedTerminal;
                attempt.height = Some(*receipt.height);
                attempt.error_code = Some("airdrop_transaction_failed".into());
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = terminal_attempt_expiry(
                    now,
                    self.policy_config.airdrop.recipient_cooldown_secs,
                );
                if let Err(error) = self.policy_store.put_airdrop_attempt(attempt) {
                    return policy_store_error_response(&error);
                }
                AirdropSubmitError::TransactionFailed { tx_hash }.into_response()
            }
            Err(AirdropSubmitError::Poll(
                groundhog_json_rpc_client::PollTransactionReceiptError::ReceiptTimeout { .. },
            )) => match self.policy_store.get_airdrop_attempt(&attempt.key) {
                Ok(Some(current)) => match current.status {
                    AirdropAttemptStatus::Pending => {
                        response_from_attempt(StatusCode::ACCEPTED, recipient, current)
                    }
                    AirdropAttemptStatus::Succeeded => {
                        response_from_attempt(StatusCode::OK, recipient, current)
                    }
                    AirdropAttemptStatus::FailedTerminal => {
                        AirdropSubmitError::TransactionFailed { tx_hash }.into_response()
                    }
                    AirdropAttemptStatus::FailedRetryable => error_response(
                        StatusCode::BAD_GATEWAY,
                        "airdrop_submit_failed",
                        "airdrop submission failed and may be retried",
                    ),
                },
                Ok(None) => error_response(
                    StatusCode::CONFLICT,
                    "airdrop_attempt_conflict",
                    "airdrop attempt disappeared while polling",
                ),
                Err(error) => policy_store_error_response(&error),
            },
            Err(error) => error.into_response(),
        }
    }
}

fn submit_amount_mismatch_response() -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        AIRDROP_SUBMIT_AMOUNT_MISMATCH,
        "airdrop submitter returned an amount that does not match its configuration",
    )
}

fn terminal_attempt_expiry(now: SystemTime, retention_secs: u64) -> Option<SystemTime> {
    now.checked_add(Duration::from_secs(
        retention_secs.max(MIN_AIRDROP_TERMINAL_RETENTION_SECS),
    ))
}

fn response_from_attempt(
    status: StatusCode,
    recipient: ContractAddress,
    attempt: AirdropAttempt,
) -> Response {
    let tx_hash = attempt.tx_hash.as_deref().and_then(parse_tx_hash);
    let response = match (attempt.status, attempt.height) {
        (AirdropAttemptStatus::Pending, _) => AirdropAttemptResponse::Pending {
            recipient,
            raw_amount: attempt.raw_amount,
            tx_hash,
            retry_after_seconds: 5,
        },
        (AirdropAttemptStatus::Succeeded, Some(height)) => match tx_hash {
            Some(tx_hash) => AirdropAttemptResponse::Succeeded {
                recipient,
                tx_hash,
                raw_amount: attempt.raw_amount,
                height: height.into(),
                status: ExecStatus::Ok,
            },
            None => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "airdrop_attempt_state_invalid",
                    "succeeded airdrop attempt has no valid transaction hash",
                );
            }
        },
        _ => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "airdrop_attempt_state_invalid",
                "airdrop attempt has invalid response state",
            );
        }
    };
    (status, Json(response)).into_response()
}

fn policy_store_error_response(error: &PolicyStoreError) -> Response {
    match error {
        PolicyStoreError::InvalidAirdropTransition { .. } => error_response(
            StatusCode::ACCEPTED,
            "airdrop_already_pending",
            "airdrop attempt is already in progress",
        ),
        PolicyStoreError::AttemptNotFound
        | PolicyStoreError::InvalidWalletCreateTransition { .. } => error_response(
            StatusCode::CONFLICT,
            "airdrop_attempt_conflict",
            "airdrop attempt state changed while processing",
        ),
        PolicyStoreError::LockPoisoned => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "faucet_policy_store_unavailable",
            "faucet policy store is unavailable",
        ),
    }
}

fn airdrop_error_code(error: &AirdropSubmitError) -> &'static str {
    match error {
        AirdropSubmitError::TransactionFailed { .. } => "airdrop_transaction_failed",
        AirdropSubmitError::RpcConnect(_)
        | AirdropSubmitError::ChainTip(_)
        | AirdropSubmitError::EncodeTransferCalldata(_)
        | AirdropSubmitError::BuildTransaction(_)
        | AirdropSubmitError::SignTransaction(_)
        | AirdropSubmitError::Submit(_)
        | AirdropSubmitError::Poll(_) => "airdrop_submit_failed",
    }
}

fn hex_string(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex_encode(bytes.as_ref()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn parse_tx_hash(value: &str) -> Option<TxHash> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes: [u8; 32] = hex::decode(value).ok()?.try_into().ok()?;
    Some(TxHash(SHA256Hash::from(
        groundhog_xdr::types::SHA256Hash::from(bytes),
    )))
}

pub(crate) async fn request_airdrop(
    State(service): State<Arc<AirdropService>>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Result<Json<HostedAirdropRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request = match request {
        Ok(Json(request)) => request,
        Err(rejection) if rejection.body_text().contains("clientNonce") => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "airdrop_invalid_client_nonce",
                "airdrop request contains an invalid clientNonce",
            );
        }
        Err(rejection) => return rejection.into_response(),
    };
    service.request_airdrop(headers, Some(peer), request).await
}

/// Submits faucet airdrops for validated recipient wallet addresses.
#[async_trait::async_trait]
pub trait AirdropSubmitter: Send + Sync + 'static {
    /// Raw HOG units transferred by this submitter.
    fn raw_amount(&self) -> u128;

    /// Submit an airdrop to `recipient` and return once the transaction is accepted.
    async fn submit_airdrop(
        &self,
        recipient: ContractAddress,
    ) -> Result<AcceptedAirdrop, AirdropSubmitError>;

    /// Poll a previously accepted airdrop transaction.
    async fn poll_airdrop(&self, tx_hash: TxHash)
    -> Result<TransactionReceipt, AirdropSubmitError>;
}

/// Faucet airdrop accepted by the transaction submission endpoint.
#[derive(Debug, Clone)]
pub struct AcceptedAirdrop {
    /// Raw HOG units reported by the submitter.
    pub raw_amount: String,
    /// Submitted transaction hash.
    pub tx_hash: TxHash,
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
    /// The signed faucet transaction was not accepted.
    #[error("failed to submit airdrop transaction: {0}")]
    Submit(#[from] groundhog_json_rpc_client::SubmitTransactionError),
    /// The accepted faucet transaction could not be confirmed.
    #[error("failed to confirm airdrop transaction: {0}")]
    Poll(#[from] groundhog_json_rpc_client::PollTransactionReceiptError),
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
            | Self::Submit(_)
            | Self::Poll(_) => error_response(
                StatusCode::BAD_GATEWAY,
                "airdrop_submit_failed",
                "failed to submit airdrop transaction",
            ),
        }
    }
}

/// Runtime-only airdrop execution config.
#[derive(Clone, PartialEq, Eq)]
pub struct AirdropRuntimeConfig {
    /// Raw HOG units transferred for each airdrop request.
    pub raw_amount: u128,
    /// Faucet wallet `doAction` entrypoint.
    pub wallet_do_action_entrypoint: ContractEntrypointID,
    /// 32-byte faucet signing secret key.
    pub faucet_secret_key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for AirdropRuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AirdropRuntimeConfig")
            .field("raw_amount", &self.raw_amount)
            .field(
                "wallet_do_action_entrypoint",
                &self.wallet_do_action_entrypoint,
            )
            .field("faucet_secret_key", &format_args!("<redacted>"))
            .finish()
    }
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
}

impl AirdropRuntimeConfig {
    /// Validate wire-format config values into runtime airdrop config.
    pub fn from_wire(
        raw_amount: &str,
        wallet_do_action_entrypoint: HexBytes,
        faucet_secret_key: Zeroizing<[u8; 32]>,
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
        Ok(Self {
            raw_amount,
            wallet_do_action_entrypoint: ContractEntrypointID::from(wallet_do_action_entrypoint),
            faucet_secret_key,
        })
    }
}

pub struct RpcAirdropSubmitter {
    rpc_url: String,
    raw_amount: u128,
    network_hash: groundhog_xdr::types::NetworkHash,
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
        network_hash: groundhog_xdr::types::NetworkHash,
    ) -> Result<Self, AirdropRuntimeConfigError> {
        Ok(Self {
            rpc_url: descriptor.endpoints.rpc_url.clone(),
            raw_amount: config.raw_amount,
            network_hash,
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
    fn raw_amount(&self) -> u128 {
        self.raw_amount
    }

    async fn submit_airdrop(
        &self,
        recipient: ContractAddress,
    ) -> Result<AcceptedAirdrop, AirdropSubmitError> {
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
            &self.network_hash,
        )
        .map_err(|error| AirdropSubmitError::SignTransaction(error.to_string()))?;
        let tx_hash = submit_transaction(&self.rpc_url, &tx_hex).await?;
        Ok(AcceptedAirdrop {
            raw_amount: self.raw_amount.to_string(),
            tx_hash,
        })
    }

    async fn poll_airdrop(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionReceipt, AirdropSubmitError> {
        poll_transaction_receipt(&self.rpc_url, tx_hash)
            .await
            .map_err(Into::into)
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
    use std::time::{Duration, SystemTime};

    use groundhog_json_rpc_types::hex::HexBytes;
    use serde_json::json;
    use zeroize::Zeroizing;

    use groundhog_json_rpc_types::airdrop::HostedAirdropRequest;

    use super::{
        AirdropRuntimeConfig, MIN_AIRDROP_TERMINAL_RETENTION_SECS, terminal_attempt_expiry,
    };

    #[test]
    fn zero_cooldown_keeps_a_finite_terminal_idempotency_window() {
        let now = SystemTime::UNIX_EPOCH;

        assert_eq!(
            terminal_attempt_expiry(now, 0),
            Some(now + Duration::from_secs(MIN_AIRDROP_TERMINAL_RETENTION_SECS))
        );
    }

    #[test]
    fn airdrop_runtime_config_debug_redacts_faucet_secret_key() {
        let config = AirdropRuntimeConfig::from_wire(
            "100000000",
            HexBytes(vec![0x64; 8]),
            Zeroizing::new([0xAB; 32]),
        )
        .expect("bug: runtime config should validate");

        let debug = format!("{config:?}");

        assert!(debug.contains("faucet_secret_key: <redacted>"), "{debug}");
        assert!(!debug.contains("171"), "{debug}");
    }

    #[test]
    fn airdrop_request_accepts_future_optional_fields() {
        let request: HostedAirdropRequest = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x99",
            "networkId": "future-network",
            "genesisHash": format!("0x{}", "22".repeat(32)),
        }))
        .expect("bug: public API requests should ignore unknown future fields");

        assert_eq!(request.recipient.0.as_ref(), &[0x11; 32]);
    }

    #[test]
    fn airdrop_request_accepts_32_byte_client_nonce() {
        let request: HostedAirdropRequest = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": format!("0x{}", "99".repeat(32)),
            "networkId": "future-network",
            "genesisHash": format!("0x{}", "22".repeat(32)),
        }))
        .expect("bug: 32-byte client nonce should be valid");

        assert_eq!(request.client_nonce.as_bytes().len(), 32);
    }
}
