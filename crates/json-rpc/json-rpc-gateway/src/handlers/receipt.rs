//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.

use crate::metrics::{LookupAdmission, LookupWaitOutcome, LookupWaitStage};

use groundhog_indexer::indexer::TxInfoResult;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::{
    hex::HexBytes,
    receipt::{DETERMINISTIC_ERROR_KIND_FAULT, ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_xdr::exec_result::ExecResult;
use groundhog_xdr::transaction::{InvocationFrameOutput, TransactionHash};
use std::time::Instant;
use tokio::task::JoinHandle;
use xdr_common::VecM;

use crate::config::HealthBackend;
use crate::context::{GatewayContext, clear_scheduler_if_current};
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, REASON_PARSE_FAILED};

use super::backend_deadline;
use super::parse_event::parse_emitted_event;

struct SchedulerLookupGuard<T: Send + 'static> {
    worker: Option<JoinHandle<T>>,
    armed: bool,
}

impl<T: Send + 'static> SchedulerLookupGuard<T> {
    fn new(worker: JoinHandle<T>) -> Self {
        Self {
            worker: Some(worker),
            armed: true,
        }
    }

    fn worker_mut(&mut self) -> &mut JoinHandle<T> {
        self.worker
            .as_mut()
            .expect("bug: scheduler worker should exist")
    }

    async fn abort(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        self.armed = false;
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.worker.take();
    }
}

impl<T: Send + 'static> Drop for SchedulerLookupGuard<T> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

/// Map hogd's `ExecResult` (XDR int enum) to the gateway's `ExecStatus`.
fn map_exec_result(r: ExecResult) -> ExecStatus {
    if r == ExecResult::ExecResultOK {
        ExecStatus::Ok
    } else if r == ExecResult::ExecResultTxRootFrameReverted {
        ExecStatus::Reverted { message: None }
    } else if r == ExecResult::ExecResultTxDeterministicError {
        // ExecResult's int-enum doesn't carry a sub-type; report the shared
        // fault kind until the indexer exposes finer-grained error codes
        // (e.g. "out_of_gas", "stack_overflow").
        ExecStatus::DeterministicError {
            kind: DETERMINISTIC_ERROR_KIND_FAULT.into(),
        }
    } else if r == ExecResult::ExecResultTxNonDeterministicError {
        ExecStatus::NonDeterministicError
    } else if r == ExecResult::ExecResultFailedGasSponsorship {
        ExecStatus::FailedGasSponsorship
    } else {
        ExecStatus::Other
    }
}

/// Full receipt for a committed transaction.
///
/// Flow:
/// 1. Query hogd indexer `get_transaction_info(tx_hash)`
/// 2. If not found, return `None`
/// 3. Parse each `TxEmittedEvent.serialized_event` into topics + data
/// 4. Build `TransactionReceipt`
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_receipt(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    tx_hash: TxHash,
) -> Result<Option<TransactionReceipt>, GatewayError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("indexer not connected".into()))?;

    let hash = TransactionHash::from(tx_hash.0.0.into_inner());
    let started_at = Instant::now();
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let rpc_result =
        tokio::time::timeout_at(deadline.into(), client.get_transaction_info(&hash)).await;
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "get_transaction_info",
        match &rpc_result {
            Ok(Ok(_)) => BackendRequestOutcome::Ok,
            Ok(Err(_)) => BackendRequestOutcome::Error,
            Err(_) => BackendRequestOutcome::Timeout,
        },
        started_at.elapsed(),
    );
    let result = rpc_result
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;

    match result {
        TxInfoResult::TxInfoFound { info } => {
            let mut events = Vec::with_capacity(info.events.events.0.len());
            for raw in info.events.events.0 {
                if let Some(event) = parse_emitted_event(raw) {
                    events.push(event);
                } else {
                    ctx.metrics
                        .observe_malformed_event_dropped(REASON_PARSE_FAILED);
                }
            }
            let (return_data, abort_data) = match info.events.root_output {
                InvocationFrameOutput::Return { return_buffer } => {
                    (Some(HexBytes(return_buffer.0)), None)
                }
                InvocationFrameOutput::Abort { abort_buffer } => {
                    (None, Some(HexBytes(abort_buffer.0)))
                }
                _ => (None, None),
            };

            Ok(Some(TransactionReceipt {
                tx_hash,
                height: info.height,
                status: map_exec_result(info.events.exec_result),
                events,
                return_data,
                abort_data,
            }))
        }
        // A recent nondeterministic failure is not a committed record;
        // receipts exist only for committed transactions. The diagnostic
        // surfaces through `getTransactionStatus`.
        _ => Ok(None),
    }
}

async fn acquire_lookup_permits(
    ctx: &GatewayContext,
    deadline: Instant,
) -> Result<
    (
        tokio::sync::OwnedSemaphorePermit,
        tokio::sync::OwnedSemaphorePermit,
    ),
    GatewayError,
> {
    let admission = ctx
        .scheduler_lookup_admission
        .clone()
        .try_acquire_owned()
        .map_err(|error| {
            ctx.metrics.scheduler_lookup().admit(match error {
                tokio::sync::TryAcquireError::NoPermits => LookupAdmission::Full,
                tokio::sync::TryAcquireError::Closed => LookupAdmission::Closed,
            });
            ctx.metrics.observe_backend_request(
                HealthBackend::Scheduler,
                "schedgateway_lookup_tx",
                BackendRequestOutcome::Rejected,
                std::time::Duration::ZERO,
            );
            GatewayError::Overloaded(
                "scheduler transaction-status lookup capacity exhausted".into(),
            )
        })?;
    ctx.metrics
        .scheduler_lookup()
        .admit(LookupAdmission::Accepted);
    let wait = ctx
        .metrics
        .scheduler_lookup()
        .wait(LookupWaitStage::ExecutionPermit);
    let permit = tokio::time::timeout_at(
        deadline.into(),
        ctx.scheduler_lookup_execution.clone().acquire_owned(),
    )
    .await;
    wait.finish(match &permit {
        Ok(Ok(_)) => LookupWaitOutcome::Ready,
        Ok(Err(_)) => LookupWaitOutcome::Closed,
        Err(_) => LookupWaitOutcome::Timeout,
    });
    let permit = permit
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|_| {
            GatewayError::BackendUnavailable("scheduler lookup execution closed".into())
        })?;
    Ok((admission, permit))
}

