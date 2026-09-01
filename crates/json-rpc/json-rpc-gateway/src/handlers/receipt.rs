//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.

use groundhog_indexer::indexer::TxInfoResult;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::{
    hex::HexBytes,
    receipt::{DETERMINISTIC_ERROR_KIND_FAULT, ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_xdr::exec_result::ExecResult;
use groundhog_xdr::transaction::{InvocationFrameOutput, TransactionHash};
use std::{sync::Arc, time::Instant};
use tokio::{sync::RwLock, task::JoinHandle};
use xdr_common::VecM;

use crate::config::HealthBackend;
use crate::context::{GatewayContext, clear_scheduler_if_current};
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, REASON_PARSE_FAILED};

use super::backend_deadline;
use super::parse_event::parse_emitted_event;

struct SchedulerLookupGuard<T: Send + 'static> {
    scheduler: Arc<RwLock<Option<crate::context::SchedulerClient>>>,
    generation: u64,
    worker: Option<JoinHandle<T>>,
    armed: bool,
}

impl<T: Send + 'static> SchedulerLookupGuard<T> {
    fn new(
        scheduler: Arc<RwLock<Option<crate::context::SchedulerClient>>>,
        generation: u64,
        worker: JoinHandle<T>,
    ) -> Self {
        Self {
            scheduler,
            generation,
            worker: Some(worker),
            armed: true,
        }
    }

    fn worker_mut(&mut self) -> &mut JoinHandle<T> {
        self.worker
            .as_mut()
            .expect("bug: scheduler worker should exist")
    }

    async fn clear_and_abort(&mut self) {
        clear_scheduler_if_current(&self.scheduler, self.generation).await;
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
            for raw in &info.events.events.0 {
                if let Some(event) = parse_emitted_event(raw) {
                    events.push(event);
                } else {
                    ctx.metrics
                        .observe_malformed_event_dropped(REASON_PARSE_FAILED);
                }
            }
            let (return_data, abort_data) = match &info.events.root_output {
                InvocationFrameOutput::Return { return_buffer } => {
                    (Some(HexBytes(return_buffer.0.clone())), None)
                }
                InvocationFrameOutput::Abort { abort_buffer } => {
                    (None, Some(HexBytes(abort_buffer.0.clone())))
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

/// Lightweight status check.
///
/// Lookup order:
/// 1. **Scheduler (mempool)** -> if found, return `Pending`
/// 2. **Indexer** -> if committed, return `Committed { height }`;
///    if the indexer holds a recent nondeterministic-failure
///    diagnostic, return `RecentFailure` (best-effort, advisory)
/// 3. **Neither** -> return `Unknown`
///
/// Degraded-mode behavior:
/// - Scheduler unreachable, indexer up → terminal indexer answers
///   (`Committed`, `RecentFailure`) still return. An indexer miss returns
///   `BackendUnavailable` instead of `Unknown`: without a mempool answer
///   the gateway cannot claim the transaction is nowhere, and a
///   still-pending transaction must not read as gone.
/// - Indexer unreachable → return `BackendUnavailable` error.
///   The indexer is the authoritative source.
async fn transaction_is_pending(
    ctx: &GatewayContext,
    deadline: Instant,
    tx_hash: TxHash,
) -> Result<bool, GatewayError> {
    let client = ctx
        .scheduler
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("scheduler not connected".into()))?;
    let permit = ctx
        .scheduler_lookup_admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            GatewayError::Overloaded(
                "scheduler transaction-status lookup capacity exhausted".into(),
            )
        })?;
    let generation = client.generation();
    let lookup_client = client.lookup_client().clone();
    let hash = TransactionHash::from(tx_hash.0.0.into_inner());
    let batch: VecM<TransactionHash> = VecM(vec![hash]);
    let started_at = Instant::now();
    let worker = tokio::spawn(async move {
        let _permit = permit;
        lookup_client.schedcoord_lookup_transactions(&batch).await
    });
    let mut guard = SchedulerLookupGuard::new(ctx.scheduler.clone(), generation, worker);
    let rpc_result = if let Ok(result) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        guard.worker_mut(),
    )
    .await
    {
        Ok(result.unwrap_or(Err(rpc_driver::RpcError::SystemError)))
    } else {
        guard.clear_and_abort().await;
        Err(())
    };
    if matches!(rpc_result, Ok(Err(_))) {
        clear_scheduler_if_current(&ctx.scheduler, generation).await;
    }
    guard.disarm();
    ctx.metrics.observe_backend_request(
        HealthBackend::Scheduler,
        "schedcoord_lookup_transactions",
        match &rpc_result {
            Ok(Ok(_)) => BackendRequestOutcome::Ok,
            Ok(Err(_)) => BackendRequestOutcome::Error,
            Err(()) => BackendRequestOutcome::Timeout,
        },
        started_at.elapsed(),
    );
    match rpc_result {
        Ok(Ok(results)) => match results.iter().next() {
            Some(Some(_)) => Ok(true),
            Some(None) => Ok(false),
            None => Err(GatewayError::BackendUnavailable(
                "scheduler lookup returned no slot for the queried transaction".into(),
            )),
        },
        Ok(Err(error)) => Err(GatewayError::BackendUnavailable(format!(
            "scheduler lookup failed: {error}"
        ))),
        Err(()) => Err(GatewayError::BackendTimeout),
    }
}

