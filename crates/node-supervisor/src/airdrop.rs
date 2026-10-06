use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use groundhog_json_rpc_api::AirdropApiServer;
use groundhog_json_rpc_server::{
    ChainPlane, ReadPlane, RpcRequestContext, RpcServerError, SubmitPlane,
    tx::validate_raw_transaction_bytes,
};
use groundhog_json_rpc_types::{
    ContractAddress,
    airdrop::{AirdropAttemptResponse, AirdropRequest},
    hex::Hex0x,
    receipt::ExecStatus,
    runtime_manifest::{BOOTSTRAP_ACCOUNT_ROLE_FAUCET, RuntimeManifest},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_signing::{
    DEFAULT_TX_GAS_LIMIT, LocalSigner, PubkeyAccount, SignTransactionParams,
    derive_dev_account_signer, encode_signed_transaction_xdr, sign_transaction, transaction_hash,
};
use groundhog_tx_validation::NetworkConfigParams;
use jsonrpsee::core::RpcResult;
use tokio::sync::Semaphore;

use crate::{
    LocalAirdropConfig, NodeSupervisorConfig, block_driver::BlockDriverTrigger,
    request_deadline::within_request_deadline,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const LOCAL_AIRDROP_RETRY_AFTER_SECONDS: u64 = 1;

#[derive(Clone)]
pub(crate) struct LocalAirdropContext {
    network_hash: groundhog_xdr::types::NetworkHash,
    signer: Arc<LocalSigner>,
    config: LocalAirdropConfig,
}

impl LocalAirdropContext {
    pub(crate) fn from_validated_runtime(
        manifest: &RuntimeManifest,
        config: &NodeSupervisorConfig,
    ) -> Result<Self, String> {
        let faucet = manifest
            .bootstrap_accounts
            .iter()
            .find(|account| account.index == 0)
            .ok_or_else(|| "runtime manifest is missing dev-0".to_owned())?;
        if faucet.role != BOOTSTRAP_ACCOUNT_ROLE_FAUCET
            || manifest.bootstrap_account_derivation.seed != config.hogd_dev.dev_account_seed
        {
            return Err(
                "runtime manifest dev-0 faucet derivation does not match configuration".into(),
            );
        }
        let signer = derive_dev_account_signer(&config.hogd_dev.dev_account_seed, 0);
        if faucet.public_key.as_ref() != signer.verifying_key_bytes() {
            return Err("runtime manifest dev-0 public key does not match configuration".into());
        }
        let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
        if faucet.wallet_address.0 != account.address() {
            return Err(
                "runtime manifest dev-0 address is not its nonce-zero native account".into(),
            );
        }
        Ok(Self {
            network_hash: manifest.network_hash.to_network_hash(),
            signer: Arc::new(signer),
            config: config.local_airdrop,
        })
    }
}

#[derive(Clone)]
pub(crate) struct LocalAirdropRpc {
    backend: Arc<dyn AirdropBackend>,
    builder: Arc<dyn AirdropTransactionBuilder>,
    attempts: Arc<Mutex<HashMap<AttemptKey, Arc<Attempt>>>>,
    capacity: usize,
    concurrency: Arc<Semaphore>,
    admission: Arc<Semaphore>,
    timeout: Duration,
    raw_amount: u128,
    block_driver: BlockDriverTrigger,
}

impl LocalAirdropRpc {
    pub(crate) fn new<S, R>(
        context: LocalAirdropContext,
        submit_chain: S,
        read: R,
        block_driver: BlockDriverTrigger,
    ) -> Self
    where
        S: SubmitPlane + ChainPlane,
        R: ReadPlane,
    {
        let builder = NativeAirdropBuilder {
            network_hash: context.network_hash,
            signer: context.signer,
            next_nonce: AtomicU64::new(nonce_from_timestamp()),
            raw_amount: context.config.raw_amount,
        };
        Self::with_parts(
            Arc::new(PlaneAirdropBackend { submit_chain, read }),
            Arc::new(builder),
            context.config,
            block_driver,
        )
    }

    fn with_parts(
        backend: Arc<dyn AirdropBackend>,
        builder: Arc<dyn AirdropTransactionBuilder>,
        config: LocalAirdropConfig,
        block_driver: BlockDriverTrigger,
    ) -> Self {
        Self {
            backend,
            builder,
            attempts: Arc::new(Mutex::new(HashMap::new())),
            capacity: config.attempt_capacity,
            concurrency: Arc::new(Semaphore::new(config.max_concurrency)),
            // Allow a finite burst of 16 execution waves across all callers, including
            // same-key observers. Execution concurrency remains independently bounded.
            admission: Arc::new(Semaphore::new(
                config
                    .max_concurrency
                    .saturating_mul(16)
                    .min(Semaphore::MAX_PERMITS),
            )),
            timeout: config.request_timeout,
            raw_amount: config.raw_amount,
            block_driver,
        }
    }

    async fn request(
        &self,
        request: AirdropRequest,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let context = RpcRequestContext::new(Some(self.timeout));
        let _admission = self.admission.try_acquire().map_err(|_| {
            RpcServerError::Overloaded("local airdrop admission capacity reached".into())
        })?;
        within_request_deadline(context, self.request_admitted(request, context)).await
    }

    async fn request_admitted(
        &self,
        request: AirdropRequest,
        context: RpcRequestContext,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let key = AttemptKey::new(&request);
        let attempt = self.find_or_admit(key)?;
        let observed_revision = attempt.revision()?;
        let _operation = attempt.operation.lock().await;

        if attempt.revision()? != observed_revision {
            return attempt.last_result()?.ok_or_else(|| {
                RpcServerError::BackendUnavailable(
                    "local airdrop attempt completed without an outcome".into(),
                )
            })?;
        }
        if attempt.is_terminal()? {
            return attempt.last_result()?.ok_or_else(|| {
                RpcServerError::BackendUnavailable(
                    "terminal local airdrop attempt has no outcome".into(),
                )
            })?;
        }

        let permit = self.concurrency.acquire().await.map_err(|_| {
            RpcServerError::BackendUnavailable("local airdrop execution is closed".into())
        })?;
        if context
            .remaining()
            .is_some_and(|remaining| remaining.is_zero())
        {
            return Err(RpcServerError::BackendTimeout);
        }
        let result = self
            .advance_attempt(&attempt, request.recipient, context)
            .await;
        drop(permit);
        attempt.record_result(result.clone());
        result
    }

    fn find_or_admit(&self, key: AttemptKey) -> Result<Arc<Attempt>, RpcServerError> {
        let mut attempts = self.attempts.lock().map_err(|_| {
            RpcServerError::BackendUnavailable("local airdrop attempt store is poisoned".into())
        })?;
        if let Some(attempt) = attempts.get(&key) {
            return Ok(attempt.clone());
        }
        if attempts.len() >= self.capacity
            && let Some(terminal_key) = attempts
                .iter()
                .find(|(_, attempt)| Arc::strong_count(attempt) == 1 && attempt.is_evictable())
                .map(|(key, _)| key.clone())
        {
            attempts.remove(&terminal_key);
        }
        if attempts.len() >= self.capacity {
            return Err(RpcServerError::Overloaded(
                "local airdrop attempt capacity reached".into(),
            ));
        }
        let attempt = Arc::new(Attempt::new());
        attempts.insert(key, attempt.clone());
        Ok(attempt)
    }

    async fn advance_attempt(
        &self,
        attempt: &Attempt,
        recipient: ContractAddress,
        context: RpcRequestContext,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        if attempt.signed()?.is_none() {
            let tip = self.backend.chain_tip(context).await?;
            let signed = match self.builder.build(
                recipient,
                (*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET),
            ) {
                Ok(signed) => signed,
                Err(error) => {
                    attempt.set_terminal(true)?;
                    return Err(error);
                }
            };
            attempt.store_signed(signed)?;
        }
        let signed = attempt.signed()?.ok_or_else(|| {
            RpcServerError::BackendUnavailable("local airdrop transaction was not retained".into())
        })?;

        let mut submit_error = if attempt.is_submitted()? {
            None
        } else {
            match self.backend.submit(context, &signed).await {
                Ok(()) => {
                    attempt.set_submitted(true)?;
                    None
                }
                Err(error) => Some(classify_backend_error(error)),
            }
        };
        if let Some(error) = submit_error.take_if(|error| is_deterministic_failure(error)) {
            attempt.set_terminal(true)?;
            return Err(error);
        }

        match self.backend.receipt(context, signed.tx_hash).await {
            Ok(Some(receipt)) if matches!(receipt.status, ExecStatus::Ok) => {
                attempt.set_terminal(true)?;
                Ok(AirdropAttemptResponse::Succeeded {
                    recipient,
                    tx_hash: signed.tx_hash,
                    raw_amount: self.raw_amount.to_string(),
                    height: receipt.height,
                    status: receipt.status,
                })
            }
            Ok(Some(receipt)) => {
                attempt.set_terminal(true)?;
                Err(RpcServerError::TxRejected(format!(
                    "local airdrop execution failed: {:?}",
                    receipt.status
                )))
            }
            Ok(None) => match self.backend.status(context, signed.tx_hash).await {
                Ok(TransactionStatus::Invalid { reason }) => {
                    attempt.set_terminal(true)?;
                    Err(RpcServerError::TxRejected(reason))
                }
                Ok(TransactionStatus::Pending) => {
                    if submit_error.is_some() {
                        self.block_driver.trigger_after_submit();
                    }
                    attempt.set_submitted(true)?;
                    Ok(self.pending_response(recipient, signed.tx_hash))
                }
                Ok(TransactionStatus::RecentFailure { .. }) => {
                    attempt.set_submitted(false)?;
                    Err(RpcServerError::BackendUnavailable(
                        "local airdrop has a recent nondeterministic failure and may still commit"
                            .into(),
                    ))
                }
                Ok(TransactionStatus::Unknown) => {
                    attempt.set_submitted(false)?;
                    match submit_error {
                        Some(error) => Err(error),
                        None => Ok(self.pending_response(recipient, signed.tx_hash)),
                    }
                }
                Ok(_) => match submit_error {
                    Some(error) => Err(error),
                    None => Ok(self.pending_response(recipient, signed.tx_hash)),
                },
                Err(error) => Err(submit_error.unwrap_or_else(|| classify_backend_error(error))),
            },
            Err(error) => Err(classify_backend_error(error)),
        }
    }

    fn pending_response(
        &self,
        recipient: ContractAddress,
        tx_hash: TxHash,
    ) -> AirdropAttemptResponse {
        AirdropAttemptResponse::Pending {
            recipient,
            raw_amount: self.raw_amount.to_string(),
            tx_hash: Some(tx_hash),
            retry_after_seconds: LOCAL_AIRDROP_RETRY_AFTER_SECONDS,
        }
    }
}

#[async_trait]
impl AirdropApiServer for LocalAirdropRpc {
    #[tracing::instrument(skip_all, fields(?request.recipient))]
    async fn request_airdrop(&self, request: AirdropRequest) -> RpcResult<AirdropAttemptResponse> {
        self.request(request).await.map_err(Into::into)
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct AttemptKey {
    recipient: [u8; 32],
    client_nonce: Vec<u8>,
}

impl AttemptKey {
    fn new(request: &AirdropRequest) -> Self {
        Self {
            recipient: request.recipient.0.into_inner(),
            client_nonce: request.client_nonce.as_bytes().to_vec(),
        }
    }
}

struct Attempt {
    operation: tokio::sync::Mutex<()>,
    state: Mutex<AttemptState>,
}

#[derive(Default)]
struct AttemptState {
    signed: Option<SignedAirdrop>,
    last_result: Option<Result<AirdropAttemptResponse, RpcServerError>>,
    revision: u64,
    terminal: bool,
    submitted: bool,
}

impl Attempt {
    fn new() -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            state: Mutex::new(AttemptState::default()),
        }
    }

    fn signed(&self) -> Result<Option<SignedAirdrop>, RpcServerError> {
        self.state
            .lock()
            .map(|state| state.signed.clone())
            .map_err(|_| attempt_state_poisoned())
    }

    fn store_signed(&self, signed: SignedAirdrop) -> Result<(), RpcServerError> {
        let mut state = self.state.lock().map_err(|_| attempt_state_poisoned())?;
        if state.signed.is_some() {
            return Err(RpcServerError::BackendUnavailable(
                "local airdrop transaction was constructed more than once".into(),
            ));
        }
        state.signed = Some(signed);
        Ok(())
    }

    fn revision(&self) -> Result<u64, RpcServerError> {
        self.state
            .lock()
            .map(|state| state.revision)
            .map_err(|_| attempt_state_poisoned())
    }

    fn is_terminal(&self) -> Result<bool, RpcServerError> {
        self.state
            .lock()
            .map(|state| state.terminal)
            .map_err(|_| attempt_state_poisoned())
    }

    fn set_terminal(&self, terminal: bool) -> Result<(), RpcServerError> {
        self.state
            .lock()
            .map(|mut state| state.terminal = terminal)
            .map_err(|_| attempt_state_poisoned())
    }

    fn is_submitted(&self) -> Result<bool, RpcServerError> {
        self.state
            .lock()
            .map(|state| state.submitted)
            .map_err(|_| attempt_state_poisoned())
    }

    fn set_submitted(&self, submitted: bool) -> Result<(), RpcServerError> {
        self.state
            .lock()
            .map(|mut state| state.submitted = submitted)
            .map_err(|_| attempt_state_poisoned())
    }

    fn last_result(
        &self,
    ) -> Result<Option<Result<AirdropAttemptResponse, RpcServerError>>, RpcServerError> {
        self.state
            .lock()
            .map(|state| state.last_result.clone())
            .map_err(|_| attempt_state_poisoned())
    }

    fn record_result(&self, result: Result<AirdropAttemptResponse, RpcServerError>) {
        match self.state.lock() {
            Ok(mut state) => {
                state.last_result = Some(result);
                state.revision = state.revision.wrapping_add(1);
            }
            Err(error) => tracing::error!(%error, "local airdrop outcome store is poisoned"),
        }
    }

    fn is_evictable(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.signed.is_none() || state.terminal && state.last_result.is_some()
        })
    }
}