async fn transaction_is_pending(
    ctx: &GatewayContext,
    deadline: Instant,
    tx_hash: TxHash,
) -> Result<bool, GatewayError> {
    let (admission, permit) = acquire_lookup_permits(ctx, deadline).await?;
    let execution = ctx.metrics.scheduler_lookup().executing();
    // Queued requests must use the current generation, not a pre-wait snapshot.
    let wait = ctx
        .metrics
        .scheduler_lookup()
        .wait(LookupWaitStage::SchedulerClient);
    let scheduler = tokio::time::timeout_at(deadline.into(), ctx.scheduler.read()).await;
    wait.finish(if scheduler.is_ok() {
        LookupWaitOutcome::Ready
    } else {
        LookupWaitOutcome::Timeout
    });
    let scheduler = scheduler.map_err(|_| GatewayError::BackendTimeout)?;
    if Instant::now() >= deadline {
        return Err(GatewayError::BackendTimeout);
    }
    let client = scheduler
        .as_ref()
        .ok_or_else(|| GatewayError::BackendUnavailable("scheduler not connected".into()))?;
    let generation = client.generation();
    let lookup_client = client.lookup_client().clone();
    let hash = TransactionHash::from(tx_hash.0.0.into_inner());
    let batch: VecM<TransactionHash> = VecM(vec![hash]);
    let started_at = Instant::now();
    let worker = tokio::spawn(async move {
        let _admission = admission;
        let _permit = permit;
        let _execution = execution;
        if Instant::now() >= deadline {
            return None;
        }
        Some(lookup_client.schedgateway_lookup_tx(&batch).await)
    });
    drop(scheduler);
    let mut guard = SchedulerLookupGuard::new(worker);
    let rpc_result =
        if let Ok(result) = tokio::time::timeout_at(deadline.into(), guard.worker_mut()).await {
            result
                .unwrap_or(Some(Err(rpc_driver::RpcError::SystemError)))
                .ok_or(())
        } else {
            // ReplaceConnection isolates cancellation to this lookup connection;
            // a spent request budget is not evidence that the scheduler is unhealthy.
            guard.abort().await;
            Err(())
        };
    if matches!(rpc_result, Ok(Err(_))) {
        clear_scheduler_if_current(&ctx.scheduler, generation).await;
    }
    guard.disarm();
    ctx.metrics.observe_backend_request(
        HealthBackend::Scheduler,
        "schedgateway_lookup_tx",
        match &rpc_result {
            Ok(Ok(_)) => BackendRequestOutcome::Ok,
            Ok(Err(_)) => BackendRequestOutcome::Error,
            Err(()) => BackendRequestOutcome::Timeout,
        },
        started_at.elapsed(),
    );
    match rpc_result {
        Ok(Ok(results)) if results.0.len() == 1 => match results.iter().next() {
            Some(Some(_)) => Ok(true),
            Some(None) => Ok(false),
            None => unreachable!("exactly one scheduler result was checked above"),
        },
        Ok(Ok(results)) => Err(GatewayError::BackendUnavailable(format!(
            "scheduler returned {} lookup results for one transaction",
            results.0.len()
        ))),
        Ok(Err(error)) => Err(GatewayError::BackendUnavailable(format!(
            "scheduler lookup failed: {error}"
        ))),
        Err(()) => Err(GatewayError::BackendTimeout),
    }
}

async fn indexed_transaction_status(
    ctx: &GatewayContext,
    deadline: Instant,
    tx_hash: TxHash,
) -> Result<Option<TransactionStatus>, GatewayError> {
    let maybe_indexer = tokio::time::timeout_at(deadline.into(), ctx.indexer.read())
        .await
        .map_err(|_| GatewayError::BackendTimeout)?
        .clone();
    // timeout polls its future even when already expired; do not dispatch a late fallback.
    if Instant::now() >= deadline {
        return Err(GatewayError::BackendTimeout);
    }
    if let Some(client) = maybe_indexer.as_ref() {
        let hash = TransactionHash::from(tx_hash.0.0.into_inner());
        let started_at = Instant::now();
        let rpc_result =
            tokio::time::timeout_at(deadline.into(), client.get_transaction_info(&hash)).await;
        ctx.metrics.observe_backend_request(
            HealthBackend::Indexer,
            "get_transaction_info",
            match &rpc_result {
                Ok(Ok(_)) => BackendRequestOutcome::Ok,
                Ok(Err(_)) => BackendRequestOutcome::Error,
                Err(_) => BackendRequestOutcome::Timeout,
            },
            started_at.elapsed(),
        );
        match rpc_result {
            Ok(Ok(TxInfoResult::TxInfoFound { info })) => {
                return Ok(Some(TransactionStatus::Committed {
                    height: info.height,
                }));
            }
            Ok(Ok(TxInfoResult::TxInfoRecentFailure { recent_failure })) => {
                return Ok(Some(TransactionStatus::from(&recent_failure)));
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(GatewayError::BackendUnavailable(format!("indexer: {e}")));
            }
            Err(_) => return Err(GatewayError::BackendTimeout),
        }
    } else {
        return Err(GatewayError::BackendUnavailable(
            "indexer not connected".into(),
        ));
    }

    Ok(None)
}

