use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
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
    network_info::NetworkInfo,
    receipt::ExecStatus,
    runtime_manifest::{BOOTSTRAP_ACCOUNT_ROLE_FAUCET, RuntimeManifest},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_signing::{
    BuildSignedDevWalletHogTransfer, DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams,
    build_signed_dev_wallet_hog_transfer, derive_dev_account_signer,
};
use jsonrpsee::core::RpcResult;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    LocalAirdropConfig, NodeSupervisorConfig, block_driver::BlockDriverTrigger,
    request_deadline::within_request_deadline,
};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;
const LOCAL_AIRDROP_RETRY_AFTER_SECONDS: u64 = 1;

#[derive(Clone)]
pub(crate) struct LocalAirdropContext {
    network_info: NetworkInfo,
    wallet_address: groundhog_xdr::types::ContractAddress,
    wallet_entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    signer: Arc<LocalSigner>,
    config: LocalAirdropConfig,
}

impl LocalAirdropContext {
    pub(crate) fn from_validated_runtime(
        manifest: &RuntimeManifest,
        config: &NodeSupervisorConfig,
        network_info: &NetworkInfo,
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
        let wallet_entrypoint = manifest
            .bootstrap
            .dev_wallet
            .do_action_entrypoint_id()
            .map_err(|error| format!("invalid runtime dev-wallet doAction entrypoint: {error}"))?;
        Ok(Self {
            network_info: network_info.clone(),
            wallet_address: faucet.wallet_address.0,
            wallet_entrypoint,
            signer: Arc::new(signer),
            config: config.local_airdrop,
        })
    }
}