fn attempt_state_poisoned() -> RpcServerError {
    RpcServerError::BackendUnavailable("local airdrop attempt state is poisoned".into())
}

#[derive(Clone)]
struct SignedAirdrop {
    bytes: Vec<u8>,
    tx_hash: TxHash,
}

trait AirdropTransactionBuilder: Send + Sync + 'static {
    fn build(
        &self,
        recipient: ContractAddress,
        block_expiry_height: i64,
    ) -> Result<SignedAirdrop, RpcServerError>;
}

struct NativeAirdropBuilder {
    network_hash: groundhog_xdr::types::NetworkHash,
    signer: Arc<LocalSigner>,
    next_nonce: AtomicU64,
    raw_amount: u128,
}

impl AirdropTransactionBuilder for NativeAirdropBuilder {
    fn build(
        &self,
        recipient: ContractAddress,
        block_expiry_height: i64,
    ) -> Result<SignedAirdrop, RpcServerError> {
        let build = || {
            let account =
                PubkeyAccount::from_public_key_bytes(self.signer.verifying_key_bytes(), 0);
            let calldata =
                groundhog_signing::native_hog_transfer_calldata(&recipient.0, self.raw_amount)?;
            let tx = groundhog_signing::default_wallet::build_default_wallet_call_transaction(
                groundhog_signing::default_wallet::BuildDefaultWalletCallTransaction {
                    account: &account,
                    target_address: groundhog_signing::NATIVE_HOG_ADDRESS,
                    target_entrypoint: groundhog_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
                    target_calldata: &calldata,
                    gas_limit: DEFAULT_TX_GAS_LIMIT * 2,
                    target_gas_limit: DEFAULT_TX_GAS_LIMIT,
                    block_expiry_height,
                    nonce: self.next_nonce.fetch_add(1, Ordering::Relaxed),
                },
            )?;
            let tx_hash = transaction_hash(&tx)?;
            let signed = sign_transaction(
                tx,
                self.signer.as_ref(),
                SignTransactionParams::default(),
                &self.network_hash,
            )?;
            let bytes = encode_signed_transaction_xdr(&signed)?;
            Ok::<_, groundhog_signing::SigningError>(SignedAirdrop {
                bytes,
                tx_hash: TxHash(Hex0x(tx_hash)),
            })
        };
        build().map_err(|error| {
            RpcServerError::BackendUnavailable(format!(
                "failed to construct local airdrop transaction: {error}"
            ))
        })
    }
}