#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_status(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    tx_hash: TxHash,
) -> Result<TransactionStatus, GatewayError> {
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    // Scheduler failures are advisory for terminal indexer answers, but an
    // indexer miss cannot become `Unknown` without a successful mempool miss.
    let scheduler_failure = match transaction_is_pending(ctx, deadline, tx_hash).await {
        Ok(true) => return Ok(TransactionStatus::Pending),
        Ok(false) => None,
        Err(error) => {
            tracing::warn!(%error, "scheduler unavailable, cannot detect pending txs");
            Some(error)
        }
    };

    // 2. Check indexer (authoritative).
    let maybe_indexer = ctx.indexer.read().await.clone();
    if let Some(client) = maybe_indexer.as_ref() {
        let hash = TransactionHash::from(tx_hash.0.0.into_inner());
        let started_at = Instant::now();
        let rpc_result = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            client.get_transaction_info(&hash),
        )
        .await;
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
                return Ok(TransactionStatus::Committed {
                    height: info.height,
                });
            }
            Ok(Ok(TxInfoResult::TxInfoRecentFailure { recent_failure })) => {
                return Ok(TransactionStatus::from(&recent_failure));
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

    if scheduler_failure.is_none() {
        Ok(TransactionStatus::Unknown)
    } else {
        // `Unknown` tells the client "not in mempool or indexer"; without a
        // mempool answer that claim is unverified and a still-pending
        // transaction would read as gone (inviting an unsafe re-sign).
        Err(scheduler_failure.unwrap_or_else(|| {
            GatewayError::BackendUnavailable(
                "scheduler unreachable: transaction is not committed but may still be pending"
                    .into(),
            )
        }))
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
        ConsensusValue, CoordinatorRegistrationOutcome, DbSchedResult,
        ParallelSignedTransactionBatch, Proposal, SchedulerCommitOutcome, SchedulerGetTipResult,
        SchedulerProposalResult, SchedulerValidationResult,
        sched_coord::{
            SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Client,
            SchedulerCoord_SchedulerCoord1_Dispatcher,
        },
    };
    use groundhog_scp::network::ProtocolCommit as Protocol;
    use groundhog_x23_transport::try_connect;
    use groundhog_xdr::{
        genesis::GenesisConfiguration,
        header::ProposalContext,
        transaction::{OptSignedTransactionBatch, TransactionHashBatch},
        types::SHA256Hash,
        validator::ValidatorID,
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
        BackendClient, SCHEDULER_LOOKUP_CONCURRENCY, SchedulerClient, connect_optional_scheduler,
    };
    use crate::metrics::GatewayMetrics;

    /// Indexer that answers `get_transaction_info` with not-found.
    struct NotFoundIndexer;

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for NotFoundIndexer {
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
            Ok(TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            })
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
        /// Mempool lookup succeeds and reports the transaction absent.
        MempoolMiss,
        /// Mempool lookup succeeds but returns no slot for the queried hash.
        EmptyBatch,
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

    #[async_trait]
    impl SchedulerCoord_SchedulerCoord1 for FakeScheduler {
        async fn schedulercoord_null(&self) -> Result<(), ProgramError> {
            Ok(())
        }

        async fn schedulercoord_intro_coord(
            &self,
            _arg0: groundhog_scheduler::CoordCard,
        ) -> Result<CoordinatorRegistrationOutcome, ProgramError> {
            panic!("bug: status tests should not call intro_coord")
        }

        async fn schedulercoord_submit_unvetted(
            &self,
            _arg0: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            panic!("bug: status tests should not submit")
        }

        async fn schedulercoord_submit_unvetted_with_outcomes(
            &self,
            _arg0: ParallelSignedTransactionBatch,
        ) -> Result<groundhog_scheduler::SchedulerSubmitOutcomeBatch, ProgramError> {
            panic!("bug: status tests should not submit")
        }

        async fn schedulercoord_submit_vetted(
            &self,
            _arg0: ValidatorID,
            _arg1: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            panic!("bug: status tests should not submit")
        }

        async fn schedulercoord_propose_block(
            &self,
            _arg0: groundhog_scheduler::BlockID,
            _arg1: ProposalContext,
        ) -> Result<SchedulerProposalResult, ProgramError> {
            panic!("bug: status tests should not propose")
        }

        async fn schedulercoord_validate_block(
            &self,
            _arg0: groundhog_scheduler::BlockID,
            _arg1: Proposal,
        ) -> Result<SchedulerValidationResult, ProgramError> {
            panic!("bug: status tests should not validate")
        }

        async fn schedulercoord_commit_block(
            &self,
            _arg0: groundhog_scheduler::BlockID,
            _arg1: ConsensusValue,
        ) -> Result<SchedulerCommitOutcome, ProgramError> {
            panic!("bug: status tests should not commit")
        }

        async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
            panic!("bug: status tests should not read the tip")
        }

        async fn schedcoord_load_genesis_configuration(
            &self,
            _arg0: GenesisConfiguration,
        ) -> Result<DbSchedResult, ProgramError> {
            panic!("bug: status tests should not load genesis")
        }

        async fn schedulercoord_commit_genesis_ceremony(
            &self,
            _request: groundhog_scheduler::GenesisCeremonyRequest,
        ) -> Result<groundhog_scheduler::GenesisCeremonyResult, ProgramError> {
            panic!("bug: status tests should not commit a genesis ceremony")
        }

        async fn schedcoord_lookup_transactions(
            &self,
            _arg0: TransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            match &self.mode {
                FakeSchedulerMode::MempoolMiss => Ok(VecM(vec![None])),
                FakeSchedulerMode::EmptyBatch => Ok(VecM(vec![])),
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
    }

    async fn start_not_found_indexer() -> (
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
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(NotFoundIndexer)),
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
        services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
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
            Some(addr) => connect_optional_scheduler(&addr.to_string(), 1)
                .await
                .expect("bug: test scheduler should connect"),
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

    fn test_hash() -> TxHash {
        TxHash(groundhog_json_rpc_types::hex::Hex0x(SHA256Hash::from(
            [7u8; 32],
        )))
    }

    #[tokio::test]
    async fn status_errors_when_scheduler_not_connected_and_indexer_misses() {
        let (indexer_addr, _indexer) = start_not_found_indexer().await;
        let ctx = test_context(None, indexer_addr).await;

        let err = get_transaction_status(&ctx, RpcRequestContext::unbounded(), test_hash())
            .await
            .expect_err("bug: unconsulted mempool must not produce a definite status");

        assert!(matches!(err, GatewayError::BackendUnavailable(_)));
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
        for _ in 0..SCHEDULER_LOOKUP_CONCURRENCY {
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
            normal_client.schedulercoord_null(),
        )
        .await
        .expect("blocked lookup must not occupy the normal scheduler lane")
        .expect("normal scheduler lane should remain healthy");

        let replacement = try_connect(
            "replacement scheduler",
            &new_scheduler_addr.to_string(),
            1,
            SchedulerCoord_SchedulerCoord1_Client::new,
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
                u32::try_from(SCHEDULER_LOOKUP_CONCURRENCY)
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

        let scheduler = ctx
            .scheduler
            .read()
            .await
            .as_ref()
            .expect("scheduler should remain connected")
            .clone();
        scheduler
            .schedulercoord_null()
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
