use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use thruput_json_rpc_types::base58::FixedIdentifier;

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use thruput_json_rpc_client::native::{
    NativeClient, NativeClientError,
    receipt::{ReceiptPollCause, ReceiptPollConfig, ReceiptPollError, poll_receipt},
};
use thruput_json_rpc_types::airdrop::{AirdropAttemptResponse, HostedAirdropRequest};
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::network_descriptor::NetworkDescriptor;
use thruput_json_rpc_types::receipt::ExecStatus;
use thruput_json_rpc_types::transaction::TxHash;
use thruput_json_rpc_types::{ContractAddress, SHA256Hash};
use thruput_json_rpc_types::{
    requests,
    responses::{DecodedResponse, ReceiptExecution, ResponseData, TransactionReceipt},
};
use thruput_signing::default_wallet::{
    BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction,
};
use thruput_signing::{
    DEFAULT_TX_GAS_LIMIT, LocalSigner, PubkeyAccount, SignTransactionParams,
    sign_and_encode_transaction_hex, transaction_hash,
};
use thruput_xdr::contract::ContractEntrypointID;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::Instrument;
use zeroize::Zeroizing;

use crate::{
    config::{MIN_AIRDROP_TERMINAL_RETENTION_SECS, PolicyRuntimeConfig},
    error::{error_response, error_response_with_retry_after},
    policy::{
        AdmitAirdropAttemptResult, AirdropAttempt, AirdropAttemptKey, AirdropAttemptStatus,
        ClientIdentity, FaucetPolicyStore, PolicyStoreError, QuotaCheck, QuotaKey, client_identity,
    },
    utils::next_nonce,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const AIRDROP_SUBMIT_AMOUNT_MISMATCH: &str = "airdrop_submit_amount_mismatch";
const MAX_CONCURRENT_AIRDROP_EXECUTIONS: usize = 64;
const AIRDROP_EXECUTION_TIMEOUT: Duration = Duration::from_mins(10);

enum ReplaceAttemptError {
    Stale,
    Store(PolicyStoreError, u64),
}

enum ExecutionReservation {
    Permit(OwnedSemaphorePermit),
    Existing(Box<AirdropAttempt>),
}

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
        if !self.request_matches_network(&request) {
            return error_response(
                StatusCode::BAD_REQUEST,
                "airdrop_network_mismatch",
                "airdrop request does not match this faucet network",
            );
        }
        let key = AirdropAttemptKey {
            network_id: request.network_id.clone(),
            genesis_hash: request.genesis_hash.to_string(),
            recipient: request.recipient.to_string(),
            client_nonce: hex::encode(request.client_nonce.as_bytes()),
        };
        let now = SystemTime::now();
        if let Err(error) = self.policy_store.expire(now) {
            return policy_store_error_response(&error);
        }
        let execution_permit = match self.reserve_execution_for_new_attempt(&key) {
            Ok(ExecutionReservation::Permit(permit)) => permit,
            Ok(ExecutionReservation::Existing(attempt)) => {
                return self
                    .start_or_observe_airdrop_attempt(request, *attempt, None)
                    .await;
            }
            Err(response) => return *response,
        };
        let (client_identity, client_quota_max, client_quota_window) =
            self.airdrop_client_quota(&headers, peer);
        let attempt = AirdropAttempt {
            key: key.clone(),
            client_identity: client_identity.clone(),
            raw_amount: self.submitter.raw_amount().to_string(),
            status: AirdropAttemptStatus::Pending,
            execution_id: 0,
            tx_hash: None,
            transaction: None,
            height: None,
            return_data: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            // Unresolved attempts must never expire: removing one can let the same
            // idempotency key submit a second transaction.
            expires_at: None,
        };
        let quota_checks = [
            QuotaCheck {
                key: QuotaKey::AirdropRecipient(request.recipient.to_string()),
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
            self.policy_config.airdrop.max_retained_attempts,
            self.policy_config.airdrop.max_pending_per_recipient,
            &quota_checks,
            now,
        ) {
            Ok(AdmitAirdropAttemptResult::Inserted) => {
                self.start_or_observe_airdrop_attempt(request, attempt, Some(execution_permit))
                    .await
            }
            Ok(AdmitAirdropAttemptResult::Existing(attempt)) => {
                self.start_or_observe_airdrop_attempt(request, *attempt, Some(execution_permit))
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

    async fn start_or_observe_airdrop_attempt(
        &self,
        request: HostedAirdropRequest,
        attempt: AirdropAttempt,
        execution_permit: Option<OwnedSemaphorePermit>,
    ) -> Response {
        if attempt.error_code.as_deref() == Some(AIRDROP_SUBMIT_AMOUNT_MISMATCH) {
            return submit_amount_mismatch_response();
        }
        let previous_status = match attempt.status {
            AirdropAttemptStatus::Succeeded => {
                return response_from_attempt(StatusCode::OK, request.recipient, attempt);
            }
            AirdropAttemptStatus::Executing | AirdropAttemptStatus::Ambiguous => {
                return response_from_attempt(StatusCode::ACCEPTED, request.recipient, attempt);
            }
            AirdropAttemptStatus::Pending | AirdropAttemptStatus::FailedRetryable => attempt.status,
            AirdropAttemptStatus::FailedTerminal => {
                return error_response(
                    StatusCode::CONFLICT,
                    "airdrop_attempt_terminal",
                    "airdrop attempt cannot be retried",
                );
            }
        };
        let attempt = match self.policy_store.claim_airdrop_transition(
            &attempt.key,
            &[previous_status],
            None,
            AirdropAttemptStatus::Executing,
            SystemTime::now(),
        ) {
            Ok(attempt) => attempt,
            Err(PolicyStoreError::InvalidAirdropTransition { .. }) => {
                return self.current_airdrop_attempt_response(request.recipient, &attempt.key);
            }
            Err(error) => return policy_store_error_response(&error),
        };
        let execution_permit = if let Some(execution_permit) = execution_permit {
            execution_permit
        } else {
            let Ok(execution_permit) = self.execution.clone().try_acquire_owned() else {
                match self.policy_store.claim_airdrop_transition(
                    &attempt.key,
                    &[AirdropAttemptStatus::Executing],
                    Some(attempt.execution_id),
                    previous_status,
                    SystemTime::now(),
                ) {
                    Ok(_) => {}
                    Err(PolicyStoreError::InvalidAirdropTransition { .. }) => {
                        return self
                            .current_airdrop_attempt_response(request.recipient, &attempt.key);
                    }
                    Err(error) => return policy_store_error_response(&error),
                }
                return error_response_with_retry_after(
                    StatusCode::TOO_MANY_REQUESTS,
                    "airdrop_capacity_exceeded",
                    "airdrop execution capacity reached",
                    Some(1),
                );
            };
            execution_permit
        };
        self.spawn_airdrop_worker(request, attempt, execution_permit)
            .await
    }

    fn current_airdrop_attempt_response(
        &self,
        recipient: ContractAddress,
        key: &AirdropAttemptKey,
    ) -> Response {
        match self.policy_store.get_airdrop_attempt(key) {
            Ok(Some(attempt)) => match attempt.status {
                AirdropAttemptStatus::Pending
                | AirdropAttemptStatus::Executing
                | AirdropAttemptStatus::Ambiguous => {
                    response_from_attempt(StatusCode::ACCEPTED, recipient, attempt)
                }
                AirdropAttemptStatus::Succeeded => {
                    response_from_attempt(StatusCode::OK, recipient, attempt)
                }
                AirdropAttemptStatus::FailedRetryable => error_response(
                    StatusCode::BAD_GATEWAY,
                    "airdrop_submit_failed",
                    "airdrop submission failed and may be retried",
                ),
                AirdropAttemptStatus::FailedTerminal => error_response(
                    StatusCode::CONFLICT,
                    "airdrop_attempt_terminal",
                    "airdrop attempt cannot be retried",
                ),
            },
            Ok(None) => error_response(
                StatusCode::CONFLICT,
                "airdrop_attempt_conflict",
                "airdrop attempt disappeared while processing",
            ),
            Err(error) => policy_store_error_response(&error),
        }
    }

    fn replace_executing_attempt(
        &self,
        attempt: AirdropAttempt,
    ) -> Result<AirdropAttempt, ReplaceAttemptError> {
        let execution_id = attempt.execution_id;
        match self
            .policy_store
            .replace_airdrop_attempt_if_status(&[AirdropAttemptStatus::Executing], attempt)
        {
            Ok(attempt) => Ok(attempt),
            Err(PolicyStoreError::InvalidAirdropTransition { .. }) => {
                Err(ReplaceAttemptError::Stale)
            }
            Err(error) => Err(ReplaceAttemptError::Store(error, execution_id)),
        }
    }

    fn replace_attempt_error_response(
        &self,
        recipient: ContractAddress,
        key: &AirdropAttemptKey,
        error: ReplaceAttemptError,
    ) -> Response {
        match error {
            ReplaceAttemptError::Stale => self.current_airdrop_attempt_response(recipient, key),
            ReplaceAttemptError::Store(error, execution_id) => {
                self.release_execution_claim(key, execution_id);
                policy_store_error_response(&error)
            }
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

    fn request_matches_network(&self, request: &HostedAirdropRequest) -> bool {
        request.network_id == self.network_descriptor.network_id
            && request.network_hash == self.network_descriptor.network_hash
            && request.genesis_hash.0 == self.network_descriptor.genesis_hash.0
    }

    fn reserve_execution_for_new_attempt(
        &self,
        key: &AirdropAttemptKey,
    ) -> Result<ExecutionReservation, Box<Response>> {
        match self.policy_store.get_airdrop_attempt(key) {
            Ok(Some(attempt)) => return Ok(ExecutionReservation::Existing(Box::new(attempt))),
            Ok(None) => {}
            Err(error) => return Err(Box::new(policy_store_error_response(&error))),
        }
        if let Ok(permit) = self.execution.clone().try_acquire_owned() {
            return Ok(ExecutionReservation::Permit(permit));
        }
        match self.policy_store.get_airdrop_attempt(key) {
            Ok(Some(attempt)) => Ok(ExecutionReservation::Existing(Box::new(attempt))),
            Ok(None) => Err(Box::new(error_response_with_retry_after(
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_capacity_exceeded",
                "airdrop execution capacity reached",
                Some(1),
            ))),
            Err(error) => Err(Box::new(policy_store_error_response(&error))),
        }
    }

    async fn submit_airdrop_attempt(
        &self,
        request: HostedAirdropRequest,
        mut attempt: AirdropAttempt,
    ) -> Response {
        let key = attempt.key.clone();
        let prepared = match self.submitter.prepare_airdrop(request.recipient).await {
            Ok(prepared) => prepared,
            Err(error) => {
                attempt.status = AirdropAttemptStatus::FailedRetryable;
                attempt.expires_at = terminal_attempt_expiry(
                    SystemTime::now(),
                    self.policy_config.airdrop.recipient_cooldown_secs,
                );
                return self.finish_airdrop_error(request.recipient, attempt, error);
            }
        };
        let amount_matches =
            prepared.raw_amount.parse::<u128>().ok() == Some(self.submitter.raw_amount());
        attempt.tx_hash = Some(prepared.tx_hash.0.to_string());
        attempt.transaction = Some(prepared.clone());
        attempt.error_code = None;
        attempt.expires_at = None;
        attempt.updated_at = SystemTime::now();
        if !amount_matches {
            attempt.error_code = Some(AIRDROP_SUBMIT_AMOUNT_MISMATCH.into());
            attempt.status = AirdropAttemptStatus::FailedTerminal;
            attempt.expires_at = terminal_attempt_expiry(
                attempt.updated_at,
                self.policy_config.airdrop.recipient_cooldown_secs,
            );
        }
        // Retain the original identity before any broadcast, including a lost response.
        let mut attempt = match self.replace_executing_attempt(attempt) {
            Ok(attempt) => attempt,
            Err(error) => {
                return self.replace_attempt_error_response(request.recipient, &key, error);
            }
        };
        if !amount_matches {
            return submit_amount_mismatch_response();
        }
        if let Err(error) = self.submitter.submit_airdrop(&prepared).await {
            if error.proves_first_send_not_admitted(prepared.tx_hash) {
                attempt.status = AirdropAttemptStatus::FailedTerminal;
                attempt.expires_at = terminal_attempt_expiry(
                    SystemTime::now(),
                    self.policy_config.airdrop.recipient_cooldown_secs,
                );
            } else {
                attempt.status = AirdropAttemptStatus::Pending;
            }
            return self.finish_airdrop_error(request.recipient, attempt, error);
        }
        self.poll_airdrop_attempt(request.recipient, attempt).await
    }

    fn finish_airdrop_error(
        &self,
        recipient: ContractAddress,
        mut attempt: AirdropAttempt,
        error: AirdropSubmitError,
    ) -> Response {
        tracing::warn!(
            error = %error,
            details = ?error,
            status = ?attempt.status,
            transaction_hash = ?attempt.tx_hash,
            "airdrop attempt failed"
        );
        let key = attempt.key.clone();
        attempt.error_code = Some(airdrop_error_code(&error).into());
        attempt.updated_at = SystemTime::now();
        match self.replace_executing_attempt(attempt) {
            Ok(_) => error.into_response(),
            Err(replace_error) => {
                self.replace_attempt_error_response(recipient, &key, replace_error)
            }
        }
    }

    async fn execute_airdrop_attempt(
        &self,
        request: HostedAirdropRequest,
        attempt: AirdropAttempt,
    ) -> Response {
        if attempt.tx_hash.is_some() {
            self.poll_airdrop_attempt(request.recipient, attempt).await
        } else {
            self.submit_airdrop_attempt(request, attempt).await
        }
    }

    async fn spawn_airdrop_worker(
        &self,
        request: HostedAirdropRequest,
        attempt: AirdropAttempt,
        execution_permit: OwnedSemaphorePermit,
    ) -> Response {
        let service = self.clone();
        let timeout_service = self.clone();
        let timeout_key = attempt.key.clone();
        let execution_id = attempt.execution_id;
        let recovery_service = self.clone();
        let recovery_key = attempt.key.clone();
        let worker = tokio::spawn(
            async move {
                match tokio::time::timeout(
                    AIRDROP_EXECUTION_TIMEOUT,
                    service.execute_airdrop_attempt(request, attempt),
                )
                .await
                {
                    Ok(response) => response,
                    Err(_) => timeout_service.timed_out_airdrop_task(&timeout_key, execution_id),
                }
            }
            .in_current_span(),
        );
        // Keep panic recovery alive even if the client disconnects while the worker runs.
        let supervisor = tokio::spawn(async move {
            let _execution_permit = execution_permit;
            match worker.await {
                Ok(response) => response,
                Err(error) => {
                    recovery_service.failed_airdrop_task(&recovery_key, execution_id, &error)
                }
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

    fn failed_airdrop_task(
        &self,
        key: &AirdropAttemptKey,
        execution_id: u64,
        error: &tokio::task::JoinError,
    ) -> Response {
        tracing::error!(%error, "airdrop worker task failed");
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
        if attempt.status != AirdropAttemptStatus::Executing || attempt.execution_id != execution_id
        {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "airdrop_submit_failed",
                "failed to submit airdrop transaction",
            );
        }
        attempt.status = if attempt.tx_hash.is_some() {
            AirdropAttemptStatus::Pending
        } else {
            AirdropAttemptStatus::FailedTerminal
        };
        attempt.error_code = Some("airdrop_submit_failed".into());
        let now = SystemTime::now();
        attempt.updated_at = now;
        attempt.expires_at = if attempt.tx_hash.is_some() {
            None
        } else {
            terminal_attempt_expiry(now, self.policy_config.airdrop.recipient_cooldown_secs)
        };
        match self
            .policy_store
            .replace_airdrop_attempt_if_status(&[AirdropAttemptStatus::Executing], attempt)
        {
            Ok(_) | Err(PolicyStoreError::InvalidAirdropTransition { .. }) => {}
            Err(error) => {
                self.release_execution_claim(key, execution_id);
                return policy_store_error_response(&error);
            }
        }
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "airdrop_submit_failed",
            "failed to submit airdrop transaction",
        )
    }

    fn timed_out_airdrop_task(&self, key: &AirdropAttemptKey, execution_id: u64) -> Response {
        tracing::error!("airdrop worker task timed out");
        let mut attempt = match self.policy_store.get_airdrop_attempt(key) {
            Ok(Some(attempt)) => attempt,
            Ok(None) => {
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "airdrop_submit_failed",
                    "airdrop transaction execution timed out",
                );
            }
            Err(error) => return policy_store_error_response(&error),
        };
        if attempt.status != AirdropAttemptStatus::Executing || attempt.execution_id != execution_id
        {
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "airdrop_submit_failed",
                "airdrop transaction execution timed out",
            );
        }
        attempt.status = if attempt.tx_hash.is_some() {
            AirdropAttemptStatus::Pending
        } else {
            AirdropAttemptStatus::Ambiguous
        };
        attempt.error_code = Some("airdrop_submit_failed".into());
        attempt.updated_at = SystemTime::now();
        attempt.expires_at = None;
        match self
            .policy_store
            .replace_airdrop_attempt_if_status(&[AirdropAttemptStatus::Executing], attempt)
        {
            Ok(_) | Err(PolicyStoreError::InvalidAirdropTransition { .. }) => {}
            Err(error) => {
                self.release_execution_claim(key, execution_id);
                return policy_store_error_response(&error);
            }
        }
        error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "airdrop_submit_failed",
            "airdrop transaction execution timed out",
        )
    }

    fn release_execution_claim(&self, key: &AirdropAttemptKey, execution_id: u64) {
        match self.policy_store.claim_airdrop_transition(
            key,
            &[AirdropAttemptStatus::Executing],
            Some(execution_id),
            AirdropAttemptStatus::Pending,
            SystemTime::now(),
        ) {
            Ok(_)
            | Err(
                PolicyStoreError::AttemptNotFound
                | PolicyStoreError::InvalidAirdropTransition { .. },
            ) => {}
            Err(error) => tracing::error!(%error, "failed to release airdrop worker claim"),
        }
    }

    async fn observed_funding_receipt(
        &self,
        tx_hash: TxHash,
        committed_height: Option<i64>,
    ) -> Result<TransactionReceipt, AirdropSubmitError> {
        let receipt = self
            .submitter
            .poll_airdrop(tx_hash, committed_height)
            .await?;
        if !valid_funding_receipt(tx_hash, &receipt)
            || committed_height.is_some_and(|height| height != *receipt.block_height)
        {
            return Err(AirdropSubmitError::InvalidReceipt);
        }
        Ok(receipt)
    }

    async fn poll_airdrop_attempt(
        &self,
        recipient: ContractAddress,
        mut attempt: AirdropAttempt,
    ) -> Response {
        let key = attempt.key.clone();
        let Some(tx_hash) = attempt.tx_hash.as_deref().and_then(parse_tx_hash) else {
            attempt.status = AirdropAttemptStatus::FailedTerminal;
            attempt.error_code = Some("airdrop_attempt_state_invalid".into());
            attempt.updated_at = SystemTime::now();
            attempt.expires_at = terminal_attempt_expiry(
                attempt.updated_at,
                self.policy_config.airdrop.recipient_cooldown_secs,
            );
            let _ = self
                .policy_store
                .replace_airdrop_attempt_if_status(&[AirdropAttemptStatus::Executing], attempt);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "airdrop_attempt_state_invalid",
                "airdrop attempt has an invalid accepted transaction hash",
            );
        };
        let result = self.observed_funding_receipt(tx_hash, attempt.height).await;
        if let Err(AirdropSubmitError::Poll(error)) = &result
            && let Some(commitment) = &error.evidence.commitment
            && commitment.transaction_hash == tx_hash
            && *commitment.block_height >= 0
        {
            attempt.height.get_or_insert(*commitment.block_height);
        }
        match result {
            Ok(receipt) if matches!(receipt.execution, ReceiptExecution::Return { .. }) => {
                attempt.status = AirdropAttemptStatus::Succeeded;
                attempt.return_data = match receipt.execution {
                    ReceiptExecution::Return { return_data } => Some(return_data),
                    _ => None,
                };
                attempt.height = Some(*receipt.block_height);
                attempt.error_code = None;
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = terminal_attempt_expiry(
                    now,
                    self.policy_config.airdrop.recipient_cooldown_secs,
                );
                match self.replace_executing_attempt(attempt) {
                    Ok(attempt) => response_from_attempt(StatusCode::OK, recipient, attempt),
                    Err(error) => self.replace_attempt_error_response(recipient, &key, error),
                }
            }
            Ok(receipt) => {
                attempt.status = AirdropAttemptStatus::FailedTerminal;
                attempt.height = Some(*receipt.block_height);
                attempt.error_code = Some("airdrop_transaction_failed".into());
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = terminal_attempt_expiry(
                    now,
                    self.policy_config.airdrop.recipient_cooldown_secs,
                );
                match self.replace_executing_attempt(attempt) {
                    Ok(_) => AirdropSubmitError::TransactionFailed { tx_hash }.into_response(),
                    Err(error) => self.replace_attempt_error_response(recipient, &key, error),
                }
            }
            Err(AirdropSubmitError::Poll(error))
                if matches!(error.cause, ReceiptPollCause::Timeout) =>
            {
                // Scheduler diagnostics never establish safe redispatch. Repeated
                // HTTP requests reconcile the retained hash without sending again.
                tracing::info!(
                    transaction_hash = %tx_hash.0,
                    last_status = ?error.evidence.last_status,
                    last_lookup_error = ?error.evidence.last_lookup_error,
                    "airdrop receipt remains unresolved"
                );
                attempt.status = AirdropAttemptStatus::Pending;
                attempt.updated_at = SystemTime::now();
                match self.replace_executing_attempt(attempt) {
                    Ok(attempt) => response_from_attempt(StatusCode::ACCEPTED, recipient, attempt),
                    Err(error) => self.replace_attempt_error_response(recipient, &key, error),
                }
            }
            Err(error) => {
                attempt.status = AirdropAttemptStatus::Pending;
                self.finish_airdrop_error(recipient, attempt, error)
            }
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

fn valid_funding_receipt(tx_hash: TxHash, receipt: &TransactionReceipt) -> bool {
    receipt.transaction_hash == tx_hash && *receipt.block_height >= 0 && receipt.is_supported()
}

fn response_from_attempt(
    status: StatusCode,
    recipient: ContractAddress,
    attempt: AirdropAttempt,
) -> Response {
    let tx_hash = attempt.tx_hash.as_deref().and_then(parse_tx_hash);
    let response = match (attempt.status, attempt.height) {
        (
            AirdropAttemptStatus::Pending
            | AirdropAttemptStatus::Executing
            | AirdropAttemptStatus::Ambiguous,
            _,
        ) => AirdropAttemptResponse::Pending {
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
                return_data: attempt.return_data,
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
        PolicyStoreError::AttemptNotFound => error_response(
            StatusCode::CONFLICT,
            "airdrop_attempt_conflict",
            "airdrop attempt state changed while processing",
        ),
        PolicyStoreError::LockPoisoned
        | PolicyStoreError::QuotaWindowTooLarge
        | PolicyStoreError::AirdropExecutionIdExhausted => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "faucet_policy_store_unavailable",
            "faucet policy store is unavailable",
        ),
    }
}

fn airdrop_error_code(error: &AirdropSubmitError) -> &'static str {
    match error {
        AirdropSubmitError::TransactionFailed { .. } => "airdrop_transaction_failed",
        AirdropSubmitError::InvalidReceipt => "airdrop_receipt_invalid",
        AirdropSubmitError::RpcConnect(_)
        | AirdropSubmitError::Preflight(_)
        | AirdropSubmitError::Rpc { .. }
        | AirdropSubmitError::Unsupported { .. }
        | AirdropSubmitError::EncodeTransferCalldata(_)
        | AirdropSubmitError::BuildTransaction(_)
        | AirdropSubmitError::SignTransaction(_)
        | AirdropSubmitError::Submit(_)
        | AirdropSubmitError::Encoding(_)
        | AirdropSubmitError::Poll(_) => "airdrop_submit_failed",
    }
}

pub(crate) fn parse_tx_hash(value: &str) -> Option<TxHash> {
    let hash = thruput_xdr::types::SHA256Hash::decode_base58(value).ok()?;
    Some(TxHash(SHA256Hash::from(hash)))
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

    /// Build and sign an airdrop without broadcasting it.
    async fn prepare_airdrop(
        &self,
        recipient: ContractAddress,
    ) -> Result<PreparedAirdrop, AirdropSubmitError>;

    /// Submit a newly retained transaction once, without signing a replacement.
    /// Return only after admission of the expected hash. `RpcConnect`, `Preflight`
    /// and `Encoding` are valid only before any transaction bytes are sent.
    /// After dispatch, retain uncertainty and return the original method-decoded
    /// `Rpc` error (including transport failures), never a pre-dispatch error.
    async fn submit_airdrop(&self, transaction: &PreparedAirdrop)
    -> Result<(), AirdropSubmitError>;

    /// Poll a previously accepted transaction without discarding known commitment.
    async fn poll_airdrop(
        &self,
        tx_hash: TxHash,
        committed_height: Option<i64>,
    ) -> Result<TransactionReceipt, AirdropSubmitError>;
}

/// Faucet transaction ready to retain before broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAirdrop {
    /// Raw HOG units reported by the submitter.
    pub raw_amount: String,
    /// Canonical transaction hash, computed before submission.
    pub tx_hash: TxHash,
    /// Original signed transaction, encoded as hexadecimal.
    pub signed_transaction: String,
    /// Original expiry, retained as evidence.
    pub block_expiry_height: i64,
}

/// Errors produced while submitting a faucet airdrop.
#[derive(Debug, thiserror::Error)]
pub enum AirdropSubmitError {
    /// Receipt metadata or execution output is inconsistent with the attempt.
    #[error("faucet received an inconsistent transaction receipt")]
    InvalidReceipt,
    /// The transaction committed but did not execute successfully.
    #[error("airdrop transaction {tx_hash:?} committed but execution failed")]
    TransactionFailed { tx_hash: TxHash },
    /// The RPC client could not be constructed, before any transaction bytes were sent.
    #[error("failed to connect to RPC endpoint: {0}")]
    RpcConnect(String),
    /// Endpoint or signing-network preflight failed before transaction dispatch.
    #[error("RPC preflight failed: {0}")]
    Preflight(Box<thruput_client::native::NativeRpcError>),
    /// A native call failed, retaining its original application or transport error.
    #[error("{method} failed: {source}")]
    Rpc {
        method: &'static str,
        source: Box<NativeClientError>,
    },
    /// An unfamiliar response is retained without assuming its meaning.
    #[error("unsupported {method} response: {raw}")]
    Unsupported {
        method: &'static str,
        raw: serde_json::Value,
    },
    /// The HOG transfer calldata could not be encoded.
    #[error("failed to encode HOG transfer calldata: {0}")]
    EncodeTransferCalldata(String),
    /// The faucet transaction could not be built.
    #[error("failed to build airdrop transaction: {0}")]
    BuildTransaction(String),
    /// The faucet transaction could not be signed.
    #[error("failed to sign airdrop transaction: {0}")]
    SignTransaction(String),
    /// Locally retained transaction bytes are invalid hexadecimal; none were sent.
    #[error("invalid retained transaction encoding: {0}")]
    Encoding(hex::FromHexError),
    /// The signed faucet transaction was not accepted.
    #[error("failed to submit airdrop transaction: {0}")]
    Submit(String),
    /// The accepted faucet transaction could not be confirmed.
    #[error("failed to confirm airdrop transaction: {0}")]
    Poll(Box<ReceiptPollError>),
}

impl AirdropSubmitError {
    // Only called for the first send, never after a dispatched/uncertain attempt.
    fn proves_first_send_not_admitted(&self, expected_hash: TxHash) -> bool {
        use thruput_json_rpc_types::error::native::{AttemptAdmission, DecodedError, ErrorData};
        if matches!(
            self,
            Self::RpcConnect(_) | Self::Preflight(_) | Self::Encoding(_)
        ) {
            return true;
        }
        let Self::Rpc {
            method: "sendTransaction",
            source,
        } = self
        else {
            return false;
        };
        let NativeClientError::Application { data, .. } = source.as_ref() else {
            return false;
        };
        let DecodedError::Known { data, .. } = data.as_ref() else {
            return false;
        };
        let (ErrorData::TransactionInvalid {
            admission,
            transaction_hash: hash,
            ..
        }
        | ErrorData::TransactionRejected {
            admission,
            transaction_hash: hash,
            ..
        }
        | ErrorData::BackendUnavailable {
            admission: Some(admission),
            transaction_hash: hash,
            ..
        }
        | ErrorData::BackendTimeout {
            admission: Some(admission),
            transaction_hash: hash,
            ..
        }
        | ErrorData::Overloaded {
            admission: Some(admission),
            transaction_hash: hash,
            ..
        }
        | ErrorData::BackendDataInvalid {
            admission: Some(admission),
            transaction_hash: hash,
            ..
        }) = data.as_ref()
        else {
            return false;
        };
        matches!(
            admission,
            AttemptAdmission::NotAttempted | AttemptAdmission::Rejected
        ) && hash.is_none_or(|hash| hash == expected_hash)
    }

    fn into_response(self) -> Response {
        match self {
            Self::InvalidReceipt => error_response(
                StatusCode::BAD_GATEWAY,
                "airdrop_receipt_invalid",
                "faucet received an inconsistent transaction receipt",
            ),
            Self::TransactionFailed { .. } => error_response(
                StatusCode::BAD_GATEWAY,
                "airdrop_transaction_failed",
                "airdrop transaction did not commit successfully",
            ),
            _ => error_response(
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
    /// 32-byte faucet signing secret key.
    pub faucet_secret_key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for AirdropRuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AirdropRuntimeConfig")
            .field("raw_amount", &self.raw_amount)
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
    /// The native HOG transfer selector must be eight bytes.
    #[error("native HOG transfer entrypoint must be 8 bytes, got {0}")]
    InvalidTransferEntrypoint(usize),
    /// The signing key must control the advertised native faucet account.
    #[error("faucet wallet address does not match the signing key nonce-zero pubkey account")]
    FaucetAddressMismatch,
}

impl AirdropRuntimeConfig {
    /// Validate wire-format config values into runtime airdrop config.
    pub fn from_wire(
        raw_amount: &str,
        faucet_secret_key: Zeroizing<[u8; 32]>,
    ) -> Result<Self, AirdropRuntimeConfigError> {
        let raw_amount = raw_amount
            .parse::<u128>()
            .ok()
            .filter(|amount| *amount > 0)
            .ok_or(AirdropRuntimeConfigError::EmptyAmount)?;
        Ok(Self {
            raw_amount,
            faucet_secret_key,
        })
    }
}

pub struct RpcAirdropSubmitter {
    rpc_url: String,
    raw_amount: u128,
    network_hash: thruput_xdr::types::NetworkHash,
    faucet_account: PubkeyAccount,
    faucet_signer: LocalSigner,
    hog_token_address: thruput_xdr::types::ContractAddress,
    hog_transfer_entrypoint: ContractEntrypointID,
}

impl RpcAirdropSubmitter {
    /// Construct an RPC-backed airdrop submitter from public descriptor and private config.
    pub fn new(
        descriptor: &NetworkDescriptor,
        config: &AirdropRuntimeConfig,
        network_hash: thruput_xdr::types::NetworkHash,
    ) -> Result<Self, AirdropRuntimeConfigError> {
        let faucet_signer = LocalSigner::from_secret_key(*config.faucet_secret_key);
        let faucet_account =
            PubkeyAccount::from_public_key_bytes(faucet_signer.verifying_key_bytes(), 0);
        if faucet_account.address() != descriptor.faucet.wallet_address.0 {
            return Err(AirdropRuntimeConfigError::FaucetAddressMismatch);
        }
        Ok(Self {
            rpc_url: descriptor.endpoints.rpc_url.clone(),
            raw_amount: config.raw_amount,
            network_hash,
            faucet_account,
            faucet_signer,
            hog_token_address: descriptor.native_hog.token_address.0,
            hog_transfer_entrypoint: ContractEntrypointID::from(entrypoint_bytes(
                &descriptor.native_hog.transfer_entrypoint,
            )?),
        })
    }

    async fn client(
        &self,
    ) -> Result<NativeClient<thruput_json_rpc_client::HttpClient>, AirdropSubmitError> {
        let client = NativeClient::new(
            crate::telemetry::rpc_client(&self.rpc_url)
                .map_err(|error| AirdropSubmitError::RpcConnect(error.to_string()))?,
        );
        thruput_client::native::check_endpoint(
            &client,
            &[
                "getNetworkInfo",
                "getChainTip",
                "sendTransaction",
                "getTransactionReceipt",
                "getTransactionStatus",
            ],
        )
        .await
        .map_err(|error| AirdropSubmitError::Preflight(Box::new(error)))?;
        thruput_client::native::validate_network(&client, &self.network_hash)
            .await
            .map_err(|error| AirdropSubmitError::Preflight(Box::new(error)))?;
        Ok(client)
    }
}

#[async_trait::async_trait]
impl AirdropSubmitter for RpcAirdropSubmitter {
    fn raw_amount(&self) -> u128 {
        self.raw_amount
    }

    async fn prepare_airdrop(
        &self,
        recipient: ContractAddress,
    ) -> Result<PreparedAirdrop, AirdropSubmitError> {
        let client = self.client().await?;
        let block_expiry_height = thruput_client::native::resolve_block_expiry_height(
            &client,
            DEFAULT_BLOCK_EXPIRY_OFFSET,
        )
        .await
        .map_err(|error| AirdropSubmitError::Preflight(Box::new(error)))?;
        let transfer_calldata = encode_transfer_calldata(&recipient.0, self.raw_amount)?;
        let nonce = next_nonce()
            .map_err(|error| AirdropSubmitError::BuildTransaction(error.to_string()))?;
        let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
            account: &self.faucet_account,
            target_address: self.hog_token_address,
            target_entrypoint: self.hog_transfer_entrypoint,
            target_calldata: transfer_calldata.as_slice(),
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce,
        })
        .map_err(|error| AirdropSubmitError::BuildTransaction(error.to_string()))?;
        let tx_hash = TxHash(
            transaction_hash(&tx)
                .map_err(|error| AirdropSubmitError::BuildTransaction(error.to_string()))?
                .into(),
        );
        let tx_hex = sign_and_encode_transaction_hex(
            tx,
            &self.faucet_signer,
            SignTransactionParams::default(),
            &self.network_hash,
        )
        .map_err(|error| AirdropSubmitError::SignTransaction(error.to_string()))?;
        Ok(PreparedAirdrop {
            raw_amount: self.raw_amount.to_string(),
            tx_hash,
            signed_transaction: tx_hex,
            block_expiry_height,
        })
    }

    async fn submit_airdrop(
        &self,
        transaction: &PreparedAirdrop,
    ) -> Result<(), AirdropSubmitError> {
        let signed = &transaction.signed_transaction;
        let bytes = hex::decode(signed.strip_prefix("0x").unwrap_or(signed))
            .map_err(AirdropSubmitError::Encoding)?;
        let span = tracing::info_span!(
            target: "groundhog_otel", "jsonrpc.submit",
            otel.name = "sendTransaction", otel.kind = "client",
            rpc.system = "jsonrpc", rpc.method = "sendTransaction"
        );
        let actual = async {
            // Validate and submit on the same traced transport, exactly once.
            let client = self.client().await?;
            known(
                "sendTransaction",
                client
                    .send_transaction(requests::SendTransactionRequest {
                        signed_transaction: HexBytes(bytes),
                    })
                    .await,
            )
        }
        .instrument(span)
        .await?;
        if actual.transaction_hash != transaction.tx_hash {
            return Err(AirdropSubmitError::Submit(
                "returned transaction hash differs from retained transaction".into(),
            ));
        }
        Ok(())
    }

    async fn poll_airdrop(
        &self,
        tx_hash: TxHash,
        committed_height: Option<i64>,
    ) -> Result<TransactionReceipt, AirdropSubmitError> {
        let client = self.client().await?;
        poll_receipt(
            &client,
            self.network_hash.clone().into_inner().into(),
            tx_hash,
            committed_height.map(|height| {
                thruput_json_rpc_client::native::receipt::KnownCommitment {
                    network_hash: self.network_hash.clone().into_inner().into(),
                    transaction_hash: tx_hash,
                    block_height: height.into(),
                }
            }),
            ReceiptPollConfig::default(),
            std::future::pending(),
        )
        .await
        .map_err(|error| AirdropSubmitError::Poll(Box::new(error)))
    }
}

fn known<T>(
    method: &'static str,
    result: Result<DecodedResponse<T>, NativeClientError>,
) -> Result<T, AirdropSubmitError> {
    match result {
        Ok(DecodedResponse::Known(value)) => Ok(value),
        Ok(DecodedResponse::Unsupported { raw }) => {
            Err(AirdropSubmitError::Unsupported { method, raw })
        }
        Ok(_) => Err(AirdropSubmitError::Unsupported {
            method,
            raw: serde_json::Value::Null,
        }),
        Err(source) => Err(AirdropSubmitError::Rpc {
            method,
            source: Box::new(source),
        }),
    }
}

fn entrypoint_bytes(entrypoint: &HexBytes) -> Result<[u8; 8], AirdropRuntimeConfigError> {
    let len = entrypoint.0.len();
    entrypoint
        .0
        .as_slice()
        .try_into()
        .map_err(|_| AirdropRuntimeConfigError::InvalidTransferEntrypoint(len))
}

fn encode_transfer_calldata(
    recipient: &thruput_xdr::types::ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, AirdropSubmitError> {
    thruput_signing::native_hog_transfer_calldata(recipient, raw_amount)
        .map_err(|error| AirdropSubmitError::EncodeTransferCalldata(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use serde_json::json;
    use zeroize::Zeroizing;

    use thruput_json_rpc_types::airdrop::HostedAirdropRequest;

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
        let config = AirdropRuntimeConfig::from_wire("100000000", Zeroizing::new([0xAB; 32]))
            .expect("bug: runtime config should validate");

        let debug = format!("{config:?}");

        assert!(debug.contains("faucet_secret_key: <redacted>"), "{debug}");
        assert!(!debug.contains("171"), "{debug}");
    }

    #[test]
    fn airdrop_request_accepts_future_optional_fields() {
        let request: HostedAirdropRequest = serde_json::from_value(json!({
            "recipient": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x11; 32])),
            "clientNonce": "0x99",
            "networkId": "future-network",
            "networkHash": thruput_json_rpc_types::NetworkHashText::from([0x33; 32]),
            "genesisHash": thruput_json_rpc_types::SHA256Hash::from(thruput_xdr::types::SHA256Hash::from([0x22; 32])),
            "futureField": true,
        }))
        .expect("bug: public API requests should ignore unknown future fields");

        assert_eq!(request.recipient.0.as_ref(), &[0x11; 32]);
    }

    #[test]
    fn airdrop_request_accepts_32_byte_client_nonce() {
        let request: HostedAirdropRequest = serde_json::from_value(json!({
            "recipient": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x11; 32])),
            "clientNonce": format!("0x{}", "99".repeat(32)),
            "networkId": "future-network",
            "networkHash": thruput_json_rpc_types::NetworkHashText::from([0x33; 32]),
            "genesisHash": thruput_json_rpc_types::SHA256Hash::from(thruput_xdr::types::SHA256Hash::from([0x22; 32])),
        }))
        .expect("bug: 32-byte client nonce should be valid");

        assert_eq!(request.client_nonce.as_bytes().len(), 32);
    }
}