#[async_trait]
trait AirdropBackend: Send + Sync + 'static {
    async fn chain_tip(
        &self,
        context: RpcRequestContext,
    ) -> Result<groundhog_json_rpc_types::block::ChainTip, RpcServerError>;
    async fn submit(
        &self,
        context: RpcRequestContext,
        signed: &SignedAirdrop,
    ) -> Result<(), RpcServerError>;
    async fn receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<groundhog_json_rpc_types::receipt::TransactionReceipt>, RpcServerError>;
    async fn status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError>;
}

struct PlaneAirdropBackend<S, R> {
    submit_chain: S,
    read: R,
}

#[async_trait]
impl<S, R> AirdropBackend for PlaneAirdropBackend<S, R>
where
    S: SubmitPlane + ChainPlane,
    R: ReadPlane,
{
    async fn chain_tip(
        &self,
        context: RpcRequestContext,
    ) -> Result<groundhog_json_rpc_types::block::ChainTip, RpcServerError> {
        within_request_deadline(context, async {
            Ok(self.submit_chain.chain_tip(context).await?.tip)
        })
        .await
    }

    async fn submit(
        &self,
        context: RpcRequestContext,
        signed: &SignedAirdrop,
    ) -> Result<(), RpcServerError> {
        let tx =
            validate_raw_transaction_bytes(signed.bytes.clone(), &NetworkConfigParams::default())
                .map_err(|error| {
                RpcServerError::BackendUnavailable(format!(
                    "locally constructed airdrop transaction failed validation: {error}"
                ))
            })?;
        if tx.hash != signed.tx_hash {
            return Err(RpcServerError::BackendUnavailable(
                "retained local airdrop transaction hash does not match its signed bytes".into(),
            ));
        }
        within_request_deadline(
            context,
            self.submit_chain.submit_raw_transaction(context, tx),
        )
        .await
    }

    async fn receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<groundhog_json_rpc_types::receipt::TransactionReceipt>, RpcServerError> {
        within_request_deadline(context, self.read.transaction_receipt(context, tx_hash)).await
    }

    async fn status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        within_request_deadline(context, self.read.transaction_status(context, tx_hash)).await
    }
}

fn classify_backend_error(error: RpcServerError) -> RpcServerError {
    match error {
        RpcServerError::TxInvalid(message) => RpcServerError::TxRejected(message),
        RpcServerError::NotImplemented(method) => RpcServerError::BackendUnavailable(format!(
            "local airdrop requires backend operation `{method}`"
        )),
        other => other,
    }
}

fn is_deterministic_failure(error: &RpcServerError) -> bool {
    matches!(error, RpcServerError::TxRejected(_))
}

fn nonce_from_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests;
