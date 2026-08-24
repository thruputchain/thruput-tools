use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use groundhog_json_rpc_client::{
    GroundhogApiClient, HttpClientBuilder, poll_transaction_receipt, submit_transaction,
};
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
    WalletCreateProofV2, build_deploy_transaction, create_wallet_calldata, derive_deploy_address,
    dev_wallet_contract, sign_and_encode_transaction_hex, source_address_auth_claim,
    wallet_create_proof_message_v2,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{Ed25519PublicKey, SHA256Hash};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use xdr_common::VecM;

use crate::{
    airdrop::{AirdropSubmitError, AirdropSubmitter},
    config::PolicyRuntimeConfig,
    error::{error_response, error_response_with_retry_after},
    policy::{
        FaucetPolicyStore, InsertWalletCreateAttemptResult, PolicyStoreError, QuotaCheck, QuotaKey,
        WalletCreateAttempt, WalletCreateAttemptKey, WalletCreateAttemptStatus, client_identity,
    },
    utils::next_nonce,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const WALLET_CREATE_QUOTA_EXCEEDED: &str = "wallet_create_quota_exceeded";

struct ResolveWalletCreateAttempt<'a> {
    headers: &'a HeaderMap,
    peer: Option<SocketAddr>,
    request: &'a CreateWalletRequest,
    key: &'a WalletCreateAttemptKey,
    prepared: &'a PreparedWalletCreate,
    existing: Option<WalletCreateAttempt>,
    now: SystemTime,
}