/// Check the indexer first so committed reads do not occupy the scheduler lane.
/// Pending takes precedence over advisory failure diagnostics. After an indexer
/// miss and scheduler miss, recheck the indexer to cover a commit between reads.
/// All stages share one deadline; Unknown requires successful misses from both
/// backends. A pending answer remains available if the indexer is disconnected.
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_status(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    tx_hash: TxHash,
) -> Result<TransactionStatus, GatewayError> {
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let indexed = indexed_transaction_status(ctx, deadline, tx_hash).await;
    if let Ok(Some(TransactionStatus::Committed { height })) = indexed {
        return Ok(TransactionStatus::Committed { height });
    }
    let scheduler_failure = match transaction_is_pending(ctx, deadline, tx_hash).await {
        Ok(true) => return Ok(TransactionStatus::Pending),
        Ok(false) => None,
        Err(error) => {
            tracing::warn!(%error, "scheduler lookup did not complete; checking indexer");
            Some(error)
        }
    };
    // Retain the existing pending-over-diagnostic precedence. Indexer failures
    // cannot yield Unknown, even if the scheduler reports a miss.
    if let Some(status) = indexed? {
        return Ok(status);
    }
    if let Some(status) = indexed_transaction_status(ctx, deadline, tx_hash).await? {
        return Ok(status);
    }
    match scheduler_failure {
        Some(error) => Err(error),
        None => Ok(TransactionStatus::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use groundhog_xdr::PROTOCOL_VERSION;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use groundhog_indexer::{
        AddressTransactionQuery, BlockLookupResult, BlockTransactionQuery, EventPageResult,
        EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
        TransactionPageResult, TransactionQuery, TransactionRefPageResult, TxInfoOutcome,
    };
    use groundhog_scheduler::{
        ExpectedDBTip, ParallelSignedTransactionBatch, PendingTxCount, SchedGatewayIntroOutcome,
        SchedGatewaySubmitOutcomeBatch,
        sched_gateway::{
            SchedGateway_SchedGateway1, SchedGateway_SchedGateway1_Client,
            SchedGateway_SchedGateway1_Dispatcher,
        },
    };
    use groundhog_scp::network::ProtocolCommit as Protocol;
    use groundhog_x23_transport::try_connect;
    use groundhog_xdr::{
        transaction::{OptSignedTransactionBatch, TransactionHashBatch},
        types::SHA256Hash,
    };
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;
    use tokio::sync::{Notify, RwLock};

    use super::*;

    #[test]
    fn deterministic_exec_result_maps_to_fault_kind() {
        let status = map_exec_result(ExecResult::ExecResultTxDeterministicError);
        match status {
            ExecStatus::DeterministicError { kind } => {
                assert_eq!(
                    kind,
                    groundhog_json_rpc_types::receipt::DETERMINISTIC_ERROR_KIND_FAULT
                );
            }
            other => panic!("expected DeterministicError, got {other:?}"),
        }
    }
    use crate::config::GatewayConfig;
    use crate::context::{
        BackendClient, SCHEDULER_LOOKUP_CONCURRENCY, SCHEDULER_LOOKUP_MAX_OUTSTANDING,
        SchedulerClient, connect_optional_scheduler,
    };
    use crate::metrics::GatewayMetrics;

    struct FakeIndexer {
        results: std::sync::Mutex<std::collections::VecDeque<Option<TxInfoResult>>>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for FakeIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _arg0: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            _arg0: TransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
            let result = if results.len() > 1 {
                results.pop_front().unwrap()
            } else {
                results.front().unwrap().clone()
            };
            result.ok_or_else(|| ProgramError::msg("fake indexer failure"))
        }

        async fn get_block_by_height(
            &self,
            _arg0: groundhog_xdr::types::BlockHeight,
        ) -> Result<BlockLookupResult, ProgramError> {
            panic!("bug: status tests should not query blocks")
        }

        async fn get_events(&self, _arg0: EventQuery) -> Result<EventPageResult, ProgramError> {
            panic!("bug: status tests should not query events")
        }

        async fn get_transactions(
            &self,
            _arg0: TransactionQuery,
        ) -> Result<TransactionPageResult, ProgramError> {
            panic!("bug: status tests should not query transactions")
        }

        async fn get_transactions_by_address(
            &self,
            _arg0: AddressTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("bug: status tests should not query transactions by address")
        }

        async fn get_transactions_by_block(
            &self,
            _arg0: BlockTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("bug: status tests should not query transactions by block")
        }
    }

    #[derive(Clone)]
    enum FakeSchedulerMode {
        GatedMiss(Arc<LookupGate>),
        Pending,
        /// Mempool lookup succeeds and reports the transaction absent.
        MempoolMiss,
        /// Mempool lookup succeeds but returns no slot for the queried hash.
        EmptyBatch,
        /// Mempool lookup violates the exact-cardinality response contract.
        ExtraSlot,
        /// Mempool lookup fails at the RPC layer.
        RpcFail,
        /// Mempool lookup waits until the test replaces the active generation.
        GatedRpcFail {
            started: Arc<Notify>,
            release: Arc<Notify>,
        },
        /// The first calls wait for cancellation; subsequent lookups miss.
        GatedThenMempoolMiss {
            gated_calls: usize,
            calls: Arc<AtomicUsize>,
            started: Arc<Notify>,
            release: Arc<Notify>,
        },
    }

    struct FakeScheduler {
        mode: FakeSchedulerMode,
    }

    struct LookupGate {
        started: tokio::sync::Semaphore,
        release: tokio::sync::Semaphore,
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl LookupGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                started: tokio::sync::Semaphore::new(0),
                release: tokio::sync::Semaphore::new(0),
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
            })
        }

        async fn wait_started(&self, count: u32) {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.started.acquire_many(count),
            )
            .await
            .unwrap()
            .unwrap()
            .forget();
        }
    }

    fn poll_pending<F: Future>(future: std::pin::Pin<&mut F>) {
        assert!(
            future
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
    }

    async fn watchdog<F: Future>(future: F) -> F::Output {
        tokio::time::timeout(std::time::Duration::from_secs(5), future)
            .await
            .expect("test operation exceeded independent watchdog")
    }

    #[async_trait]
    impl SchedGateway_SchedGateway1 for FakeScheduler {
        async fn schedgateway_intro(
            &self,
            _arg0: Protocol,
        ) -> Result<SchedGatewayIntroOutcome, ProgramError> {
            Ok(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess)
        }

        async fn schedgateway_submit_tx_batch(
            &self,
            _arg0: ParallelSignedTransactionBatch,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, ProgramError> {
            panic!("bug: status tests should not submit")
        }

        async fn schedgateway_get_tip(&self) -> Result<ExpectedDBTip, ProgramError> {
            Ok(ExpectedDBTip::Default {
                error: groundhog_scheduler::RPCError::kRPCNotFound.0,
            })
        }

        async fn schedgateway_lookup_tx(
            &self,
            _arg0: TransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            match &self.mode {
                FakeSchedulerMode::GatedMiss(gate) => {
                    gate.calls.fetch_add(1, Ordering::SeqCst);
                    let active = gate.active.fetch_add(1, Ordering::SeqCst) + 1;
                    gate.max_active.fetch_max(active, Ordering::SeqCst);
                    gate.started.add_permits(1);
                    gate.release.acquire().await.unwrap().forget();
                    gate.active.fetch_sub(1, Ordering::SeqCst);
                    Ok(VecM(vec![None]))
                }
                FakeSchedulerMode::Pending => {
                    let TxInfoResult::TxInfoFound { info } = committed_result() else {
                        unreachable!()
                    };
                    Ok(VecM(vec![Some(info.tx.signed_tx)]))
                }
                FakeSchedulerMode::MempoolMiss => Ok(VecM(vec![None])),
                FakeSchedulerMode::EmptyBatch => Ok(VecM(vec![])),
                FakeSchedulerMode::ExtraSlot => Ok(VecM(vec![None, None])),
                FakeSchedulerMode::RpcFail => {
                    Err(ProgramError::msg("fake scheduler lookup failure"))
                }
                FakeSchedulerMode::GatedRpcFail { started, release } => {
                    started.notify_one();
                    release.notified().await;
                    Err(ProgramError::msg("delayed scheduler lookup failure"))
                }
                FakeSchedulerMode::GatedThenMempoolMiss {
                    gated_calls,
                    calls,
                    started,
                    release,
                } => {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    if call < *gated_calls {
                        started.notify_one();
                        release.notified().await;
                    }
                    Ok(VecM(vec![None]))
                }
            }
        }

        async fn schedgateway_pending_tx_count(&self) -> Result<PendingTxCount, ProgramError> {
            panic!("bug: status tests should not read the pending count")
        }
    }

    async fn start_not_found_indexer() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        start_fake_indexer(
            TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            },
            Arc::new(AtomicUsize::new(0)),
        )
        .await
    }

    async fn start_fake_indexer(
        result: TxInfoResult,
        calls: Arc<AtomicUsize>,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        start_scripted_indexer(vec![Some(result)], calls).await
    }

    async fn start_scripted_indexer(
        results: Vec<Option<TxInfoResult>>,
        calls: Arc<AtomicUsize>,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test indexer should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test indexer should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(FakeIndexer {
                results: std::sync::Mutex::new(results.into()),
                calls,
            })),
        ));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn start_fake_scheduler(
        mode: FakeSchedulerMode,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test scheduler should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedGateway_SchedGateway1_Dispatcher::new(
            Box::new(FakeScheduler { mode }),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn test_context(
        scheduler_addr: Option<std::net::SocketAddr>,
        indexer_addr: std::net::SocketAddr,
    ) -> GatewayContext {
        let scheduler = match scheduler_addr {
            Some(addr) => {
                connect_optional_scheduler(&addr.to_string(), 1, 4, PROTOCOL_VERSION, None)
                    .await
                    .expect("bug: test scheduler should connect")
            }
            None => None,
        };
        let indexer = try_connect(
            "indexer",
            &indexer_addr.to_string(),
            1,
            IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
        )
        .await
        .expect("bug: test indexer should connect");

        GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(scheduler)),
            Arc::new(RwLock::new(
                indexer.map(|client| BackendClient::new(client.clone(), client)),
            )),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(GatewayConfig::default()),
            GatewayMetrics::for_tests(),
        )
    }

    async fn gathered_metrics(ctx: &GatewayContext) -> String {
        let health = crate::health::GatewayHealth::new(
            &GatewayConfig::default(),
            groundhog_build_info::BuildVersion::new("test", None),
            None,
        );
        ctx.metrics.gather(&health.snapshot().await).unwrap()
    }

    fn assert_lookup_admission(metrics: &str, outcome: &str, count: usize) {
        assert!(metrics.contains(&format!(
            "groundhog_gateway_scheduler_lookup_admission_total{{outcome=\"{outcome}\"}} {count}\n"
        )));
    }

    fn assert_lookup_gauges(metrics: &str, queued: usize, executing: usize) {
        for (stage, value) in [("queued", queued), ("executing", executing)] {
            assert!(
                metrics.contains(&format!(
                    "groundhog_gateway_scheduler_lookup_in_flight{{stage=\"{stage}\"}} {value}\n"
                )),
                "unexpected {stage} lookup gauge: {metrics}"
            );
        }
    }

    fn test_hash() -> TxHash {
        TxHash(groundhog_json_rpc_types::hex::Hex0x(SHA256Hash::from(
            [7u8; 32],
        )))
    }

    async fn start_status_http_server(
        ctx: Arc<GatewayContext>,
    ) -> (std::net::SocketAddr, jsonrpsee::server::ServerHandle) {
        use groundhog_json_rpc_api::GroundhogApiServer;
        use groundhog_json_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};
        use groundhog_json_rpc_types::{NetworkHash, network_info::NetworkInfo};

        let config = ctx.config.clone();
        let rpc_metrics = ctx.metrics.clone();
        let batch_config = config.clone();
        let max_response_body_size = jsonrpsee::core::TEN_MB_SIZE_BYTES;
        let server = jsonrpsee::server::Server::builder()
            .set_config(
                jsonrpsee::server::ServerConfig::builder()
                    .max_connections(config.max_connections)
                    .max_response_body_size(max_response_body_size)
                    .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(1_000))
                    .build(),
            )
            .set_http_middleware(crate::middleware::http_middleware(
                &config,
                ctx.metrics.clone(),
            ))
            .set_rpc_middleware(
                jsonrpsee::core::middleware::RpcServiceBuilder::new()
                    .layer_fn(move |inner| {
                        crate::rpc_middleware::RpcMetricsService::new(inner, rpc_metrics.clone())
                    })
                    .layer_fn(move |inner| {
                        batch_config.batch_service(inner, max_response_body_size)
                    }),
            )
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();
        let handle = server.start(
            GroundhogRpcServer::unified(
                crate::backend::GatewayBackend::new(ctx.clone()),
                NetworkInfo::new(NetworkHash::from(SHA256Hash::from([0; 32]))),
                RpcServerConfig::default()
                    .with_request_timeout_ms(config.request_timeout_ms.max(3))
                    .with_max_events_per_page(config.max_events_per_page)
                    .with_max_transactions_per_page(config.max_transactions_per_page)
                    .with_max_cursor_bytes(config.max_cursor_bytes)
                    .with_max_view_calldata_bytes(config.max_view_calldata_bytes)
                    .with_view_gas_limit(config.view_gas_limit),
                BuildInfo::new("test", Some("test")),
            )
            .into_rpc(),
        );
        (addr, handle)
    }

    #[tokio::test]
    async fn http_status_array_returns_twenty_unknown_at_serial_and_default_concurrency() {
        for concurrency in [1, 128] {
            let gate = LookupGate::new();
            let (scheduler_addr, scheduler) =
                start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
            let indexer_calls = Arc::new(AtomicUsize::new(0));
            let (indexer_addr, indexer) = start_fake_indexer(
                TxInfoResult::Default {
                    outcome: TxInfoOutcome::TxInfoNotFound.0,
                },
                indexer_calls.clone(),
            )
            .await;
            let mut ctx = test_context(Some(scheduler_addr), indexer_addr).await;
            ctx.config = Arc::new(GatewayConfig {
                rpc_batch_concurrency: concurrency.try_into().unwrap(),
                ..GatewayConfig::default()
            });
            let ctx = Arc::new(ctx);
            let (addr, handle) = watchdog(start_status_http_server(ctx.clone())).await;
            let request: Vec<_> = (0..20)
                .map(|id| {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "method": "groundhog_getTransactionStatus", "params": [test_hash()],
                    })
                })
                .collect();
            let response = tokio::spawn(async move {
                reqwest::Client::new()
                    .post(format!("http://{addr}"))
                    .json(&request)
                    .send()
                    .await
                    .unwrap()
                    .json::<Vec<serde_json::Value>>()
                    .await
                    .unwrap()
            });
            let width = concurrency.min(SCHEDULER_LOOKUP_CONCURRENCY);
            gate.wait_started(width.try_into().unwrap()).await;
            // Hold the first wave until every dispatched entry is admitted.
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while ctx.scheduler_lookup_admission.available_permits()
                    != SCHEDULER_LOOKUP_MAX_OUTSTANDING - concurrency.min(20)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(gate.calls.load(Ordering::SeqCst), width);
            assert_eq!(indexer_calls.load(Ordering::SeqCst), concurrency.min(20));
            gate.release.add_permits(20);
            let responses = tokio::time::timeout(std::time::Duration::from_secs(5), response)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(responses.len(), 20);
            for (id, response) in responses.iter().enumerate() {
                assert_eq!(
                    response,
                    &serde_json::json!({
                        "jsonrpc": "2.0", "id": id, "result": {"status": "unknown"},
                    })
                );
            }
            assert_eq!(gate.calls.load(Ordering::SeqCst), 20);
            assert_eq!(indexer_calls.load(Ordering::SeqCst), 40);
            assert_eq!(gate.max_active.load(Ordering::SeqCst), width);
            let snapshot = crate::health::GatewayHealthSnapshot::for_test(
                crate::health::HealthStatus::Starting,
                None,
                &[],
            );
            let metrics = ctx.metrics.gather(&snapshot).unwrap();
            assert!(metrics.contains("groundhog_gateway_rpc_requests_total{method=\"groundhog_getTransactionStatus\",outcome=\"ok\"} 20\n"));
            assert!(metrics.contains("groundhog_gateway_http_requests_total{endpoint=\"jsonrpc\",status_class=\"2xx\"} 1\n"));
            assert!(metrics.contains("groundhog_gateway_rpc_batch_size_count 1\n"));
            handle.stop().unwrap();
            watchdog(handle.stopped()).await;
            ctx.abort_request_workers();
            scheduler.abort();
            indexer.abort();
        }
    }

    #[tokio::test]
    async fn lookup_queue_is_bounded_across_callers_and_cancellation_recovers_permits() {
        let gate = LookupGate::new();
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let mut callers = Vec::new();
        for _ in 0..SCHEDULER_LOOKUP_MAX_OUTSTANDING {
            let mut caller = Box::pin(transaction_is_pending(
                &ctx,
                Instant::now() + std::time::Duration::from_secs(4),
                test_hash(),
            ));
            poll_pending(caller.as_mut());
            callers.push(caller);
        }
        gate.wait_started(4).await;
        assert_lookup_gauges(&gathered_metrics(&ctx).await, 64, 4);
        assert_eq!(ctx.scheduler_lookup_admission.available_permits(), 0);
        assert!(matches!(
            watchdog(transaction_is_pending(
                &ctx,
                Instant::now() + std::time::Duration::from_secs(4),
                test_hash()
            ))
            .await,
            Err(GatewayError::Overloaded(_))
        ));
        callers.truncate(4);
        let metrics = gathered_metrics(&ctx).await;
        assert_lookup_gauges(&metrics, 0, 4);
        assert!(metrics.contains(
            "groundhog_gateway_scheduler_lookup_admission_total{outcome=\"accepted\"} 68\n"
        ));
        assert!(
            metrics.contains(
                "groundhog_gateway_scheduler_lookup_admission_total{outcome=\"full\"} 1\n"
            )
        );
        assert!(metrics.contains(concat!(
            "groundhog_gateway_scheduler_lookup_wait_duration_seconds_count",
            "{outcome=\"cancelled\",stage=\"execution_permit\"} 64\n",
        )));
        assert_eq!(ctx.scheduler_lookup_admission.available_permits(), 64);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 4);
        gate.release.add_permits(4);
        for caller in callers {
            assert!(!watchdog(caller).await.unwrap());
        }
        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING
        );
        assert_eq!(ctx.scheduler_lookup_execution.available_permits(), 4);
        assert!(ctx.scheduler.read().await.is_some());
        assert_lookup_gauges(&gathered_metrics(&ctx).await, 0, 0);
    }

    #[tokio::test]
    async fn queued_timeout_does_not_launch_work_or_invalidate_scheduler() {
        let gate = LookupGate::new();
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
        let indexer_calls = Arc::new(AtomicUsize::new(0));
        let (indexer_addr, _indexer) = start_fake_indexer(
            TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            },
            indexer_calls.clone(),
        )
        .await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let slots = ctx
            .scheduler_lookup_execution
            .clone()
            .try_acquire_many_owned(4)
            .unwrap();
        let context = RpcRequestContext::new(Some(std::time::Duration::from_millis(20)));
        let deadline = context.deadline().unwrap();
        let mut lookup = Box::pin(transaction_is_pending(&ctx, deadline, test_hash()));
        poll_pending(lookup.as_mut());
        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING - 1
        );
        assert!(matches!(
            watchdog(lookup).await,
            Err(GatewayError::BackendTimeout)
        ));
        drop(slots);
        let metrics = gathered_metrics(&ctx).await;
        assert_lookup_gauges(&metrics, 0, 0);
        assert!(metrics.contains("groundhog_gateway_scheduler_lookup_wait_duration_seconds_count{outcome=\"timeout\",stage=\"execution_permit\"} 1\n"));
        assert!(metrics.contains("groundhog_gateway_scheduler_lookup_wait_duration_seconds_count{outcome=\"ready\",stage=\"scheduler_client\"} 0\n"));
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
        assert_eq!(indexer_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING
        );
        assert_eq!(ctx.scheduler_lookup_execution.available_permits(), 4);
        assert!(ctx.scheduler.read().await.is_some());
        // An expired request must not dispatch even when execution is immediately available.
        assert!(matches!(
            watchdog(transaction_is_pending(&ctx, deadline, test_hash())).await,
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn queued_lookup_execution_timeout_preserves_normal_scheduler_lane() {
        use std::time::Duration;

        let gate = LookupGate::new();
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let generation = watchdog(ctx.scheduler.read())
            .await
            .as_ref()
            .unwrap()
            .generation();
        let slots = ctx
            .scheduler_lookup_execution
            .clone()
            .try_acquire_many_owned(4)
            .unwrap();

        tokio::time::pause();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut lookup = Box::pin(transaction_is_pending(&ctx, deadline, test_hash()));
        poll_pending(lookup.as_mut());
        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING - 1
        );
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_millis(9_900)).await;
        drop(slots);
        poll_pending(lookup.as_mut());

        // Keep the paused clock from auto-advancing while x23 reaches the gate.
        // This watchdog uses wall time, independent of the simulated RPC deadline.
        let started = Instant::now();
        while gate.started.available_permits() == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "lookup never reached x23"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(ctx.scheduler_lookup_execution.available_permits(), 3);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::resume();
        assert!(matches!(
            watchdog(lookup).await,
            Err(GatewayError::BackendTimeout)
        ));

        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING
        );
        assert_eq!(ctx.scheduler_lookup_execution.available_permits(), 4);
        let scheduler = watchdog(ctx.scheduler.read())
            .await
            .as_ref()
            .expect("a lookup deadline must not evict the scheduler")
            .clone();
        assert_eq!(scheduler.generation(), generation);
        assert!(matches!(
            watchdog(scheduler.schedgateway_get_tip()).await.unwrap(),
            ExpectedDBTip::Default { error }
                if error == groundhog_scheduler::RPCError::kRPCNotFound.0
        ));

        // The backend may finish the canceled call later; the replacement lane remains usable.
        gate.release.add_permits(2);
        assert!(
            !watchdog(transaction_is_pending(
                &ctx,
                (tokio::time::Instant::now() + Duration::from_secs(4)).into_std(),
                test_hash()
            ))
            .await
            .unwrap()
        );
        assert_eq!(gate.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING
        );
        assert_eq!(ctx.scheduler_lookup_execution.available_permits(), 4);
    }

    #[tokio::test]
    async fn queued_lookup_uses_replacement_scheduler_generation() {
        let (old_addr, _old) = start_fake_scheduler(FakeSchedulerMode::RpcFail).await;
        let (new_addr, _new) = start_fake_scheduler(FakeSchedulerMode::MempoolMiss).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(old_addr), indexer_addr).await;
        let slots = ctx
            .scheduler_lookup_execution
            .clone()
            .try_acquire_many_owned(4)
            .unwrap();
        let mut lookup = Box::pin(transaction_is_pending(
            &ctx,
            Instant::now() + std::time::Duration::from_secs(4),
            test_hash(),
        ));
        poll_pending(lookup.as_mut());
        let replacement = watchdog(connect_optional_scheduler(
            &new_addr.to_string(),
            1,
            4,
            PROTOCOL_VERSION,
            None,
        ))
        .await
        .unwrap()
        .unwrap();
        let generation = replacement.generation();
        *ctx.scheduler.write().await = Some(replacement);
        drop(slots);
        assert!(!watchdog(lookup).await.unwrap());
        assert_eq!(
            ctx.scheduler.read().await.as_ref().unwrap().generation(),
            generation
        );
    }

    fn committed_result() -> TxInfoResult {
        use groundhog_xdr::{
            auth_claim::{AuthClaim, AuthClaim_source_address_claim},
            event::TxEventLog,
            transaction::*,
        };
        use xdr_common::BytesM;

        TxInfoResult::TxInfoFound {
            info: groundhog_indexer::TxInfo {
                height: 42.into(),
                tx: ProposedTransaction {
                    signed_tx: SignedTransaction {
                        tx: Transaction {
                            invocation: Invocation {
                                invoked_address: [0; 32].into(),
                                entrypoint: [0; 8].into(),
                                gas_limit: 0,
                                calldata: BytesM(vec![]),
                            },
                            uniquifier: Uniquifier {
                                block_expiry_height: 0.into(),
                                nonce: 0,
                            },
                            contracts_to_deploy: VecM(vec![]),
                            auth_claim: AuthClaim::kSourceAddressAuthData {
                                source_address_claim: AuthClaim_source_address_claim {
                                    allowed_keys: 0,
                                    required_weight: 0,
                                },
                            },
                        },
                        signatures: VecM(vec![]),
                    },
                    nondeterministic_proposal_data: NondeterministicProposalData {
                        nondeterministic_bits: BytesM(vec![]),
                        nondeterministic_bit_count: 0,
                        observed_queue_entry_sequence: VecM(vec![]),
                    },
                },
                events: TxEventLog {
                    tx_hash: [7; 32].into(),
                    exec_result: ExecResult::ExecResultOK,
                    root_output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(vec![]),
                    },
                    events: VecM(vec![]),
                },
            },
        }
    }

    fn recent_diagnostic() -> groundhog_trace::TxFailureDiagnostic {
        use groundhog_trace::{DbConflictCode, DbConflictDiagnostic, TxFailureDiagnostic};
        TxFailureDiagnostic {
            attempt_height: 43.into(),
            conflict: DbConflictDiagnostic {
                addrkey: groundhog_xdr::types::AddressAndKey {
                    address: [0; 32].into(),
                    key: groundhog_xdr::types::StorageKey::FreeNativeTokenBalance,
                },
                code: DbConflictCode::kDbConflictExclusiveLock,
            },
        }
    }

    fn indexer_miss() -> TxInfoResult {
        TxInfoResult::Default {
            outcome: TxInfoOutcome::TxInfoNotFound.0,
        }
    }

    #[tokio::test]
    async fn committed_status_skips_scheduler_lookup() {
        let gate = LookupGate::new();
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let (indexer_addr, _indexer) = start_fake_indexer(committed_result(), calls.clone()).await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let status = watchdog(get_transaction_status(
            &ctx,
            RpcRequestContext::unbounded(),
            test_hash(),
        ))
        .await
        .unwrap();
        assert!(matches!(status, TransactionStatus::Committed { height } if height == 42.into()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pending_wins_over_miss_recent_failure_and_indexer_failure() {
        for result in [
            Some(indexer_miss()),
            Some(TxInfoResult::TxInfoRecentFailure {
                recent_failure: recent_diagnostic(),
            }),
            None,
        ] {
            let (scheduler_addr, _scheduler) =
                start_fake_scheduler(FakeSchedulerMode::Pending).await;
            let calls = Arc::new(AtomicUsize::new(0));
            let (indexer_addr, _indexer) =
                start_scripted_indexer(vec![result], calls.clone()).await;
            let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
            assert!(matches!(
                watchdog(get_transaction_status(
                    &ctx,
                    RpcRequestContext::unbounded(),
                    test_hash()
                ))
                .await
                .unwrap(),
                TransactionStatus::Pending
            ));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            *ctx.indexer.write().await = None;
            assert!(matches!(
                watchdog(get_transaction_status(
                    &ctx,
                    RpcRequestContext::unbounded(),
                    test_hash()
                ))
                .await
                .unwrap(),
                TransactionStatus::Pending
            ));
        }
    }

    #[tokio::test]
    async fn scheduler_miss_retains_indexer_diagnostic_or_error() {
        for result in [
            Some(TxInfoResult::TxInfoRecentFailure {
                recent_failure: recent_diagnostic(),
            }),
            None,
        ] {
            let has_diagnostic = result.is_some();
            let (scheduler_addr, _scheduler) =
                start_fake_scheduler(FakeSchedulerMode::MempoolMiss).await;
            let calls = Arc::new(AtomicUsize::new(0));
            let (indexer_addr, _indexer) =
                start_scripted_indexer(vec![result], calls.clone()).await;
            let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
            let result = watchdog(get_transaction_status(
                &ctx,
                RpcRequestContext::unbounded(),
                test_hash(),
            ))
            .await;
            if has_diagnostic {
                assert!(matches!(
                    result.unwrap(),
                    TransactionStatus::RecentFailure { .. }
                ));
            } else {
                assert!(matches!(result, Err(GatewayError::BackendUnavailable(_))));
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn indexer_recheck_covers_commit_race_and_preserves_errors() {
        // Both a scheduler miss and a scheduler failure can race with a commit.
        for mode in [FakeSchedulerMode::MempoolMiss, FakeSchedulerMode::RpcFail] {
            for second in [Some(committed_result()), Some(indexer_miss()), None] {
                let expected_commit = matches!(second, Some(TxInfoResult::TxInfoFound { .. }));
                let expected_unknown = matches!(mode, FakeSchedulerMode::MempoolMiss)
                    && matches!(second, Some(TxInfoResult::Default { .. }));
                let (scheduler_addr, _scheduler) = start_fake_scheduler(mode.clone()).await;
                let calls = Arc::new(AtomicUsize::new(0));
                let (indexer_addr, _indexer) =
                    start_scripted_indexer(vec![Some(indexer_miss()), second], calls.clone()).await;
                let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
                let result = watchdog(get_transaction_status(
                    &ctx,
                    RpcRequestContext::unbounded(),
                    test_hash(),
                ))
                .await;
                if expected_commit {
                    assert!(matches!(
                        result.unwrap(),
                        TransactionStatus::Committed { .. }
                    ));
                } else if expected_unknown {
                    assert!(matches!(result.unwrap(), TransactionStatus::Unknown));
                } else {
                    assert!(matches!(result, Err(GatewayError::BackendUnavailable(_))));
                }
                assert_eq!(calls.load(Ordering::SeqCst), 2);
            }
        }
    }

    #[tokio::test]
    async fn expired_indexer_wait_does_not_dispatch_scheduler() {
        let gate = LookupGate::new();
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let (indexer_addr, _indexer) = start_fake_indexer(indexer_miss(), calls.clone()).await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let _locked = ctx.indexer.write().await;
        let context = RpcRequestContext::new(Some(std::time::Duration::from_millis(20)));
        assert!(matches!(
            watchdog(get_transaction_status(&ctx, context, test_hash())).await,
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn scheduler_timeout_does_not_dispatch_indexer_recheck() {
        let gate = LookupGate::new();
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedMiss(gate.clone())).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let (indexer_addr, _indexer) = start_fake_indexer(indexer_miss(), calls.clone()).await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let context = RpcRequestContext::new(Some(std::time::Duration::from_millis(100)));
        let result = watchdog(get_transaction_status(&ctx, context, test_hash())).await;
        assert!(matches!(result, Err(GatewayError::BackendTimeout)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
        assert!(ctx.scheduler.read().await.is_some());
    }

    #[tokio::test]
    async fn terminal_indexer_answers_survive_full_lookup_admission() {
        let committed = committed_result();
        let diagnostic = recent_diagnostic();
        let recent_status = TransactionStatus::from(&diagnostic);
        for (result, expected) in [
            (
                committed,
                TransactionStatus::Committed { height: 42.into() },
            ),
            (
                TxInfoResult::TxInfoRecentFailure {
                    recent_failure: diagnostic,
                },
                recent_status,
            ),
        ] {
            let (scheduler_addr, _scheduler) =
                start_fake_scheduler(FakeSchedulerMode::MempoolMiss).await;
            let (indexer_addr, _indexer) =
                start_fake_indexer(result, Arc::new(AtomicUsize::new(0))).await;
            let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
            let _admission = ctx
                .scheduler_lookup_admission
                .clone()
                .acquire_many_owned(SCHEDULER_LOOKUP_MAX_OUTSTANDING.try_into().unwrap())
                .await
                .unwrap();
            let is_committed = matches!(expected, TransactionStatus::Committed { .. });
            let status = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert!(ctx.scheduler.read().await.is_some());
            let metrics = gathered_metrics(&ctx).await;
            assert_lookup_gauges(&metrics, 0, 0);
            assert_lookup_admission(&metrics, "full", usize::from(!is_committed));
            assert_lookup_admission(&metrics, "accepted", 0);
            assert_eq!(
                metrics.contains(concat!(
                    "groundhog_gateway_backend_requests_total{backend=\"scheduler\",",
                    "operation=\"schedgateway_lookup_tx\",outcome=\"rejected\"} 1",
                )),
                !is_committed,
            );
        }
    }

    #[tokio::test]
    async fn status_errors_when_scheduler_not_connected_and_indexer_misses() {
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(None, indexer_addr).await;

        let err = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect_err("bug: unconsulted mempool must not produce a definite status");

        assert!(matches!(err, GatewayError::BackendUnavailable(_)));
        let metrics = gathered_metrics(&ctx).await;
        assert_lookup_gauges(&metrics, 0, 0);
        // Acquiring an empty client lock is ready, not a healthy backend call.
        assert!(metrics.contains("groundhog_gateway_scheduler_lookup_wait_duration_seconds_count{outcome=\"ready\",stage=\"scheduler_client\"} 1\n"));
        assert!(!metrics.contains("groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedgateway_lookup_tx\",outcome=\"ok\"}"));
    }

    #[tokio::test]
    async fn scheduler_client_lock_timeout_releases_lookup_capacity() {
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(None, indexer_addr).await;
        let write_lock = ctx.scheduler.write().await;
        let deadline = Instant::now() + std::time::Duration::from_millis(20);
        let mut lookup = Box::pin(transaction_is_pending(&ctx, deadline, test_hash()));
        poll_pending(lookup.as_mut());
        assert_lookup_gauges(&gathered_metrics(&ctx).await, 0, 1);
        assert!(matches!(
            watchdog(lookup).await,
            Err(GatewayError::BackendTimeout)
        ));
        drop(write_lock);
        let metrics = gathered_metrics(&ctx).await;
        assert_lookup_gauges(&metrics, 0, 0);
        assert!(metrics.contains("groundhog_gateway_scheduler_lookup_wait_duration_seconds_count{outcome=\"timeout\",stage=\"scheduler_client\"} 1\n"));
        assert_eq!(
            ctx.scheduler_lookup_execution.available_permits(),
            SCHEDULER_LOOKUP_CONCURRENCY
        );
        assert_eq!(
            ctx.scheduler_lookup_admission.available_permits(),
            SCHEDULER_LOOKUP_MAX_OUTSTANDING
        );
    }

    #[tokio::test]
    async fn status_errors_when_scheduler_lookup_fails_and_indexer_misses() {
        let (scheduler_addr, _scheduler) = start_fake_scheduler(FakeSchedulerMode::RpcFail).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;

        let err = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect_err("bug: failed mempool lookup must not produce a definite status");

        assert!(matches!(err, GatewayError::BackendUnavailable(_)));
        assert!(
            ctx.scheduler.read().await.is_none(),
            "transport failure should clear the matching scheduler generation"
        );
    }

    #[tokio::test]
    async fn status_errors_when_lookup_has_no_slot_and_indexer_misses() {
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::EmptyBatch).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;

        let err = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect_err("bug: a lookup with no per-tx answer must not produce a definite status");

        assert!(matches!(err, GatewayError::BackendUnavailable(_)));
    }

    #[tokio::test]
    async fn status_errors_when_lookup_returns_extra_slot_and_indexer_misses() {
        let (scheduler_addr, _scheduler) = start_fake_scheduler(FakeSchedulerMode::ExtraSlot).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;

        let err = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect_err("bug: extra per-tx answers must not produce a definite status");

        assert!(matches!(err, GatewayError::BackendUnavailable(_)));
    }

    #[tokio::test]
    async fn status_is_unknown_when_mempool_and_indexer_both_miss() {
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::MempoolMiss).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;

        let status = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect("bug: consulted-and-missed lookups should produce a status");

        assert!(matches!(status, TransactionStatus::Unknown));
    }

    #[tokio::test]
    async fn saturated_lookup_admission_does_not_clear_healthy_scheduler() {
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::MempoolMiss).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(Some(scheduler_addr), indexer_addr).await;
        let mut permits = Vec::new();
        for _ in 0..SCHEDULER_LOOKUP_MAX_OUTSTANDING {
            permits.push(
                ctx.scheduler_lookup_admission
                    .clone()
                    .try_acquire_owned()
                    .expect("test should reserve lookup capacity"),
            );
        }

        let error = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect_err("saturated lookup admission should not claim an unknown status");

        assert!(matches!(error, GatewayError::Overloaded(_)));
        assert!(
            ctx.scheduler.read().await.is_some(),
            "local admission saturation must not clear a healthy scheduler"
        );

        drop(permits);
        let status = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect("lookup should recover when admission capacity is released");
        assert!(matches!(status, TransactionStatus::Unknown));
    }

    #[tokio::test]
    async fn stale_transport_failure_does_not_clear_new_scheduler_generation() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (old_scheduler_addr, _old_scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedRpcFail {
                started: started.clone(),
                release: release.clone(),
            })
            .await;
        let (new_scheduler_addr, _new_scheduler) =
            start_fake_scheduler(FakeSchedulerMode::MempoolMiss).await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = Arc::new(test_context(Some(old_scheduler_addr), indexer_addr).await);

        let lookup = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash()).await
            }
        });
        started.notified().await;

        let normal_client = ctx
            .scheduler
            .read()
            .await
            .as_ref()
            .expect("scheduler should remain connected")
            .clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            normal_client.schedgateway_intro(&PROTOCOL_VERSION),
        )
        .await
        .expect("blocked lookup must not occupy the normal scheduler lane")
        .expect("normal scheduler lane should remain healthy");

        let replacement = try_connect(
            "replacement scheduler",
            &new_scheduler_addr.to_string(),
            1,
            SchedGateway_SchedGateway1_Client::new,
        )
        .await
        .expect("replacement scheduler should connect")
        .expect("replacement scheduler should be present");
        let replacement =
            SchedulerClient::new(replacement.clone(), replacement.clone(), replacement);
        let replacement_generation = replacement.generation();
        *ctx.scheduler.write().await = Some(replacement);
        release.notify_one();

        assert!(matches!(
            lookup.await.expect("lookup task should join"),
            Err(GatewayError::BackendUnavailable(_))
        ));
        assert_eq!(
            ctx.scheduler
                .read()
                .await
                .as_ref()
                .expect("replacement scheduler should survive")
                .generation(),
            replacement_generation
        );
    }

    #[tokio::test]
    async fn canceled_lookups_preserve_scheduler_and_recover_capacity() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (scheduler_addr, _scheduler) =
            start_fake_scheduler(FakeSchedulerMode::GatedThenMempoolMiss {
                gated_calls: SCHEDULER_LOOKUP_CONCURRENCY,
                calls,
                started: started.clone(),
                release: release.clone(),
            })
            .await;
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = Arc::new(test_context(Some(scheduler_addr), indexer_addr).await);
        let generation = ctx
            .scheduler
            .read()
            .await
            .as_ref()
            .expect("scheduler should be connected")
            .generation();

        for _ in 0..SCHEDULER_LOOKUP_CONCURRENCY {
            let lookup = tokio::spawn({
                let ctx = ctx.clone();
                async move {
                    get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash()).await
                }
            });
            started.notified().await;
            lookup.abort();
            let _ = lookup.await;
            release.notify_one();
        }

        let permits = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            ctx.scheduler_lookup_admission.clone().acquire_many_owned(
                u32::try_from(SCHEDULER_LOOKUP_MAX_OUTSTANDING)
                    .expect("test lookup capacity should fit u32"),
            ),
        )
        .await
        .expect("cancellation cleanup should release lookup admission")
        .expect("lookup semaphore should remain open");
        assert_eq!(
            ctx.scheduler
                .read()
                .await
                .as_ref()
                .expect("lookup cancellation must preserve the scheduler")
                .generation(),
            generation
        );
        drop(permits);
        // Admission is held by the worker until abort cleanup, alongside its gauge.
        assert_lookup_gauges(&gathered_metrics(&ctx).await, 0, 0);
        assert_eq!(
            ctx.scheduler_lookup_execution.available_permits(),
            SCHEDULER_LOOKUP_CONCURRENCY
        );

        let scheduler = ctx
            .scheduler
            .read()
            .await
            .as_ref()
            .expect("scheduler should remain connected")
            .clone();
        scheduler
            .schedgateway_intro(&PROTOCOL_VERSION)
            .await
            .expect("normal scheduler lane should remain available");
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash()),
        )
        .await
        .expect("lookup capacity should recover after cancellation")
        .expect("replacement lookup connections should remain usable");
        assert!(matches!(status, TransactionStatus::Unknown));
    }
}