#[derive(Clone)]
pub(crate) struct LocalAirdropRpc {
    backend: Arc<dyn AirdropBackend>,
    builder: Arc<dyn AirdropTransactionBuilder>,
    network_info: NetworkInfo,
    attempts: Arc<Mutex<HashMap<AttemptKey, Arc<Attempt>>>>,
    capacity: usize,
    concurrency: Arc<Semaphore>,
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
        let builder = DevWalletAirdropBuilder {
            network_hash: context.network_info.network_hash.0,
            wallet_address: context.wallet_address,
            wallet_entrypoint: context.wallet_entrypoint,
            signer: context.signer,
            next_nonce: AtomicU64::new(nonce_from_timestamp()),
            raw_amount: context.config.raw_amount,
        };
        Self::with_parts(
            context.network_info,
            Arc::new(PlaneAirdropBackend { submit_chain, read }),
            Arc::new(builder),
            context.config,
            block_driver,
        )
    }

    fn with_parts(
        network_info: NetworkInfo,
        backend: Arc<dyn AirdropBackend>,
        builder: Arc<dyn AirdropTransactionBuilder>,
        config: LocalAirdropConfig,
        block_driver: BlockDriverTrigger,
    ) -> Self {
        Self {
            backend,
            builder,
            network_info,
            attempts: Arc::new(Mutex::new(HashMap::new())),
            capacity: config.attempt_capacity,
            concurrency: Arc::new(Semaphore::new(config.max_concurrency)),
            timeout: config.request_timeout,
            raw_amount: config.raw_amount,
            block_driver,
        }
    }

    async fn request(
        &self,
        request: AirdropRequest,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let key = AttemptKey::new(&self.network_info, &request);
        let (attempt, admission_permit) = self.find_or_admit(key)?;
        let observed_revision = attempt.revision.load(Ordering::Acquire);
        let _operation = attempt.operation.lock().await;

        if attempt.revision.load(Ordering::Acquire) != observed_revision {
            return attempt.last_result()?.ok_or_else(|| {
                RpcServerError::BackendUnavailable(
                    "local airdrop attempt completed without an outcome".into(),
                )
            })?;
        }
        if attempt.terminal.load(Ordering::Acquire) {
            return attempt.last_result()?.ok_or_else(|| {
                RpcServerError::BackendUnavailable(
                    "terminal local airdrop attempt has no outcome".into(),
                )
            })?;
        }

        let permit = match admission_permit {
            Some(permit) => permit,
            None => self.concurrency.clone().try_acquire_owned().map_err(|_| {
                RpcServerError::Overloaded("local airdrop concurrency limit reached".into())
            })?,
        };
        let result = self.advance_attempt(&attempt, request.recipient).await;
        drop(permit);
        attempt.record_result(result.clone());
        result
    }

    fn find_or_admit(
        &self,
        key: AttemptKey,
    ) -> Result<(Arc<Attempt>, Option<OwnedSemaphorePermit>), RpcServerError> {
        let mut attempts = self.attempts.lock().map_err(|_| {
            RpcServerError::BackendUnavailable("local airdrop attempt store is poisoned".into())
        })?;
        if let Some(attempt) = attempts.get(&key) {
            return Ok((attempt.clone(), None));
        }
        if attempts.len() >= self.capacity
            && let Some(terminal_key) = attempts
                .iter()
                .find(|(_, attempt)| attempt.is_evictable())
                .map(|(key, _)| key.clone())
        {
            attempts.remove(&terminal_key);
        }
        if attempts.len() >= self.capacity {
            return Err(RpcServerError::Overloaded(
                "local airdrop attempt capacity reached".into(),
            ));
        }
        let permit = self.concurrency.clone().try_acquire_owned().map_err(|_| {
            RpcServerError::Overloaded("local airdrop concurrency limit reached".into())
        })?;
        let attempt = Arc::new(Attempt::new());
        attempts.insert(key, attempt.clone());
        Ok((attempt, Some(permit)))
    }

    async fn advance_attempt(
        &self,
        attempt: &Attempt,
        recipient: ContractAddress,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let context = RpcRequestContext::new(Some(self.timeout));
        if attempt.signed()?.is_none() {
            let tip = self.backend.chain_tip(context).await?;
            let signed = match self.builder.build(
                recipient,
                (*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET),
            ) {
                Ok(signed) => signed,
                Err(error) => {
                    attempt.terminal.store(true, Ordering::Release);
                    return Err(error);
                }
            };
            attempt.store_signed(signed)?;
        }
        let signed = attempt.signed()?.ok_or_else(|| {
            RpcServerError::BackendUnavailable("local airdrop transaction was not retained".into())
        })?;

        let mut submit_error = if attempt.submitted.load(Ordering::Acquire) {
            None
        } else {
            match self.backend.submit(context, &signed).await {
                Ok(()) => {
                    attempt.submitted.store(true, Ordering::Release);
                    None
                }
                Err(error) => Some(classify_backend_error(error)),
            }
        };
        if let Some(error) = submit_error.take_if(|error| is_deterministic_failure(error)) {
            attempt.terminal.store(true, Ordering::Release);
            return Err(error);
        }

        match self.backend.receipt(context, signed.tx_hash).await {
            Ok(Some(receipt)) if matches!(receipt.status, ExecStatus::Ok) => {
                attempt.terminal.store(true, Ordering::Release);
                Ok(AirdropAttemptResponse::Succeeded {
                    recipient,
                    tx_hash: signed.tx_hash,
                    raw_amount: self.raw_amount.to_string(),
                    height: receipt.height,
                    status: receipt.status,
                })
            }
            Ok(Some(receipt)) => {
                attempt.terminal.store(true, Ordering::Release);
                Err(RpcServerError::TxRejected(format!(
                    "local airdrop execution failed: {:?}",
                    receipt.status
                )))
            }
            Ok(None) => match self.backend.status(context, signed.tx_hash).await {
                Ok(TransactionStatus::Invalid { reason }) => {
                    attempt.terminal.store(true, Ordering::Release);
                    Err(RpcServerError::TxRejected(reason))
                }
                Ok(TransactionStatus::Pending) => {
                    if submit_error.is_some() {
                        self.block_driver.trigger_after_submit();
                    }
                    attempt.submitted.store(true, Ordering::Release);
                    Ok(AirdropAttemptResponse::Pending {
                        recipient,
                        raw_amount: self.raw_amount.to_string(),
                        tx_hash: Some(signed.tx_hash),
                        retry_after_seconds: LOCAL_AIRDROP_RETRY_AFTER_SECONDS,
                    })
                }
                Ok(TransactionStatus::RecentFailure { .. }) => {
                    attempt.submitted.store(false, Ordering::Release);
                    Err(RpcServerError::BackendUnavailable(
                        "local airdrop has a recent nondeterministic failure and may still commit"
                            .into(),
                    ))
                }
                Ok(_) => match submit_error {
                    Some(error) => Err(error),
                    None => Ok(AirdropAttemptResponse::Pending {
                        recipient,
                        raw_amount: self.raw_amount.to_string(),
                        tx_hash: Some(signed.tx_hash),
                        retry_after_seconds: LOCAL_AIRDROP_RETRY_AFTER_SECONDS,
                    }),
                },
                Err(error) => Err(submit_error.unwrap_or_else(|| classify_backend_error(error))),
            },
            Err(error) => Err(classify_backend_error(error)),
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
    network_id: String,
    genesis_hash: [u8; 32],
    recipient: [u8; 32],
    client_nonce: Vec<u8>,
}

impl AttemptKey {
    fn new(network_info: &NetworkInfo, request: &AirdropRequest) -> Self {
        Self {
            network_id: network_info.network_id.clone(),
            genesis_hash: network_info.genesis_hash.into_inner(),
            recipient: request.recipient.0.into_inner(),
            client_nonce: request.client_nonce.as_bytes().to_vec(),
        }
    }
}

struct Attempt {
    operation: tokio::sync::Mutex<()>,
    signed: Mutex<Option<SignedAirdrop>>,
    last_result: Mutex<Option<Result<AirdropAttemptResponse, RpcServerError>>>,
    revision: AtomicU64,
    terminal: AtomicBool,
    submitted: AtomicBool,
}

impl Attempt {
    fn new() -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            signed: Mutex::new(None),
            last_result: Mutex::new(None),
            revision: AtomicU64::new(0),
            terminal: AtomicBool::new(false),
            submitted: AtomicBool::new(false),
        }
    }

    fn signed(&self) -> Result<Option<SignedAirdrop>, RpcServerError> {
        self.signed
            .lock()
            .map(|signed| signed.clone())
            .map_err(|_| {
                RpcServerError::BackendUnavailable(
                    "local airdrop signed transaction store is poisoned".into(),
                )
            })
    }

    fn store_signed(&self, signed: SignedAirdrop) -> Result<(), RpcServerError> {
        let mut slot = self.signed.lock().map_err(|_| {
            RpcServerError::BackendUnavailable(
                "local airdrop signed transaction store is poisoned".into(),
            )
        })?;
        if slot.is_some() {
            return Err(RpcServerError::BackendUnavailable(
                "local airdrop transaction was constructed more than once".into(),
            ));
        }
        *slot = Some(signed);
        Ok(())
    }

    fn last_result(
        &self,
    ) -> Result<Option<Result<AirdropAttemptResponse, RpcServerError>>, RpcServerError> {
        self.last_result
            .lock()
            .map(|result| result.clone())
            .map_err(|_| {
                RpcServerError::BackendUnavailable("local airdrop outcome store is poisoned".into())
            })
    }

    fn record_result(&self, result: Result<AirdropAttemptResponse, RpcServerError>) {
        match self.last_result.lock() {
            Ok(mut last_result) => *last_result = Some(result),
            Err(error) => tracing::error!(%error, "local airdrop outcome store is poisoned"),
        }
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn is_evictable(&self) -> bool {
        if !self.terminal.load(Ordering::Acquire) {
            return false;
        }
        self.last_result
            .lock()
            .is_ok_and(|last_result| last_result.is_some())
    }
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

struct DevWalletAirdropBuilder {
    network_hash: groundhog_xdr::types::NetworkHash,
    wallet_address: groundhog_xdr::types::ContractAddress,
    wallet_entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    signer: Arc<LocalSigner>,
    next_nonce: AtomicU64,
    raw_amount: u128,
}

impl AirdropTransactionBuilder for DevWalletAirdropBuilder {
    fn build(
        &self,
        recipient: ContractAddress,
        block_expiry_height: i64,
    ) -> Result<SignedAirdrop, RpcServerError> {
        let signed = build_signed_dev_wallet_hog_transfer(
            BuildSignedDevWalletHogTransfer {
                wallet_address: self.wallet_address,
                wallet_entrypoint: self.wallet_entrypoint,
                recipient: recipient.0,
                raw_amount: self.raw_amount,
                gas_limit: DEFAULT_TX_GAS_LIMIT,
                block_expiry_height,
                nonce: self.next_nonce.fetch_add(1, Ordering::Relaxed),
                sign_params: SignTransactionParams::default(),
                network_hash: self.network_hash,
            },
            self.signer.as_ref(),
        )
        .map_err(|error| {
            RpcServerError::BackendUnavailable(format!(
                "failed to construct local airdrop transaction: {error}"
            ))
        })?;
        Ok(SignedAirdrop {
            bytes: signed.bytes,
            tx_hash: TxHash(Hex0x(signed.tx_hash)),
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
        within_request_deadline(context, self.submit_chain.chain_tip(context)).await
    }

    async fn submit(
        &self,
        context: RpcRequestContext,
        signed: &SignedAirdrop,
    ) -> Result<(), RpcServerError> {
        let tx = validate_raw_transaction_bytes(signed.bytes.clone()).map_err(|error| {
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
    }

    impl TestBackend {
        fn new(submit_results: Vec<Result<(), RpcServerError>>) -> Self {
            Self {
                submit_results: Mutex::new(submit_results.into()),
                status_results: Mutex::new(VecDeque::new()),
                submissions: Mutex::new(Vec::new()),
                submit_delay: Duration::ZERO,
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
        async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
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
    async fn recent_failure_retry_resubmits_exact_signed_bytes() {
        let backend = Arc::new(
            TestBackend::new(vec![Ok(()), Ok(())]).with_status_results(vec![Ok(
                TransactionStatus::RecentFailure {
                    attempt_height: BlockHeight::from(8),
                    conflict: ConflictDiagnostic {
                        address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x44; 32])),
                        key: HexBytes(vec![0x01]),
                        code: ConflictCode::BoundsExceeded,
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

    #[test]
    fn terminal_attempt_is_evictable_only_after_outcome_is_recorded() {
        let attempt = Attempt::new();
        attempt.terminal.store(true, Ordering::Release);

        assert!(!attempt.is_evictable());

        attempt.record_result(Err(RpcServerError::TxRejected("failed".into())));
        assert!(attempt.is_evictable());
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
            NetworkInfo::new(
                "localnet",
                Hex0x(groundhog_xdr::types::SHA256Hash::from([0x42; 32])),
            )
            .expect("bug: localnet network ID should validate"),
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
}