enum WalletCreateFlow {
    Continue(Box<WalletCreateAttempt>),
    Respond(Response),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateWalletRequest {
    owner_public_key: ApiEd25519PublicKey,
    deploy_nonce: ApiSha256Hash,
    client_nonce: HexBytes,
    owner_signature: HexBytes,
    network_id: String,
    genesis_hash: ApiSha256Hash,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateWalletResponse {
    owner_public_key: ApiEd25519PublicKey,
    deploy_nonce: ApiSha256Hash,
    client_nonce: HexBytes,
    attempt_status: &'static str,
    wallet_address: ApiContractAddress,
    prefund_raw_amount: String,
    wallet_contract_hash: ApiSha256Hash,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefund_tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<BlockHeight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<ExecStatus>,
}

pub(crate) async fn create_wallet(
    State(service): State<Arc<WalletCreateService>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<CreateWalletRequest>,
) -> Response {
    service.create_wallet(headers, Some(peer), request).await
}

pub struct WalletCreateService {
    network_descriptor: NetworkDescriptor,
    prefund_submitter: Arc<dyn AirdropSubmitter>,
    wallet_create_submitter: Arc<dyn WalletCreateSubmitter>,
    policy_store: Arc<dyn FaucetPolicyStore>,
    policy_config: PolicyRuntimeConfig,
}

impl WalletCreateService {
    pub fn new(
        network_descriptor: NetworkDescriptor,
        prefund_submitter: Arc<dyn AirdropSubmitter>,
        wallet_create_submitter: Arc<dyn WalletCreateSubmitter>,
        policy_store: Arc<dyn FaucetPolicyStore>,
        policy_config: PolicyRuntimeConfig,
    ) -> Self {
        Self {
            network_descriptor,
            prefund_submitter,
            wallet_create_submitter,
            policy_store,
            policy_config,
        }
    }

    async fn create_wallet(
        &self,
        headers: HeaderMap,
        peer: Option<SocketAddr>,
        request: CreateWalletRequest,
    ) -> Response {
        if request.network_id != self.network_descriptor.network_id
            || request.genesis_hash.0 != self.network_descriptor.genesis_hash.0
        {
            return error_response(
                StatusCode::BAD_REQUEST,
                "wallet_create_network_mismatch",
                "wallet-create request does not match this faucet network",
            );
        }
        if request.client_nonce.0.is_empty() {
            return error_response(
                StatusCode::BAD_REQUEST,
                "wallet_create_invalid_client_nonce",
                "clientNonce must not be empty",
            );
        }

        let prepared = match self.wallet_create_submitter.verify_wallet_create_request(
            request.owner_public_key.0,
            request.deploy_nonce.0,
            &request.client_nonce.0,
            &request.network_id,
            request.genesis_hash.0,
            &request.owner_signature.0,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return error.into_response(),
        };
        let key = WalletCreateAttemptKey {
            network_id: request.network_id.clone(),
            genesis_hash: hex_string(request.genesis_hash.0),
            owner_public_key: hex_string(request.owner_public_key.0),
            client_nonce: hex_encode(&request.client_nonce.0),
        };
        let now = SystemTime::now();
        if let Err(error) = self.policy_store.expire(now) {
            return policy_store_error_response(&error);
        }
        let existing = match self.policy_store.get_wallet_create_attempt(&key) {
            Ok(existing) => existing,
            Err(error) => return policy_store_error_response(&error),
        };

        let mut attempt = match self.resolve_wallet_create_attempt(ResolveWalletCreateAttempt {
            headers: &headers,
            peer,
            request: &request,
            key: &key,
            prepared: &prepared,
            existing,
            now,
        }) {
            WalletCreateFlow::Continue(attempt) => *attempt,
            WalletCreateFlow::Respond(response) => return response,
        };

        if attempt.create_tx_hash.is_some() {
            return self.poll_wallet_create_attempt(request, attempt).await;
        }
        if attempt.prefund_height.is_none()
            && let Some(response) = self
                .prefund_wallet_create_attempt(&key, &prepared, &mut attempt)
                .await
        {
            return response;
        }
        self.submit_wallet_create_attempt(request, key, attempt)
            .await
    }

    fn resolve_wallet_create_attempt(
        &self,
        input: ResolveWalletCreateAttempt<'_>,
    ) -> WalletCreateFlow {
        if let Some(attempt) = input.existing {
            return existing_wallet_create_attempt(input.request, attempt);
        }
        if let Some(response) = self.check_wallet_create_quota(
            input.headers,
            input.peer,
            input.request.owner_public_key.0,
            input.now,
        ) {
            return WalletCreateFlow::Respond(response);
        }
        let attempt = new_wallet_create_attempt(
            input.key.clone(),
            input.request,
            input.prepared,
            input.now,
            self.policy_config
                .wallet_create
                .unprefunded_attempt_ttl_secs,
        );
        match self.policy_store.insert_wallet_create_attempt_if_absent(
            attempt.clone(),
            self.policy_config.wallet_create.max_pending_per_owner,
        ) {
            Ok(InsertWalletCreateAttemptResult::Existing(attempt)) => {
                existing_wallet_create_attempt(input.request, *attempt)
            }
            Ok(InsertWalletCreateAttemptResult::Inserted) => {
                WalletCreateFlow::Continue(Box::new(attempt))
            }
            Ok(InsertWalletCreateAttemptResult::PendingLimitReached) => {
                WalletCreateFlow::Respond(error_response_with_retry_after(
                    StatusCode::TOO_MANY_REQUESTS,
                    "wallet_create_owner_pending_limit_exceeded",
                    "owner already has a wallet-create attempt in progress",
                    Some(5),
                ))
            }
            Err(error) => WalletCreateFlow::Respond(policy_store_error_response(&error)),
        }
    }

    fn check_wallet_create_quota(
        &self,
        headers: &HeaderMap,
        peer: Option<SocketAddr>,
        owner_public_key: Ed25519PublicKey,
        now: SystemTime,
    ) -> Option<Response> {
        let identity = client_identity(
            headers,
            peer,
            self.policy_config.trust_proxy_client_ip_headers,
        );
        match self.policy_store.check_and_record_quotas(
            &[
                QuotaCheck {
                    key: QuotaKey::WalletCreateOwner(hex_string(owner_public_key)),
                    max: self.policy_config.wallet_create.owner_quota_max,
                    window: Duration::from_secs(
                        self.policy_config.wallet_create.owner_quota_window_secs,
                    ),
                },
                QuotaCheck {
                    key: QuotaKey::WalletCreateClient(identity),
                    max: self.policy_config.wallet_create.ip_quota_max,
                    window: Duration::from_secs(
                        self.policy_config.wallet_create.ip_quota_window_secs,
                    ),
                },
            ],
            now,
        ) {
            Ok(decision) if decision.allowed => None,
            Ok(decision) => Some(error_response_with_retry_after(
                StatusCode::TOO_MANY_REQUESTS,
                WALLET_CREATE_QUOTA_EXCEEDED,
                "wallet-create quota exceeded",
                decision.retry_after.map(|duration| duration.as_secs()),
            )),
            Err(error) => Some(policy_store_error_response(&error)),
        }
    }

    async fn prefund_wallet_create_attempt(
        &self,
        key: &WalletCreateAttemptKey,
        prepared: &PreparedWalletCreate,
        attempt: &mut WalletCreateAttempt,
    ) -> Option<Response> {
        if attempt.prefund_tx_hash.is_some() {
            return self.poll_prefund_wallet_create_attempt(attempt).await;
        }
        if let Some(response) = self.claim_wallet_create_transition(
            key,
            &[
                WalletCreateAttemptStatus::Prepared,
                WalletCreateAttemptStatus::FailedRetryable,
            ],
            WalletCreateAttemptStatus::PrefundPending,
            attempt,
        ) {
            return Some(response);
        }

        let prefund = match self
            .prefund_submitter
            .submit_airdrop(prepared.wallet_address)
            .await
        {
            Ok(prefund) => prefund,
            Err(error) => {
                attempt.status = WalletCreateAttemptStatus::FailedRetryable;
                attempt.error_code = Some(airdrop_error_code(&error).into());
                attempt.updated_at = SystemTime::now();
                let _ = self.policy_store.put_wallet_create_attempt(attempt.clone());
                return Some(prefund_error_response(&error));
            }
        };
        attempt.prefund_tx_hash = Some(tx_hash_string(&prefund.tx_hash));
        // Keep accepted transactions until a receipt makes the transition terminal.
        // Expiring this state could cause the prefund to be submitted twice.
        attempt.expires_at = None;
        attempt.updated_at = SystemTime::now();
        if let Some(response) = self.put_wallet_create_attempt(attempt) {
            return Some(response);
        }
        self.poll_prefund_wallet_create_attempt(attempt).await
    }

    async fn poll_prefund_wallet_create_attempt(
        &self,
        attempt: &mut WalletCreateAttempt,
    ) -> Option<Response> {
        let Some(tx_hash) = attempt.prefund_tx_hash.as_deref().and_then(parse_tx_hash) else {
            return Some(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wallet_create_attempt_state_invalid",
                "wallet-create attempt has an invalid prefund transaction hash",
            ));
        };
        match self.prefund_submitter.poll_airdrop(tx_hash).await {
            Ok(receipt) if matches!(receipt.status, ExecStatus::Ok) => {
                attempt.prefund_height = Some(*receipt.height);
                attempt.expires_at = SystemTime::now().checked_add(Duration::from_secs(
                    self.policy_config
                        .wallet_create
                        .prefunded_attempt_retention_secs,
                ));
                attempt.status = WalletCreateAttemptStatus::PrefundConfirmed;
                attempt.error_code = None;
                attempt.updated_at = SystemTime::now();
                self.put_wallet_create_attempt(attempt)
            }
            Ok(receipt) => {
                attempt.prefund_height = Some(*receipt.height);
                attempt.status = WalletCreateAttemptStatus::FailedTerminal;
                attempt.error_code = Some("wallet_create_prefund_transaction_failed".into());
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = now.checked_add(Duration::from_secs(
                    self.policy_config
                        .wallet_create
                        .prefunded_attempt_retention_secs,
                ));
                if let Some(response) = self.put_wallet_create_attempt(attempt) {
                    return Some(response);
                }
                Some(prefund_error_response(
                    &AirdropSubmitError::TransactionFailed { tx_hash },
                ))
            }
            Err(error) => {
                attempt.error_code = Some(airdrop_error_code(&error).into());
                attempt.updated_at = SystemTime::now();
                if let Some(response) = self.put_wallet_create_attempt(attempt) {
                    return Some(response);
                }
                Some(prefund_error_response(&error))
            }
        }
    }

    async fn submit_wallet_create_attempt(
        &self,
        request: CreateWalletRequest,
        key: WalletCreateAttemptKey,
        mut attempt: WalletCreateAttempt,
    ) -> Response {
        if attempt.create_tx_hash.is_some() {
            return self.poll_wallet_create_attempt(request, attempt).await;
        }
        if let Some(response) = self.claim_wallet_create_transition(
            &key,
            &[
                WalletCreateAttemptStatus::PrefundConfirmed,
                WalletCreateAttemptStatus::FailedRetryable,
            ],
            WalletCreateAttemptStatus::CreatePending,
            &mut attempt,
        ) {
            return response;
        }
        match self
            .wallet_create_submitter
            .create_wallet(
                request.owner_public_key.0,
                request.deploy_nonce.0,
                request.client_nonce.0.clone(),
                request.network_id.clone(),
                request.genesis_hash.0,
                request.owner_signature.0.clone(),
            )
            .await
        {
            Ok(accepted) => {
                attempt.create_tx_hash = Some(tx_hash_string(&accepted.tx_hash));
                // Keep the accepted hash until its receipt is resolved so retries poll
                // instead of submitting a second wallet-create transaction.
                attempt.expires_at = None;
                attempt.updated_at = SystemTime::now();
                if let Some(response) = self.put_wallet_create_attempt(&attempt) {
                    return response;
                }
                self.poll_wallet_create_attempt(request, attempt).await
            }
            Err(error) => {
                attempt.status = WalletCreateAttemptStatus::FailedRetryable;
                attempt.error_code = Some(wallet_create_error_code(&error).into());
                attempt.updated_at = SystemTime::now();
                let _ = self.policy_store.put_wallet_create_attempt(attempt);
                error.into_response()
            }
        }
    }

    async fn poll_wallet_create_attempt(
        &self,
        request: CreateWalletRequest,
        mut attempt: WalletCreateAttempt,
    ) -> Response {
        let Some(tx_hash) = attempt.create_tx_hash.as_deref().and_then(parse_tx_hash) else {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wallet_create_attempt_state_invalid",
                "wallet-create attempt has an invalid accepted transaction hash",
            );
        };
        match self
            .wallet_create_submitter
            .poll_wallet_create(tx_hash)
            .await
        {
            Ok(receipt) if matches!(receipt.status, ExecStatus::Ok) => {
                attempt.status = WalletCreateAttemptStatus::Created;
                attempt.create_height = Some(*receipt.height);
                attempt.error_code = None;
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = now.checked_add(Duration::from_secs(
                    self.policy_config
                        .wallet_create
                        .prefunded_attempt_retention_secs,
                ));
                if let Some(response) = self.put_wallet_create_attempt(&attempt) {
                    return response;
                }
                response_from_attempt(StatusCode::OK, request, attempt)
            }
            Ok(receipt) => {
                attempt.status = WalletCreateAttemptStatus::FailedTerminal;
                attempt.create_height = Some(*receipt.height);
                attempt.error_code = Some("wallet_create_transaction_failed".into());
                let now = SystemTime::now();
                attempt.updated_at = now;
                attempt.expires_at = now.checked_add(Duration::from_secs(
                    self.policy_config
                        .wallet_create
                        .prefunded_attempt_retention_secs,
                ));
                if let Some(response) = self.put_wallet_create_attempt(&attempt) {
                    return response;
                }
                WalletCreateError::TransactionFailed { tx_hash }.into_response()
            }
            Err(error) => {
                attempt.error_code = Some(wallet_create_error_code(&error).into());
                attempt.updated_at = SystemTime::now();
                if let Some(response) = self.put_wallet_create_attempt(&attempt) {
                    return response;
                }
                error.into_response()
            }
        }
    }

