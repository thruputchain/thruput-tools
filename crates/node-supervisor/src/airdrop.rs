use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use groundhog_json_rpc_api::{AirdropApiServer, LocalFundingApiServer};
use groundhog_json_rpc_server::{
    ChainPlane, ReadPlane, RpcRequestContext, RpcServerError, SubmitPlane,
    tx::validate_raw_transaction_bytes,
};
use groundhog_json_rpc_types::{
    ContractAddress,
    airdrop::{
        AirdropAttemptResponse, AirdropRequest, FundAccountRequest, FundAccountResponse,
        LocalFundingErrorData,
    },
    hex::Hex0x,
    receipt::ExecStatus,
    runtime_manifest::{BOOTSTRAP_ACCOUNT_ROLE_FAUCET, RuntimeManifest},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_signing::{
    DEFAULT_TX_GAS_LIMIT, LocalSigner, PubkeyAccount, SignTransactionParams,
    encode_signed_transaction_xdr, sign_transaction, transaction_hash,
};
use groundhog_tx_validation::NetworkConfigParams;
use jsonrpsee::core::RpcResult;
use tokio::sync::Semaphore;

use crate::{
    LocalAirdropConfig, NodeSupervisorConfig,
    block_driver::BlockDriverTrigger,
    funding_journal::{FundingJournal, FundingRecord, RetainedFundingTransaction},
    request_deadline::within_request_deadline,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const LOCAL_AIRDROP_RETRY_AFTER_SECONDS: u64 = 1;

#[derive(Clone)]
pub(crate) struct LocalAirdropContext {
    network_hash: groundhog_xdr::types::NetworkHash,
    signer: Arc<LocalSigner>,
    config: LocalAirdropConfig,
    journal: Arc<Mutex<FundingJournal>>,
    lock_lease: Option<Arc<std::fs::File>>,
}

impl LocalAirdropContext {
    pub(crate) fn from_validated_runtime(
        manifest: &RuntimeManifest,
        config: &NodeSupervisorConfig,
        signer: Arc<LocalSigner>,
        journal: Arc<Mutex<FundingJournal>>,
        lock_lease: Option<Arc<std::fs::File>>,
    ) -> Result<Self, String> {
        let mut faucets = manifest
            .bootstrap_accounts
            .iter()
            .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_FAUCET);
        let faucet = faucets
            .next()
            .ok_or_else(|| "runtime manifest is missing its local faucet".to_owned())?;
        if faucets.next().is_some() {
            return Err("runtime manifest has multiple local faucets".into());
        }
        if faucet.public_key.as_ref() != signer.verifying_key_bytes() {
            return Err("runtime faucet public key does not match its private credential".into());
        }
        let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
        if faucet.wallet_address.0 != account.address() {
            return Err("runtime faucet address is not its nonce-zero native account".into());
        }
        Ok(Self {
            network_hash: manifest.network_hash.to_network_hash(),
            signer,
            config: config.local_airdrop,
            journal,
            lock_lease,
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
    persistence: Arc<Semaphore>,
    timeout: Duration,
    raw_amount: u128,
    faucet: ContractAddress,
    journal: Arc<Mutex<FundingJournal>>,
    lock_lease: Option<Arc<std::fs::File>>,
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
        let faucet = Hex0x(
            PubkeyAccount::from_public_key_bytes(context.signer.verifying_key_bytes(), 0).address(),
        );
        let builder = NativeAirdropBuilder {
            network_hash: context.network_hash,
            signer: context.signer,
            next_nonce: AtomicU64::new(nonce_from_timestamp()),
        };
        let mut rpc = Self::with_parts(
            Arc::new(PlaneAirdropBackend { submit_chain, read }),
            Arc::new(builder),
            context.config,
            block_driver,
        );
        rpc.faucet = faucet;
        rpc.journal = context.journal;
        rpc.lock_lease = context.lock_lease;
        rpc
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
            persistence: Arc::new(Semaphore::new(config.max_concurrency)),
            lock_lease: None,
            timeout: config.request_timeout,
            raw_amount: config.raw_amount,
            faucet: Hex0x(groundhog_xdr::types::ContractAddress::from([0; 32])),
            journal: Arc::new(Mutex::new(FundingJournal::memory(
                [0; 32].into(),
                Hex0x(groundhog_xdr::types::ContractAddress::from([0; 32])),
            ))),
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
        let attempt = self
            .find_or_admit(FundAccountRequest {
                recipient: request.recipient,
                raw_amount: self.raw_amount.to_string(),
                client_nonce: request.client_nonce.clone(),
                faucet: self.faucet,
            })
            .await?;
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

    async fn find_or_admit(
        &self,
        request: FundAccountRequest,
    ) -> Result<Arc<Attempt>, RpcServerError> {
        let permit = self
            .persistence
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| {
                RpcServerError::BackendUnavailable("funding persistence is closed".into())
            })?;
        let rpc = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            rpc.register_attempt(request)
        })
        .await
        .map_err(|_| {
            RpcServerError::BackendUnavailable("funding persistence worker stopped".into())
        })?
    }

    fn register_attempt(
        &self,
        request: FundAccountRequest,
    ) -> Result<Arc<Attempt>, RpcServerError> {
        let key = AttemptKey::funding(&request);
        let mut attempts = self.attempts.lock().map_err(|_| attempt_state_poisoned())?;
        let mut journal = self.journal.lock().map_err(|_| attempt_state_poisoned())?;
        let retained = journal
            .records()
            .iter()
            .any(|record| AttemptKey::funding(&record.request) == key);
        if !retained && journal.records().len() >= self.capacity {
            return Err(RpcServerError::Overloaded(
                "local funding attempt capacity reached; existing requests can still reconcile"
                    .into(),
            ));
        }
        let record = journal.register(request)?;
        if let Some(attempt) = attempts.get(&key) {
            return Ok(attempt.clone());
        }
        let attempt = Arc::new(Attempt::from_record(record));
        attempts.insert(key, attempt.clone());
        Ok(attempt)
    }

    async fn retain_signed(
        &self,
        attempt: &Attempt,
        signed: &SignedAirdrop,
    ) -> Result<(), RpcServerError> {
        let permit = self
            .persistence
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| {
                RpcServerError::BackendUnavailable("funding persistence is closed".into())
            })?;
        let lock_lease = self.lock_lease.clone();
        let journal = self.journal.clone();
        let request = attempt.request.clone();
        let retained = RetainedFundingTransaction {
            tx_hash: signed.tx_hash,
            signed_transaction: groundhog_json_rpc_types::hex::HexBytes(signed.bytes.clone()),
        };
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _lock_lease = lock_lease;
            journal
                .lock()
                .map_err(|_| attempt_state_poisoned())?
                .retain_signed(&request, retained)
        })
        .await
        .map_err(|_| {
            RpcServerError::BackendUnavailable("funding persistence worker stopped".into())
        })?
    }

    async fn prepare_signed(
        &self,
        attempt: &Attempt,
        context: RpcRequestContext,
    ) -> Result<SignedAirdrop, RpcServerError> {
        if attempt.signed()?.is_none() {
            let tip = self.backend.chain_tip(context).await?;
            let signed = self.builder.build(
                attempt.request.recipient,
                (*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET),
                attempt
                    .request
                    .amount()
                    .map_err(|e| RpcServerError::InvalidParam(e.into()))?,
            )?;
            attempt.store_signed(signed)?;
        }
        let signed = attempt.signed()?.ok_or_else(|| {
            RpcServerError::BackendUnavailable("local funding transaction was not retained".into())
        })?;
        self.retain_signed(attempt, &signed).await?;
        Ok(signed)
    }

    async fn fund(
        &self,
        request: FundAccountRequest,
    ) -> Result<FundAccountResponse, RpcServerError> {
        request
            .amount()
            .map_err(|e| RpcServerError::InvalidParam(e.into()))?;
        if request.faucet != self.faucet {
            return Err(RpcServerError::InvalidParam(
                "expected faucet belongs to a different local chain".into(),
            ));
        }
        let _admission = self.admission.try_acquire().map_err(|_| {
            RpcServerError::Overloaded("local funding admission capacity reached".into())
        })?;
        let context = RpcRequestContext::new(Some(self.timeout));
        within_request_deadline(context, self.fund_admitted(request.clone(), context))
            .await
            .map_err(|error| {
                // Do not extend the request deadline waiting for a persistence worker.
                // A retry can recover evidence if that worker still holds the journal.
                let evidence = self
                    .journal
                    .try_lock()
                    .ok()
                    .and_then(|journal| journal.durable_signed(&request));
                match evidence {
                    Some(retained) => RpcServerError::LocalFundingUnresolved {
                        reason: error.to_string(),
                        evidence: LocalFundingErrorData::new(
                            retained.tx_hash,
                            retained.signed_transaction,
                        ),
                    },
                    None => error,
                }
            })
    }

    async fn fund_admitted(
        &self,
        request: FundAccountRequest,
        context: RpcRequestContext,
    ) -> Result<FundAccountResponse, RpcServerError> {
        let attempt = self.find_or_admit(request).await?;
        let _operation = attempt.operation.lock().await;
        let _permit =
            self.concurrency.acquire().await.map_err(|_| {
                RpcServerError::BackendUnavailable("local funding is closed".into())
            })?;
        let replay = attempt.signed()?.is_some();
        let signed = self.prepare_signed(&attempt, context).await?;
        let response = || {
            FundAccountResponse::new(
                attempt.request.recipient,
                attempt.request.raw_amount.clone(),
                signed.tx_hash,
                groundhog_json_rpc_types::hex::HexBytes(signed.bytes.clone()),
            )
        };
        if replay {
            if self
                .backend
                .receipt(context, signed.tx_hash)
                .await?
                .is_some()
            {
                return Ok(response());
            }
            match self.backend.status(context, signed.tx_hash).await? {
                TransactionStatus::Pending | TransactionStatus::Committed { .. } => {
                    return Ok(response());
                }
                TransactionStatus::Invalid { reason } => {
                    return Err(RpcServerError::TxRejected(reason));
                }
                TransactionStatus::Unknown
                | TransactionStatus::RecentFailure { .. }
                | TransactionStatus::Other => {}
                _ => {
                    return Err(RpcServerError::BackendUnavailable(
                        "unrecognized retained funding status".into(),
                    ));
                }
            }
            self.check_retained_expiry(&signed, context).await?;
        }
        match self.backend.submit(context, &signed).await {
            Ok(()) => {
                attempt.set_submitted(true)?;
                self.block_driver.trigger_after_submit();
                Ok(response())
            }
            Err(error) => {
                // A request-scoped admission error must not overwrite an older
                // accepted or committed copy of this same retained transaction.
                if self
                    .backend
                    .receipt(context, signed.tx_hash)
                    .await?
                    .is_some()
                {
                    return Ok(response());
                }
                match self.backend.status(context, signed.tx_hash).await? {
                    TransactionStatus::Pending | TransactionStatus::Committed { .. } => {
                        self.block_driver.trigger_after_submit();
                        Ok(response())
                    }
                    _ => Err(classify_backend_error(error)),
                }
            }
        }
    }

    async fn check_retained_expiry(
        &self,
        signed: &SignedAirdrop,
        context: RpcRequestContext,
    ) -> Result<(), RpcServerError> {
        let tx =
            validate_raw_transaction_bytes(signed.bytes.clone(), &NetworkConfigParams::default())
                .map_err(|_| {
                RpcServerError::BackendUnavailable(
                    "retained funding transaction is malformed".into(),
                )
            })?;
        let tip = self.backend.chain_tip(context).await?;
        if *tx.block_expiry_height <= *tip.height {
            return Err(RpcServerError::BackendUnavailable("retained funding expired with no authoritative outcome; reconcile its hash instead of creating another transfer".into()));
        }
        Ok(())
    }

    async fn advance_attempt(
        &self,
        attempt: &Attempt,
        recipient: ContractAddress,
        context: RpcRequestContext,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let signed = self.prepare_signed(attempt, context).await?;
        if attempt.restored {
            if let Some(receipt) = self.backend.receipt(context, signed.tx_hash).await? {
                if matches!(receipt.status, ExecStatus::Ok) {
                    return Ok(AirdropAttemptResponse::Succeeded {
                        recipient,
                        tx_hash: signed.tx_hash,
                        raw_amount: self.raw_amount.to_string(),
                        height: receipt.height,
                        status: receipt.status,
                    });
                }
                return Err(RpcServerError::TxRejected(format!(
                    "local airdrop execution failed: {:?}",
                    receipt.status
                )));
            }
            match self.backend.status(context, signed.tx_hash).await? {
                TransactionStatus::Pending | TransactionStatus::Committed { .. } => {
                    return Ok(self.pending_response(recipient, signed.tx_hash));
                }
                TransactionStatus::Invalid { reason } => {
                    return Err(RpcServerError::TxRejected(reason));
                }
                _ => self.check_retained_expiry(&signed, context).await?,
            }
        }

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

#[async_trait]
impl LocalFundingApiServer for LocalAirdropRpc {
    #[tracing::instrument(skip_all, fields(?request.recipient))]
    async fn fund_account(&self, request: FundAccountRequest) -> RpcResult<FundAccountResponse> {
        self.fund(request).await.map_err(Into::into)
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct AttemptKey {
    recipient: [u8; 32],
    client_nonce: Vec<u8>,
}

impl AttemptKey {
    fn funding(request: &FundAccountRequest) -> Self {
        Self {
            recipient: request.recipient.0.into_inner(),
            client_nonce: request.client_nonce.as_bytes().to_vec(),
        }
    }
}

struct Attempt {
    request: FundAccountRequest,
    restored: bool,
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
    fn from_record(record: FundingRecord) -> Self {
        let signed = record.signed.map(|signed| SignedAirdrop {
            bytes: signed.signed_transaction.0,
            tx_hash: signed.tx_hash,
        });
        Self {
            request: record.request,
            restored: signed.is_some(),
            operation: tokio::sync::Mutex::new(()),
            state: Mutex::new(AttemptState {
                signed,
                ..AttemptState::default()
            }),
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
        raw_amount: u128,
    ) -> Result<SignedAirdrop, RpcServerError>;
}

struct NativeAirdropBuilder {
    network_hash: groundhog_xdr::types::NetworkHash,
    signer: Arc<LocalSigner>,
    next_nonce: AtomicU64,
}

/// Rebuild retained bytes from their immutable request and saved uniquifier.
/// This checks the transfer intent, network signature, and every transaction field.
pub(crate) fn validate_retained_funding(
    request: &FundAccountRequest,
    retained: &RetainedFundingTransaction,
    network_hash: groundhog_xdr::types::NetworkHash,
    signer: Arc<LocalSigner>,
) -> Result<(), String> {
    let retained_tx: groundhog_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&retained.signed_transaction.0)
            .map_err(|_| "retained funding transaction is malformed")?;
    let expected = NativeAirdropBuilder {
        network_hash,
        signer,
        next_nonce: AtomicU64::new(retained_tx.tx.uniquifier.nonce),
    }
    .build(
        request.recipient,
        *retained_tx.tx.uniquifier.block_expiry_height,
        request.amount()?,
    )
    .map_err(|_| "could not validate retained funding intent")?;
    if expected.tx_hash != retained.tx_hash || expected.bytes != retained.signed_transaction.0 {
        return Err(
            "retained funding transaction differs from its request or faucet signature".into(),
        );
    }
    Ok(())
}

impl AirdropTransactionBuilder for NativeAirdropBuilder {
    fn build(
        &self,
        recipient: ContractAddress,
        block_expiry_height: i64,
        raw_amount: u128,
    ) -> Result<SignedAirdrop, RpcServerError> {
        let build = || {
            let account =
                PubkeyAccount::from_public_key_bytes(self.signer.verifying_key_bytes(), 0);
            let calldata =
                groundhog_signing::native_hog_transfer_calldata(&recipient.0, raw_amount)?;
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
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use groundhog_json_rpc_types::{
        BlockHeight,
        airdrop::ClientNonce,
        block::ChainTip,
        hex::{Hex0x, HexBytes},
        transaction::{ConflictCode, ConflictDiagnostic, TransactionStatus},
    };

    use super::*;

    struct CountingBuilder {
        constructions: AtomicUsize,
        bytes: Vec<u8>,
    }

    impl AirdropTransactionBuilder for CountingBuilder {
        fn build(
            &self,
            _recipient: ContractAddress,
            _block_expiry_height: i64,
            _raw_amount: u128,
        ) -> Result<SignedAirdrop, RpcServerError> {
            self.constructions.fetch_add(1, Ordering::SeqCst);
            Ok(SignedAirdrop {
                bytes: self.bytes.clone(),
                tx_hash: test_tx_hash(),
            })
        }
    }

    struct TestBackend {
        submit_results: Mutex<VecDeque<Result<(), RpcServerError>>>,
        status_results: Mutex<VecDeque<Result<TransactionStatus, RpcServerError>>>,
        submissions: Mutex<Vec<Vec<u8>>>,
        submit_delay: Duration,
        submit_gate: Option<Arc<Semaphore>>,
        contexts: Mutex<Vec<RpcRequestContext>>,
    }

    impl TestBackend {
        fn new(submit_results: Vec<Result<(), RpcServerError>>) -> Self {
            Self {
                submit_results: Mutex::new(submit_results.into()),
                status_results: Mutex::new(VecDeque::new()),
                submissions: Mutex::new(Vec::new()),
                submit_delay: Duration::ZERO,
                submit_gate: None,
                contexts: Mutex::new(Vec::new()),
            }
        }

        fn with_submit_delay(mut self, delay: Duration) -> Self {
            self.submit_delay = delay;
            self
        }

        fn with_status_results(
            mut self,
            status_results: Vec<Result<TransactionStatus, RpcServerError>>,
        ) -> Self {
            self.status_results = Mutex::new(status_results.into());
            self
        }
    }

    #[async_trait]
    impl AirdropBackend for TestBackend {
        async fn chain_tip(&self, context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            self.contexts.lock().unwrap().push(context);
            Ok(ChainTip {
                height: BlockHeight::from(7),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x77; 32])),
            })
        }

        async fn submit(
            &self,
            _context: RpcRequestContext,
            signed: &SignedAirdrop,
        ) -> Result<(), RpcServerError> {
            self.submissions
                .lock()
                .expect("bug: submissions lock should be available")
                .push(signed.bytes.clone());
            if let Some(gate) = &self.submit_gate {
                gate.acquire().await.unwrap().forget();
            }
            if !self.submit_delay.is_zero() {
                tokio::time::sleep(self.submit_delay).await;
            }
            self.submit_results
                .lock()
                .expect("bug: submit results lock should be available")
                .pop_front()
                .unwrap_or(Ok(()))
        }

        async fn receipt(
            &self,
            _context: RpcRequestContext,
            _tx_hash: TxHash,
        ) -> Result<Option<groundhog_json_rpc_types::receipt::TransactionReceipt>, RpcServerError>
        {
            Ok(None)
        }

        async fn status(
            &self,
            _context: RpcRequestContext,
            _tx_hash: TxHash,
        ) -> Result<TransactionStatus, RpcServerError> {
            self.status_results
                .lock()
                .expect("bug: status results lock should be available")
                .pop_front()
                .unwrap_or(Ok(TransactionStatus::Pending))
        }
    }

    #[test]
    fn native_builder_binds_signatures_to_network_hash() {
        let build = |network_byte| {
            NativeAirdropBuilder {
                network_hash: groundhog_xdr::types::NetworkHash::from(
                    groundhog_xdr::types::SHA256Hash::from([network_byte; 32]),
                ),
                signer: Arc::new(LocalSigner::from_secret_key([0x33; 32])),
                next_nonce: AtomicU64::new(7),
            }
            .build(
                ContractAddress::from(groundhog_xdr::types::ContractAddress::from([0x44; 32])),
                1_000,
                100_000_000,
            )
            .expect("bug: local airdrop should build")
        };

        let first = build(0x55);
        let second = build(0x66);

        assert_eq!(first.tx_hash, second.tx_hash);
        assert_ne!(first.bytes, second.bytes);
    }

    #[tokio::test]
    async fn registered_http_small_array_waits_without_raising_execution_limit() {
        use groundhog_json_rpc_server::batch::{ConcurrentBatchService, RpcBatchConcurrency};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for concurrency in [1, RpcBatchConcurrency::default().get()] {
            let gate = Arc::new(Semaphore::new(0));
            let mut backend = TestBackend::new(vec![]);
            backend.submit_gate = Some(gate.clone());
            let backend = Arc::new(backend);
            let builder = Arc::new(CountingBuilder {
                constructions: AtomicUsize::new(0),
                bytes: vec![1, 2, 3],
            });
            let rpc = test_rpc(backend.clone(), builder.clone(), 64, 16);
            let middleware =
                jsonrpsee::core::middleware::RpcServiceBuilder::new().layer_fn(move |inner| {
                    ConcurrentBatchService::new(inner, 100_000, concurrency.try_into().unwrap())
                });
            let server = jsonrpsee::server::Server::builder()
                .set_rpc_middleware(middleware)
                .build("127.0.0.1:0")
                .await
                .unwrap();
            let address = server.local_addr().unwrap();
            let handle = server.start(AirdropApiServer::into_rpc(rpc.clone()));
            let body = serde_json::to_string(
                &(1..=20)
                    .map(|id| {
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": id, "method": "groundhog_requestAirdrop",
                            "params": [test_request(id, id)]
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            let response = tokio::spawn(async move {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(format!(
                    "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
                ).as_bytes()).await.unwrap();
                let mut response = String::new();
                socket.read_to_string(&mut response).await.unwrap();
                response
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while backend.submissions.lock().unwrap().len() < concurrency.min(16)
                    || rpc.admission.available_permits() != 256 - concurrency.min(20)
                {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    backend.submissions.lock().unwrap().len(),
                    concurrency.min(16)
                );
                assert_eq!(rpc.admission.available_permits(), 256 - concurrency.min(20));
                gate.add_permits(20);
                let response = response.await.unwrap();
                let responses: Vec<serde_json::Value> =
                    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_eq!(responses.len(), 20);
                for response in responses {
                    assert!(response.get("error").is_none(), "{response}");
                    assert_eq!(response["result"]["attemptStatus"], "pending");
                }
            })
            .await
            .unwrap();
            assert_eq!(builder.constructions.load(Ordering::SeqCst), 20);
            assert_eq!(backend.submissions.lock().unwrap().len(), 20);
            assert_eq!(rpc.concurrency.available_permits(), 16);
            assert_eq!(rpc.admission.available_permits(), 256);
            handle.stop().unwrap();
            handle.stopped().await;
        }
    }

    #[tokio::test]
    async fn admission_bounds_same_key_waiters_and_recovers_after_cancellation() {
        let backend = Arc::new(TestBackend::new(vec![]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1],
        });
        let rpc = test_rpc(backend.clone(), builder, 64, 1);
        let execution = rpc.concurrency.acquire().await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let rpc = rpc.clone();
            tasks.spawn(async move { rpc.request_airdrop(test_request(1, 1)).await });
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            // Admission precedes journal registration on a blocking worker.
            // Wait for both before checking the retained attempt or cancelling callers.
            while rpc.admission.available_permits() != 0 || rpc.attempts.lock().unwrap().len() != 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let error = rpc.request_airdrop(test_request(2, 2)).await.unwrap_err();
        assert_eq!(
            error.code(),
            groundhog_json_rpc_types::error::codes::OVERLOADED
        );
        assert_eq!(rpc.attempts.lock().unwrap().len(), 1);
        assert!(backend.submissions.lock().unwrap().is_empty());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        assert_eq!(rpc.admission.available_permits(), 16);
        drop(execution);
        rpc.request_airdrop(test_request(1, 1)).await.unwrap();
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deadline_covers_queue_and_submission_and_retains_signed_bytes() {
        let backend =
            Arc::new(TestBackend::new(vec![]).with_submit_delay(Duration::from_millis(50)));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![9, 8, 7],
        });
        let mut rpc = test_rpc(backend.clone(), builder.clone(), 2, 1);
        rpc.timeout = Duration::from_millis(10);
        let execution = rpc.concurrency.acquire().await.unwrap();
        assert!(matches!(
            rpc.request(test_request(1, 1)).await,
            Err(RpcServerError::BackendTimeout)
        ));
        assert_eq!(rpc.admission.available_permits(), 16);
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 0);
        drop(execution);
        // Both registered requests remain retained after timeout.
        assert!(matches!(
            rpc.request(test_request(2, 2)).await,
            Err(RpcServerError::BackendTimeout)
        ));
        assert_eq!(rpc.concurrency.available_permits(), 1);
        rpc.timeout = Duration::from_secs(1);
        rpc.request_airdrop(test_request(2, 2)).await.unwrap();
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(*backend.submissions.lock().unwrap(), vec![vec![9, 8, 7]; 2]);
    }

    #[tokio::test]
    async fn execution_receives_the_deadline_started_before_queueing() {
        let backend = Arc::new(TestBackend::new(vec![]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1],
        });
        let rpc = test_rpc(backend.clone(), builder, 1, 1);
        let execution = rpc.concurrency.acquire().await.unwrap();
        let task = tokio::spawn({
            let rpc = rpc.clone();
            async move { rpc.request_airdrop(test_request(1, 1)).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while rpc.admission.available_permits() == 16 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let released_at = std::time::Instant::now();
        drop(execution);
        task.await.unwrap().unwrap();
        let contexts = backend.contexts.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert!(contexts[0].deadline().unwrap() <= released_at + rpc.timeout);
        assert!(contexts[0].started_at() <= released_at);
    }

    #[test]
    fn local_context_rejects_wrong_faucet_role() {
        let config = NodeSupervisorConfig::default();
        let mut manifest = runtime_manifest(&config);
        manifest.bootstrap_accounts[0].role = "local_user".into();

        let error = LocalAirdropContext::from_validated_runtime(
            &manifest,
            &config,
            fixture_signer(),
            fixture_journal(),
            None,
        )
        .err()
        .expect("wrong dev-0 role must fail validation");

        assert!(error.contains("missing its local faucet"));
    }

    #[test]
    fn local_context_rejects_multiple_faucets() {
        let config = NodeSupervisorConfig::default();
        let mut manifest = runtime_manifest(&config);
        manifest
            .bootstrap_accounts
            .push(manifest.bootstrap_accounts[0].clone());
        let error = LocalAirdropContext::from_validated_runtime(
            &manifest,
            &config,
            fixture_signer(),
            fixture_journal(),
            None,
        )
        .err()
        .unwrap();
        assert!(error.contains("multiple local faucets"));
    }

    #[test]
    fn local_context_rejects_wrong_faucet_public_key() {
        let config = NodeSupervisorConfig::default();
        let mut manifest = runtime_manifest(&config);
        manifest.bootstrap_accounts[0].public_key =
            Hex0x(groundhog_xdr::types::Ed25519PublicKey::from([0x99; 32]));

        let error = LocalAirdropContext::from_validated_runtime(
            &manifest,
            &config,
            fixture_signer(),
            fixture_journal(),
            None,
        )
        .err()
        .expect("wrong faucet public key must fail validation");

        assert!(error.contains("public key does not match"));
    }

    #[test]
    fn local_context_rejects_non_native_faucet_address() {
        let config = NodeSupervisorConfig::default();
        let mut manifest = runtime_manifest(&config);
        manifest.bootstrap_accounts[0].wallet_address = Hex0x([0; 32].into());

        let error = LocalAirdropContext::from_validated_runtime(
            &manifest,
            &config,
            fixture_signer(),
            fixture_journal(),
            None,
        )
        .err()
        .expect("non-native faucet address must fail validation");

        assert!(error.contains("address is not its nonce-zero native account"));
    }

    #[tokio::test]
    async fn concurrent_same_key_constructs_and_submits_once() {
        let backend =
            Arc::new(TestBackend::new(vec![Ok(())]).with_submit_delay(Duration::from_millis(25)));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1, 2, 3],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(1, 1);

        let (a, b, c, d) = tokio::join!(
            rpc.request(request.clone()),
            rpc.request(request.clone()),
            rpc.request(request.clone()),
            rpc.request(request),
        );

        for result in [a, b, c, d] {
            assert!(matches!(result, Ok(AirdropAttemptResponse::Pending { .. })));
        }
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn ambiguous_retry_resubmits_exact_signed_bytes_without_reconstruction() {
        let backend = Arc::new(
            TestBackend::new(vec![
                Err(RpcServerError::BackendUnavailable("response lost".into())),
                Ok(()),
            ])
            .with_status_results(vec![Err(RpcServerError::BackendUnavailable(
                "status unavailable".into(),
            ))]),
        );
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![9, 8, 7, 6],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(2, 2);

        let first = rpc.request(request.clone()).await;
        let second = rpc.request(request).await;

        assert!(matches!(first, Err(RpcServerError::BackendUnavailable(_))));
        assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available"),
            vec![vec![9, 8, 7, 6], vec![9, 8, 7, 6]]
        );
    }

    #[tokio::test]
    async fn deterministic_submit_rejection_is_terminal_without_status_reconciliation() {
        let backend = Arc::new(
            TestBackend::new(vec![Err(RpcServerError::TxRejected("expired".into()))])
                .with_status_results(vec![Err(RpcServerError::BackendUnavailable(
                    "status unavailable".into(),
                ))]),
        );
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![2, 4, 6],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(12, 12);

        let first = rpc.request(request.clone()).await;
        let second = rpc.request(request).await;

        assert!(matches!(first, Err(RpcServerError::TxRejected(_))));
        assert!(matches!(second, Err(RpcServerError::TxRejected(_))));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_pending_retry_polls_without_resubmitting() {
        let backend = Arc::new(TestBackend::new(vec![Ok(())]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![3, 2, 1],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(7, 7);

        let first = rpc.request(request.clone()).await;
        let second = rpc.request(request).await;

        assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
        assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_unknown_retry_resubmits_exact_signed_bytes_without_reconstruction() {
        let backend = Arc::new(
            TestBackend::new(vec![Ok(()), Ok(())]).with_status_results(vec![
                Ok(TransactionStatus::Unknown),
                Ok(TransactionStatus::Pending),
            ]),
        );
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![4, 3, 2, 1],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(13, 13);

        let first = rpc.request(request.clone()).await;
        let second = rpc.request(request).await;

        assert!(matches!(
            first,
            Ok(AirdropAttemptResponse::Pending {
                tx_hash: Some(hash),
                ..
            }) if hash == test_tx_hash()
        ));
        assert!(matches!(
            second,
            Ok(AirdropAttemptResponse::Pending {
                tx_hash: Some(hash),
                ..
            }) if hash == test_tx_hash()
        ));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available"),
            vec![vec![4, 3, 2, 1], vec![4, 3, 2, 1]]
        );
    }

    #[tokio::test]
    async fn unknown_retry_with_ambiguous_submit_error_can_retry_again() {
        let backend = Arc::new(
            TestBackend::new(vec![
                Ok(()),
                Err(RpcServerError::BackendUnavailable("response lost".into())),
                Ok(()),
            ])
            .with_status_results(vec![
                Ok(TransactionStatus::Unknown),
                Ok(TransactionStatus::Unknown),
                Ok(TransactionStatus::Pending),
            ]),
        );
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![6, 7, 8, 9],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(14, 14);

        let first = rpc.request(request.clone()).await;
        let second = rpc.request(request.clone()).await;
        let third = rpc.request(request).await;

        assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
        assert!(matches!(second, Err(RpcServerError::BackendUnavailable(_))));
        assert!(matches!(third, Ok(AirdropAttemptResponse::Pending { .. })));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available"),
            vec![vec![6, 7, 8, 9]; 3]
        );
    }

    #[tokio::test]
    async fn recent_failure_retry_resubmits_exact_signed_bytes() {
        let backend = Arc::new(
            TestBackend::new(vec![Ok(()), Ok(())]).with_status_results(vec![Ok(
                TransactionStatus::RecentFailure {
                    attempt_height: BlockHeight::from(8),
                    cause:
                        groundhog_json_rpc_types::transaction::TransactionFailureCause::DbConflict {
                            conflict: ConflictDiagnostic {
                                address: Hex0x(groundhog_xdr::types::ContractAddress::from(
                                    [0x44; 32],
                                )),
                                key: HexBytes(vec![0x01]),
                                code: ConflictCode::BoundsExceeded,
                            },
                        },
                },
            )]),
        );
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![5, 4, 3],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(10, 10);

        let first = rpc.request(request.clone()).await;
        let second = rpc.request(request).await;

        assert!(matches!(first, Err(RpcServerError::BackendUnavailable(_))));
        assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available"),
            vec![vec![5, 4, 3], vec![5, 4, 3]]
        );
    }

    #[tokio::test]
    async fn registered_unsigned_attempt_consumes_capacity_and_can_resume() {
        let backend = Arc::new(TestBackend::new(vec![Ok(())]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1, 2, 3],
        });
        let rpc = test_rpc(backend, builder, 1, 1);
        let first = test_request(1, 1);
        rpc.find_or_admit(FundAccountRequest {
            recipient: first.recipient,
            client_nonce: first.client_nonce.clone(),
            raw_amount: rpc.raw_amount.to_string(),
            faucet: rpc.faucet,
        })
        .await
        .unwrap();
        assert!(matches!(
            rpc.request(test_request(2, 2)).await,
            Err(RpcServerError::Overloaded(_))
        ));
        assert!(rpc.request(first).await.is_ok());
    }

    #[tokio::test]
    async fn terminal_attempt_remains_retained_at_capacity() {
        let backend = Arc::new(TestBackend::new(vec![Err(RpcServerError::TxRejected(
            "rejected".into(),
        ))]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1, 2, 3],
        });
        let rpc = test_rpc(backend.clone(), builder, 1, 1);
        let first = test_request(1, 1);
        assert!(matches!(
            rpc.request(first.clone()).await,
            Err(RpcServerError::TxRejected(_))
        ));
        assert!(matches!(
            rpc.request(test_request(2, 2)).await,
            Err(RpcServerError::Overloaded(_))
        ));
        assert!(matches!(
            rpc.request(first).await,
            Err(RpcServerError::TxRejected(_))
        ));
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ambiguous_submission_confirmed_pending_wakes_block_driver() {
        let backend = Arc::new(
            TestBackend::new(vec![Err(RpcServerError::BackendUnavailable(
                "response lost".into(),
            ))])
            .with_status_results(vec![Ok(TransactionStatus::Pending)]),
        );
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![4, 3, 2, 1],
        });
        let notify = Arc::new(tokio::sync::Notify::new());
        let rpc = test_rpc_with_trigger(
            backend.clone(),
            builder,
            8,
            4,
            BlockDriverTrigger::new(notify.clone()),
        );

        let result = rpc.request(test_request(9, 9)).await;

        assert!(matches!(result, Ok(AirdropAttemptResponse::Pending { .. })));
        tokio::time::timeout(Duration::from_millis(10), notify.notified())
            .await
            .expect("pending reconciliation should wake instant block production");
        assert_eq!(
            backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_submission_retry_reuses_exact_signed_bytes() {
        let backend =
            Arc::new(TestBackend::new(vec![Ok(())]).with_submit_delay(Duration::from_millis(100)));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![6, 5, 4],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
        let request = test_request(8, 8);
        let task = tokio::spawn({
            let rpc = rpc.clone();
            let request = request.clone();
            async move { rpc.request(request).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        task.abort();
        let _ = task.await;

        let retry = rpc.request(request).await;

        assert!(matches!(retry, Ok(AirdropAttemptResponse::Pending { .. })));
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend
                .submissions
                .lock()
                .expect("bug: submissions lock should be available"),
            vec![vec![6, 5, 4], vec![6, 5, 4]]
        );
    }

    #[tokio::test]
    async fn unresolved_attempt_prevents_capacity_eviction() {
        let backend = Arc::new(TestBackend::new(vec![Err(
            RpcServerError::BackendUnavailable("response lost".into()),
        )]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![4, 5, 6],
        });
        let rpc = test_rpc(backend, builder, 1, 1);

        let first = rpc.request(test_request(3, 3)).await;
        let second = rpc.request(test_request(4, 4)).await;

        assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
        assert!(matches!(second, Err(RpcServerError::Overloaded(_))));
    }

    fn funding_request(raw_amount: &str) -> FundAccountRequest {
        let request = test_request(0x44, 9);
        FundAccountRequest {
            recipient: request.recipient,
            client_nonce: request.client_nonce,
            raw_amount: raw_amount.into(),
            faucet: Hex0x(
                PubkeyAccount::from_public_key_bytes(fixture_signer().verifying_key_bytes(), 0)
                    .address(),
            ),
        }
    }

    fn durable_rpc(
        dir: &std::path::Path,
        fresh: bool,
        backend: Arc<TestBackend>,
    ) -> LocalAirdropRpc {
        let signer = fixture_signer();
        let faucet = funding_request("42").faucet;
        let journal = FundingJournal::open(dir, [0; 32].into(), faucet, fresh, &signer).unwrap();
        let mut rpc = LocalAirdropRpc::with_parts(
            backend,
            Arc::new(NativeAirdropBuilder {
                network_hash: groundhog_json_rpc_types::NetworkHashHex::from([0; 32])
                    .to_network_hash(),
                signer,
                next_nonce: AtomicU64::new(42),
            }),
            LocalAirdropConfig {
                attempt_capacity: 1,
                ..LocalAirdropConfig::default()
            },
            BlockDriverTrigger::noop(),
        );
        rpc.journal = Arc::new(std::sync::Mutex::new(journal));
        rpc.faucet = faucet;
        rpc
    }

    #[tokio::test]
    async fn local_funding_admits_explicit_amount_before_production_and_retains_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![Ok(())]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request("123456");
        let response = rpc.fund(request.clone()).await.unwrap();
        assert_eq!(response.raw_amount, "123456");
        assert_eq!(
            backend.submissions.lock().unwrap().as_slice(),
            std::slice::from_ref(&response.signed_transaction.0)
        );
        // No receipt exists yet: admission preserves submit -> produce -> receipt.
        drop(rpc);
        let restarted_backend = Arc::new(TestBackend::new(vec![]));
        let restarted = durable_rpc(dir.path(), false, restarted_backend.clone());
        assert_eq!(restarted.fund(request.clone()).await.unwrap(), response);
        assert!(restarted_backend.submissions.lock().unwrap().is_empty());
        let mut conflict = request.clone();
        conflict.raw_amount = "123457".into();
        assert!(matches!(
            restarted.fund(conflict).await,
            Err(RpcServerError::InvalidParam(_))
        ));
        let mut wrong_chain = request.clone();
        wrong_chain.faucet = Hex0x([0; 32].into());
        assert!(matches!(
            restarted.fund(wrong_chain).await,
            Err(RpcServerError::InvalidParam(_))
        ));
        let mut another = request;
        another.client_nonce = ClientNonce::new(vec![10]).unwrap();
        assert!(matches!(
            restarted.fund(another).await,
            Err(RpcServerError::Overloaded(_))
        ));
    }

    #[tokio::test]
    async fn lost_admission_response_reuses_exact_retained_bytes_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(
            TestBackend::new(vec![Err(RpcServerError::BackendUnavailable(
                "lost response".into(),
            ))])
            .with_status_results(vec![Ok(TransactionStatus::Unknown)]),
        );
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request("42");
        assert!(matches!(
            rpc.fund(request.clone()).await,
            Err(RpcServerError::LocalFundingUnresolved { .. })
        ));
        let retained = backend.submissions.lock().unwrap()[0].clone();
        drop(rpc);
        let restarted_backend = Arc::new(
            TestBackend::new(vec![Ok(())])
                .with_status_results(vec![Ok(TransactionStatus::Unknown)]),
        );
        let restarted = durable_rpc(dir.path(), false, restarted_backend.clone());
        let response = restarted.fund(request).await.unwrap();
        assert_eq!(response.signed_transaction.0, retained);
        assert_eq!(restarted_backend.submissions.lock().unwrap()[0], retained);
    }

    #[tokio::test]
    async fn expired_retained_funding_without_outcome_never_creates_another_transfer() {
        let dir = tempfile::tempdir().unwrap();
        let rpc = durable_rpc(dir.path(), true, Arc::new(TestBackend::new(vec![])));
        let request = funding_request("42");
        let signed = NativeAirdropBuilder {
            network_hash: groundhog_json_rpc_types::NetworkHashHex::from([0; 32]).to_network_hash(),
            signer: fixture_signer(),
            next_nonce: AtomicU64::new(7),
        }
        .build(request.recipient, 6, 42)
        .unwrap();
        {
            let mut journal = rpc.journal.lock().unwrap();
            journal.register(request.clone()).unwrap();
            journal
                .retain_signed(
                    &request,
                    RetainedFundingTransaction {
                        tx_hash: signed.tx_hash,
                        signed_transaction: groundhog_json_rpc_types::hex::HexBytes(signed.bytes),
                    },
                )
                .unwrap();
        }
        drop(rpc);
        let backend = Arc::new(
            TestBackend::new(vec![]).with_status_results(vec![Ok(TransactionStatus::Unknown)]),
        );
        let restarted = durable_rpc(dir.path(), false, backend.clone());
        let error = restarted.fund(request).await.unwrap_err();
        assert!(error.to_string().contains("expired"));
        let object: jsonrpsee::types::ErrorObjectOwned = error.into();
        assert_eq!(
            object.code(),
            groundhog_json_rpc_types::error::codes::LOCAL_FUNDING_UNRESOLVED
        );
        let evidence: LocalFundingErrorData =
            serde_json::from_str(object.data().unwrap().get()).unwrap();
        assert_eq!(evidence.tx_hash, signed.tx_hash);
        let original = FundingJournal::open(
            dir.path(),
            [0; 32].into(),
            funding_request("42").faucet,
            false,
            &fixture_signer(),
        )
        .unwrap();
        assert_eq!(
            evidence.signed_transaction,
            original.records()[0]
                .signed
                .as_ref()
                .unwrap()
                .signed_transaction
        );
        assert!(backend.submissions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn corrupt_retained_intent_fails_startup_and_failed_persistence_never_broadcasts() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request("42");
        rpc.fund(request.clone()).await.unwrap();
        drop(rpc);
        let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
        let mut data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data["records"][0]["request"]["rawAmount"] = serde_json::json!("43");
        std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(
            FundingJournal::open(
                dir.path(),
                [0; 32].into(),
                request.faucet,
                false,
                &fixture_signer()
            )
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        // A directory at the journal path makes its atomic replacement fail.
        let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(rpc.fund(request).await.is_err());
        assert!(backend.submissions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_signed_persistence_does_not_expose_restart_unstable_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request("42");
        rpc.find_or_admit(request.clone()).await.unwrap();
        let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let error = rpc.fund(request.clone()).await.unwrap_err();
        assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
        let journal = rpc.journal.lock().unwrap();
        assert!(journal.records()[0].signed.is_some());
        assert!(journal.durable_signed(&request).is_none());
        assert!(backend.submissions.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_persistence_respects_deadline_without_broadcast_or_runtime_stall() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let mut rpc = durable_rpc(dir.path(), true, backend.clone());
        rpc.timeout = Duration::from_millis(20);
        let journal = rpc.journal.clone();
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let holder = tokio::task::spawn_blocking(move || {
            let _held = journal.lock().unwrap();
            ready_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        });
        ready_receiver.await.unwrap();
        let response =
            tokio::time::timeout(Duration::from_secs(1), rpc.fund(funding_request("42")))
                .await
                .unwrap();
        assert!(matches!(response, Err(RpcServerError::BackendTimeout)));
        assert!(backend.submissions.lock().unwrap().is_empty());
        release_sender.send(()).unwrap();
        holder.await.unwrap();
        // Cancellation leaves only registered intent; retry can advance it safely.
        rpc.timeout = Duration::from_secs(1);
        assert!(rpc.fund(funding_request("42")).await.is_ok());
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unchanged_durable_retry_does_not_require_another_journal_write() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request("42");
        let admitted = rpc.fund(request.clone()).await.unwrap();
        let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(rpc.fund(request).await.unwrap(), admitted);
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_persistence_holds_chain_lock_and_bounds_workers_until_completion() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::HogdDevConfig::default().with_data_dir(dir.path());
        let state = crate::local_state::LocalChainState::prepare(&config).unwrap();
        let intent = config.to_bootstrap_manifest([0; 32].into(), state.public_key());
        std::fs::write(dir.path().join("hog-devnode-data.json"), serde_json::to_vec(&serde_json::json!({
            "schemaVersion":4, "kind":"hog-devnode-data", "bootstrapConfig": {
                "bootstrapFaucet": intent.bootstrap_faucet, "bootstrapAccounts": intent.bootstrap_accounts
            }
        })).unwrap()).unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1],
        });
        let mut rpc = test_rpc(backend.clone(), builder, 8, 1);
        rpc.journal = state.journal.clone();
        rpc.lock_lease = state.lock.clone();
        rpc.faucet = Hex0x(
            PubkeyAccount::from_public_key_bytes(state.signer.verifying_key_bytes(), 0).address(),
        );
        rpc.timeout = Duration::from_millis(20);
        let request = FundAccountRequest {
            faucet: rpc.faucet,
            ..funding_request("42")
        };
        let journal = rpc.journal.clone();
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let holder = tokio::task::spawn_blocking(move || {
            let _held = journal.lock().unwrap();
            ready_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        });
        ready_receiver.await.unwrap();
        for _ in 0..3 {
            assert!(matches!(
                rpc.fund(request.clone()).await,
                Err(RpcServerError::BackendTimeout)
            ));
            assert_eq!(
                rpc.persistence.available_permits(),
                0,
                "only one detached worker is admitted"
            );
        }
        assert!(backend.submissions.lock().unwrap().is_empty());
        // Simulate shutdown dropping both owners while the cancelled worker remains.
        drop(state);
        drop(rpc);
        assert!(crate::local_state::LocalChainState::prepare(&config).is_err());
        release_sender.send(()).unwrap();
        holder.await.unwrap();
        let recovered = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(state) = crate::local_state::LocalChainState::prepare(&config) {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(recovered.journal.lock().unwrap().records().len(), 1);
    }

    fn test_rpc(
        backend: Arc<TestBackend>,
        builder: Arc<CountingBuilder>,
        capacity: usize,
        concurrency: usize,
    ) -> LocalAirdropRpc {
        test_rpc_with_trigger(
            backend,
            builder,
            capacity,
            concurrency,
            BlockDriverTrigger::noop(),
        )
    }

    fn test_rpc_with_trigger(
        backend: Arc<TestBackend>,
        builder: Arc<CountingBuilder>,
        capacity: usize,
        concurrency: usize,
        block_driver: BlockDriverTrigger,
    ) -> LocalAirdropRpc {
        LocalAirdropRpc::with_parts(
            backend,
            builder,
            LocalAirdropConfig {
                raw_amount: crate::config::DEFAULT_LOCAL_AIRDROP_RAW_AMOUNT,
                attempt_capacity: capacity,
                max_concurrency: concurrency,
                request_timeout: Duration::from_secs(30),
            },
            block_driver,
        )
    }

    fn test_request(recipient: u8, nonce: u8) -> AirdropRequest {
        AirdropRequest {
            recipient: Hex0x(groundhog_xdr::types::ContractAddress::from([recipient; 32])),
            client_nonce: ClientNonce::new(vec![nonce])
                .expect("bug: one-byte nonce should validate"),
        }
    }

    fn test_tx_hash() -> TxHash {
        TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0xaa; 32])))
    }

    fn runtime_manifest(_config: &NodeSupervisorConfig) -> RuntimeManifest {
        let signer = fixture_signer();
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 4,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": { "binary": "hogd-dev", "version": "0.1.0" },
            "endpoints": { "scheduler": "127.0.0.1:1000" },
            "bootstrapAccounts": [{
                "role": "faucet",
                "publicKey": format!("0x{}", hex::encode(signer.verifying_key_bytes())),
                "walletAddress": format!("0x{}", hex::encode(PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address().as_ref())),
                "initialStorageTokens": "100000000",
            }],
            "bootstrap": {
                "storageCostModel": {
                    "hash": format!("0x{}", "53".repeat(32)),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234",
                },
            },
        }))
        .expect("bug: runtime manifest fixture should deserialize")
    }
    fn fixture_signer() -> Arc<LocalSigner> {
        Arc::new(LocalSigner::from_secret_key([0x33; 32]))
    }
    fn fixture_journal() -> Arc<std::sync::Mutex<FundingJournal>> {
        Arc::new(std::sync::Mutex::new(FundingJournal::memory(
            [0; 32].into(),
            Hex0x(
                PubkeyAccount::from_public_key_bytes(fixture_signer().verifying_key_bytes(), 0)
                    .address(),
            ),
        )))
    }
}