    fn claim_wallet_create_transition(
        &self,
        key: &WalletCreateAttemptKey,
        expected: &[WalletCreateAttemptStatus],
        next: WalletCreateAttemptStatus,
        attempt: &mut WalletCreateAttempt,
    ) -> Option<Response> {
        if let Err(error) =
            self.policy_store
                .claim_wallet_create_transition(key, expected, next, SystemTime::now())
        {
            return Some(policy_store_error_response(&error));
        }
        attempt.status = next;
        attempt.updated_at = SystemTime::now();
        self.put_wallet_create_attempt(attempt)
    }

    fn put_wallet_create_attempt(&self, attempt: &WalletCreateAttempt) -> Option<Response> {
        self.policy_store
            .put_wallet_create_attempt(attempt.clone())
            .err()
            .map(|error| policy_store_error_response(&error))
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

    /// Validate the hardened wallet-create request and derive prepared metadata.
    fn verify_wallet_create_request(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
        client_nonce: &[u8],
        network_id: &str,
        genesis_hash: SHA256Hash,
        owner_signature: &[u8],
    ) -> Result<PreparedWalletCreate, WalletCreateError>;

    /// Submit a sponsored `create_wallet` transaction and return once it is accepted.
    async fn create_wallet(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
        client_nonce: Vec<u8>,
        network_id: String,
        genesis_hash: SHA256Hash,
        owner_signature: Vec<u8>,
    ) -> Result<AcceptedWalletCreate, WalletCreateError>;

    /// Poll a previously accepted wallet-create transaction.
    async fn poll_wallet_create(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionReceipt, WalletCreateError>;
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

/// Faucet wallet-create transaction accepted by the submission endpoint.
#[derive(Debug, Clone)]
pub struct AcceptedWalletCreate {
    /// Created wallet address.
    pub wallet_address: ApiContractAddress,
    /// Submitted transaction hash.
    pub tx_hash: TxHash,
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
    /// The request was signed for a different chain genesis.
    #[error("wallet-create request genesis does not match faucet genesis")]
    GenesisHashMismatch,
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
    /// The signed wallet-create transaction was not accepted.
    #[error("failed to submit wallet-create transaction: {0}")]
    Submit(#[from] groundhog_json_rpc_client::SubmitTransactionError),
    /// The accepted wallet-create transaction could not be confirmed.
    #[error("failed to confirm wallet-create transaction: {0}")]
    Poll(#[from] groundhog_json_rpc_client::PollTransactionReceiptError),
}

impl WalletCreateError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidOwnerSignature | Self::OwnerSignatureMismatch => error_response(
                StatusCode::UNAUTHORIZED,
                "wallet_create_owner_proof_invalid",
                "wallet-create owner proof is invalid",
            ),
            Self::GenesisHashMismatch => error_response(
                StatusCode::BAD_REQUEST,
                "wallet_create_network_mismatch",
                "wallet-create request does not match this faucet network",
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
            | Self::Submit(_)
            | Self::Poll(_) => error_response(
                StatusCode::BAD_GATEWAY,
                "wallet_create_submit_failed",
                "failed to submit wallet-create transaction",
            ),
        }
    }
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
    network_hash: groundhog_xdr::types::NetworkHash,
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
        faucet_secret_key: &[u8; 32],
        network_hash: groundhog_xdr::types::NetworkHash,
    ) -> Self {
        Self {
            rpc_url: descriptor.endpoints.rpc_url.clone(),
            network_hash,
            faucet_wallet_address: descriptor.faucet.wallet_address.0,
            faucet_signer: LocalSigner::from_secret_key(*faucet_secret_key),
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
        client_nonce: &[u8],
        network_id: &str,
        genesis_hash: SHA256Hash,
        wallet_address: groundhog_xdr::types::ContractAddress,
    ) -> Result<Vec<u8>, WalletCreateError> {
        wallet_create_proof_message_v2(&WalletCreateProofV2 {
            network_id,
            genesis_hash,
            faucet_wallet_address: self.faucet_wallet_address,
            wallet_contract_hash: self.wallet_contract_hash,
            owner_public_key,
            deploy_nonce,
            wallet_address,
            client_nonce,
        })
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

    fn verify_wallet_create_request(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
        client_nonce: &[u8],
        network_id: &str,
        genesis_hash: SHA256Hash,
        owner_signature: &[u8],
    ) -> Result<PreparedWalletCreate, WalletCreateError> {
        let prepared = self.prepare_wallet_create(owner_public_key, deploy_nonce)?;
        if genesis_hash != self.genesis_hash {
            return Err(WalletCreateError::GenesisHashMismatch);
        }
        let message = self.proof_message(
            owner_public_key,
            deploy_nonce,
            client_nonce,
            network_id,
            genesis_hash,
            prepared.wallet_address.0,
        )?;
        verify_owner_signature(owner_public_key, &message, owner_signature)?;
        Ok(prepared)
    }

    async fn create_wallet(
        &self,
        owner_public_key: Ed25519PublicKey,
        deploy_nonce: SHA256Hash,
        client_nonce: Vec<u8>,
        network_id: String,
        genesis_hash: SHA256Hash,
        owner_signature: Vec<u8>,
    ) -> Result<AcceptedWalletCreate, WalletCreateError> {
        let prepared = self.verify_wallet_create_request(
            owner_public_key,
            deploy_nonce,
            &client_nonce,
            &network_id,
            genesis_hash,
            &owner_signature,
        )?;

        let artifact_bytes = tokio::fs::read(&self.wallet_artifact_path)
            .await
            .map_err(|error| WalletCreateError::LoadWalletArtifact(error.to_string()))?;
        let contract = dev_wallet_contract(&self.wallet_artifact_path, artifact_bytes)
            .map_err(|error| WalletCreateError::LoadWalletArtifact(error.to_string()))?;
        let block_expiry_height = self.block_expiry_height().await?;
        let create_calldata =
            create_wallet_calldata(&deploy_nonce, &self.wallet_contract_hash, owner_public_key)
                .map_err(|error| WalletCreateError::EncodeCreateCalldata(error.to_string()))?;
        let tx = build_deploy_transaction(BuildDeployTransaction {
            invoked_address: self.faucet_wallet_address,
            entrypoint: self.create_wallet_entrypoint,
            calldata: create_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce: next_nonce(),
            contracts_to_deploy: VecM(vec![contract]),
            auth_claim: source_address_auth_claim(),
        });
        let tx_hex = sign_and_encode_transaction_hex(
            tx,
            &self.faucet_signer,
            SignTransactionParams::default(),
            &self.network_hash,
        )
        .map_err(|error| WalletCreateError::SignTransaction(error.to_string()))?;
        let tx_hash = submit_transaction(&self.rpc_url, &tx_hex).await?;
        Ok(AcceptedWalletCreate {
            wallet_address: prepared.wallet_address,
            tx_hash,
        })
    }

    async fn poll_wallet_create(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionReceipt, WalletCreateError> {
        poll_transaction_receipt(&self.rpc_url, tx_hash)
            .await
            .map_err(Into::into)
    }
}

fn new_wallet_create_attempt(
    key: WalletCreateAttemptKey,
    request: &CreateWalletRequest,
    prepared: &PreparedWalletCreate,
    now: SystemTime,
    unprefunded_attempt_ttl_secs: u64,
) -> WalletCreateAttempt {
    WalletCreateAttempt {
        key,
        deploy_nonce: hex_string(request.deploy_nonce.0),
        wallet_address: hex_string(prepared.wallet_address.0),
        wallet_contract_hash: hex_string(prepared.wallet_contract_hash.0),
        prefund_raw_amount: prepared.prefund_raw_amount.clone(),
        status: WalletCreateAttemptStatus::Prepared,
        prefund_tx_hash: None,
        prefund_height: None,
        create_tx_hash: None,
        create_height: None,
        error_code: None,
        created_at: now,
        updated_at: now,
        expires_at: now.checked_add(Duration::from_secs(unprefunded_attempt_ttl_secs)),
    }
}

fn existing_wallet_create_attempt(
    request: &CreateWalletRequest,
    mut attempt: WalletCreateAttempt,
) -> WalletCreateFlow {
    if attempt.deploy_nonce != hex_string(request.deploy_nonce.0) {
        return WalletCreateFlow::Respond(error_response(
            StatusCode::CONFLICT,
            "wallet_create_idempotency_conflict",
            "wallet-create idempotency key was reused with different request fields",
        ));
    }
    match attempt.status {
        WalletCreateAttemptStatus::Created => WalletCreateFlow::Respond(response_from_attempt(
            StatusCode::OK,
            request.clone(),
            attempt,
        )),
        WalletCreateAttemptStatus::Prepared => WalletCreateFlow::Respond(response_from_attempt(
            StatusCode::ACCEPTED,
            request.clone(),
            attempt,
        )),
        WalletCreateAttemptStatus::PrefundPending if attempt.prefund_tx_hash.is_some() => {
            WalletCreateFlow::Continue(Box::new(attempt))
        }
        WalletCreateAttemptStatus::PrefundConfirmed => {
            WalletCreateFlow::Continue(Box::new(attempt))
        }
        WalletCreateAttemptStatus::CreatePending if attempt.create_tx_hash.is_some() => {
            WalletCreateFlow::Continue(Box::new(attempt))
        }
        WalletCreateAttemptStatus::PrefundPending | WalletCreateAttemptStatus::CreatePending => {
            WalletCreateFlow::Respond(response_from_attempt(
                StatusCode::ACCEPTED,
                request.clone(),
                attempt,
            ))
        }
        WalletCreateAttemptStatus::FailedTerminal | WalletCreateAttemptStatus::Expired => {
            WalletCreateFlow::Respond(error_response(
                StatusCode::CONFLICT,
                "wallet_create_attempt_terminal",
                "wallet-create attempt cannot be retried",
            ))
        }
        WalletCreateAttemptStatus::FailedRetryable => {
            attempt.error_code = None;
            WalletCreateFlow::Continue(Box::new(attempt))
        }
    }
}

fn response_from_attempt(
    status: StatusCode,
    request: CreateWalletRequest,
    attempt: WalletCreateAttempt,
) -> Response {
    (
        status,
        Json(CreateWalletResponse {
            owner_public_key: request.owner_public_key,
            deploy_nonce: request.deploy_nonce,
            client_nonce: request.client_nonce,
            attempt_status: wallet_create_status_name(attempt.status),
            wallet_address: Hex0x(parse_32_hex(&attempt.wallet_address)),
            prefund_raw_amount: attempt.prefund_raw_amount,
            wallet_contract_hash: Hex0x(parse_32_hex(&attempt.wallet_contract_hash)),
            prefund_tx_hash: attempt.prefund_tx_hash,
            tx_hash: attempt.create_tx_hash,
            height: attempt.create_height.map(BlockHeight::from),
            status: (attempt.status == WalletCreateAttemptStatus::Created)
                .then_some(ExecStatus::Ok),
        }),
    )
        .into_response()
}

fn wallet_create_status_name(status: WalletCreateAttemptStatus) -> &'static str {
    match status {
        WalletCreateAttemptStatus::Prepared => "prepared",
        WalletCreateAttemptStatus::PrefundPending => "prefundPending",
        WalletCreateAttemptStatus::PrefundConfirmed => "prefundConfirmed",
        WalletCreateAttemptStatus::CreatePending => "createPending",
        WalletCreateAttemptStatus::Created => "created",
        WalletCreateAttemptStatus::FailedRetryable => "failedRetryable",
        WalletCreateAttemptStatus::FailedTerminal => "failedTerminal",
        WalletCreateAttemptStatus::Expired => "expired",
    }
}

fn policy_store_error_response(error: &PolicyStoreError) -> Response {
    match error {
        PolicyStoreError::InvalidWalletCreateTransition { .. } => error_response(
            StatusCode::ACCEPTED,
            "wallet_create_already_pending",
            "wallet-create attempt is already in progress",
        ),
        PolicyStoreError::AttemptNotFound | PolicyStoreError::InvalidAirdropTransition { .. } => {
            error_response(
                StatusCode::CONFLICT,
                "wallet_create_attempt_conflict",
                "wallet-create attempt state changed while processing",
            )
        }
        PolicyStoreError::LockPoisoned => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "faucet_policy_store_unavailable",
            "faucet policy store is unavailable",
        ),
    }
}

fn prefund_error_response(error: &AirdropSubmitError) -> Response {
    match error {
        AirdropSubmitError::TransactionFailed { .. } => error_response(
            StatusCode::BAD_GATEWAY,
            "wallet_create_prefund_transaction_failed",
            "wallet-create prefund transaction did not commit successfully",
        ),
        AirdropSubmitError::RpcConnect(_)
        | AirdropSubmitError::ChainTip(_)
        | AirdropSubmitError::EncodeTransferCalldata(_)
        | AirdropSubmitError::BuildTransaction(_)
        | AirdropSubmitError::SignTransaction(_)
        | AirdropSubmitError::Submit(_)
        | AirdropSubmitError::Poll(_) => error_response(
            StatusCode::BAD_GATEWAY,
            "wallet_create_prefund_failed",
            "failed to prefund wallet-create address",
        ),
    }
}

fn airdrop_error_code(error: &AirdropSubmitError) -> &'static str {
    match error {
        AirdropSubmitError::TransactionFailed { .. } => "wallet_create_prefund_transaction_failed",
        AirdropSubmitError::RpcConnect(_)
        | AirdropSubmitError::ChainTip(_)
        | AirdropSubmitError::EncodeTransferCalldata(_)
        | AirdropSubmitError::BuildTransaction(_)
        | AirdropSubmitError::SignTransaction(_)
        | AirdropSubmitError::Submit(_)
        | AirdropSubmitError::Poll(_) => "wallet_create_prefund_failed",
    }
}

fn wallet_create_error_code(error: &WalletCreateError) -> &'static str {
    match error {
        WalletCreateError::InvalidOwnerSignature | WalletCreateError::OwnerSignatureMismatch => {
            "wallet_create_owner_proof_invalid"
        }
        WalletCreateError::GenesisHashMismatch => "wallet_create_network_mismatch",
        WalletCreateError::TransactionFailed { .. } => "wallet_create_transaction_failed",
        WalletCreateError::DeriveAddress(_)
        | WalletCreateError::ChainTip(_)
        | WalletCreateError::RpcConnect(_)
        | WalletCreateError::EncodeCreateCalldata(_)
        | WalletCreateError::LoadWalletArtifact(_)
        | WalletCreateError::BuildTransaction(_)
        | WalletCreateError::SignTransaction(_)
        | WalletCreateError::Submit(_)
        | WalletCreateError::Poll(_) => "wallet_create_submit_failed",
    }
}

fn hex_string(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex_encode(bytes.as_ref()))
}

fn tx_hash_string(tx_hash: &TxHash) -> String {
    hex_string(tx_hash.0.0)
}

fn parse_tx_hash(value: &str) -> Option<TxHash> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes: [u8; 32] = hex_decode(value).ok()?.try_into().ok()?;
    Some(TxHash(ApiSha256Hash::from(SHA256Hash::from(bytes))))
}

fn parse_32_hex<T>(value: &str) -> T
where
    T: From<[u8; 32]>,
{
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes = hex_decode(value).expect("bug: policy store hex should decode");
    let bytes: [u8; 32] = bytes
        .try_into()
        .expect("bug: policy store fixed hex should be 32 bytes");
    T::from(bytes)
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

fn hex_decode(value: &str) -> Result<Vec<u8>, ()> {
    if !value.len().is_multiple_of(2) {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
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

#[cfg(test)]
mod tests {
    use groundhog_signing::{LocalSigner, Signer, wallet_create_proof_message};
    use serde_json::json;

    use super::*;

    #[test]
    fn create_wallet_request_accepts_future_optional_fields() {
        let request: CreateWalletRequest = serde_json::from_value(json!({
            "ownerPublicKey": format!("0x{}", "11".repeat(32)),
            "deployNonce": format!("0x{}", "22".repeat(32)),
            "ownerSignature": format!("0x{}", "33".repeat(64)),
            "clientNonce": "0x99",
            "networkId": "future-network",
            "genesisHash": format!("0x{}", "44".repeat(32)),
        }))
        .expect("bug: public API requests should ignore unknown future fields");

        assert_eq!(request.owner_public_key.0.as_ref(), &[0x11; 32]);
        assert_eq!(request.deploy_nonce.0.as_ref(), &[0x22; 32]);
        assert_eq!(request.owner_signature.0.as_slice(), &[0x33; 64]);
    }

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
