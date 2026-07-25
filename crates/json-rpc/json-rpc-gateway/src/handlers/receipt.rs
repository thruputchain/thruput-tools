//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.

use groundhog_indexer::indexer::TxInfoResult;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::{
    hex::HexBytes,
    receipt::{ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_xdr::exec_result::ExecResult;
use groundhog_xdr::transaction::{InvocationFrameOutput, SignedTransactionHash};
use std::time::Instant;
use xdr_common::VecM;

use crate::config::HealthBackend;
use crate::context::{GatewayContext, clear_scheduler_if_current};
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, REASON_PARSE_FAILED};

use super::backend_deadline;
use super::parse_event::parse_emitted_event;

/// Map hogd's `ExecResult` (XDR int enum) to the gateway's `ExecStatus`.
fn map_exec_result(r: ExecResult) -> ExecStatus {
    if r == ExecResult::ExecResultOK {
        ExecStatus::Ok
    } else if r == ExecResult::ExecResultTxRootFrameReverted {
        ExecStatus::Reverted { message: None }
    } else if r == ExecResult::ExecResultTxDeterministicError {
        // TODO: ExecResult int-enum doesn't carry a sub-type; use "unknown"
        // until the indexer exposes finer-grained error codes (e.g.
        // "out_of_gas", "stack_overflow").
        ExecStatus::DeterministicError {
            kind: "unknown".into(),
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

    let hash = SignedTransactionHash::from(tx_hash.0.0.into_inner());
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
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_status(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    tx_hash: TxHash,
) -> Result<TransactionStatus, GatewayError> {
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    // 1. Check scheduler mempool. Best-effort in that a failure does not
    // abort the indexer lookup, but tracked: `Unknown` asserts the
    // transaction is in neither place, which requires both to have answered.
    let mut mempool_checked = false;
    let maybe_scheduler = ctx.scheduler.read().await.clone();
    if let Some(client) = maybe_scheduler.as_ref() {
        let generation = client.generation();
        let hash = SignedTransactionHash::from(tx_hash.0.0.into_inner());
        let batch: VecM<SignedTransactionHash> = VecM(vec![hash]);

        let started_at = Instant::now();
        let client = client.clone();
        let mut call =
            tokio::spawn(async move { client.schedcoord_lookup_transactions(&batch).await });
        let rpc_result = if let Ok(result) = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            &mut call,
        )
        .await
        {
            Ok(result.unwrap_or(Err(rpc_driver::RpcError::SystemError)))
        } else {
            clear_scheduler_if_current(&ctx.scheduler, generation).await;
            call.abort();
            let _ = call.await;
            Err(())
        };
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
            // Only a per-hash slot counts as a mempool answer; an empty
            // batch left the queried transaction unaccounted for.
            Ok(Ok(results)) => match results.iter().next() {
                Some(Some(_)) => return Ok(TransactionStatus::Pending),
                Some(None) => mempool_checked = true,
                None => {
                    tracing::warn!("scheduler lookup returned no slot for the queried tx hash");
                }
            },
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "scheduler unreachable, cannot detect pending txs");
            }
            Err(()) => tracing::warn!("scheduler lookup timed out, cannot detect pending txs"),
        }
    } else {
        tracing::warn!("scheduler not connected, cannot detect pending txs");
    }

    // 2. Check indexer (authoritative).
    let maybe_indexer = ctx.indexer.read().await.clone();
    if let Some(client) = maybe_indexer.as_ref() {
        let hash = SignedTransactionHash::from(tx_hash.0.0.into_inner());
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

    if mempool_checked {
        Ok(TransactionStatus::Unknown)
    } else {
        // `Unknown` tells the client "not in mempool or indexer"; without a
        // mempool answer that claim is unverified and a still-pending
        // transaction would read as gone (inviting an unsafe re-sign).
        Err(GatewayError::BackendUnavailable(
            "scheduler unreachable: transaction is not committed but may still be pending".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use groundhog_indexer::{
        AddressTransactionQuery, EventPage, EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
        TransactionPage, TransactionQuery, TransactionRefPage, TxInfoOutcome,
    };
    use groundhog_scheduler::{
        CoordinatorRegistrationOutcome, DbSchedResult, DbTip, Nomination,
        ParallelSignedTransactionBatch, SchedulerCommitOutcome, SchedulerGetTipResult,
        SchedulerNominationResult, SchedulerValidationResult,
        sched_coord::{
            SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Client,
            SchedulerCoord_SchedulerCoord1_Dispatcher,
        },
    };
    use groundhog_scp::network::Protocol;
    use groundhog_x23_transport::try_connect;
    use groundhog_xdr::genesis::GenesisConfiguration;
    use groundhog_xdr::transaction::{OptSignedTransactionBatch, SignedTransactionHashBatch};
    use groundhog_xdr::types::SHA256Hash;
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;
    use tokio::sync::RwLock;

    use super::*;
    use crate::config::GatewayConfig;
    use crate::context::SchedulerClient;
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
            _arg0: SignedTransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            Ok(TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            })
        }

        async fn get_events(&self, _arg0: EventQuery) -> Result<EventPage, ProgramError> {
            panic!("bug: status tests should not query events")
        }

        async fn get_transactions(
            &self,
            _arg0: TransactionQuery,
        ) -> Result<TransactionPage, ProgramError> {
            panic!("bug: status tests should not query transactions")
        }

        async fn get_transactions_by_address(
            &self,
            _arg0: AddressTransactionQuery,
        ) -> Result<TransactionRefPage, ProgramError> {
            panic!("bug: status tests should not query transactions by address")
        }
    }

    #[derive(Clone, Copy)]
    enum FakeSchedulerMode {
        /// Mempool lookup succeeds and reports the transaction absent.
        MempoolMiss,
        /// Mempool lookup succeeds but returns no slot for the queried hash.
        EmptyBatch,
        /// Mempool lookup fails at the RPC layer.
        RpcFail,
    }

    struct FakeScheduler {
        mode: FakeSchedulerMode,
    }

    #[async_trait]
    impl SchedulerCoord_SchedulerCoord1 for FakeScheduler {
        async fn schedulercoord_null(&self) -> Result<(), ProgramError> {
            panic!("bug: status tests should not call null")
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

        async fn schedulercoord_submit_vetted(
            &self,
            _arg0: groundhog_coordinator::discovery::ValidatorID,
            _arg1: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            panic!("bug: status tests should not submit")
        }

        async fn schedulercoord_nominate(
            &self,
            _arg0: DbTip,
        ) -> Result<SchedulerNominationResult, ProgramError> {
            panic!("bug: status tests should not nominate")
        }

        async fn schedulercoord_validate(
            &self,
            _arg0: DbTip,
            _arg1: Nomination,
        ) -> Result<SchedulerValidationResult, ProgramError> {
            panic!("bug: status tests should not validate")
        }

        async fn schedulercoord_commit_block(
            &self,
            _arg0: groundhog_scheduler::BlockID,
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

        async fn schedcoord_lookup_transactions(
            &self,
            _arg0: SignedTransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            match self.mode {
                FakeSchedulerMode::MempoolMiss => Ok(VecM(vec![None])),
                FakeSchedulerMode::EmptyBatch => Ok(VecM(vec![])),
                FakeSchedulerMode::RpcFail => {
                    Err(ProgramError::msg("fake scheduler lookup failure"))
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
            Some(addr) => try_connect(
                "scheduler",
                &addr.to_string(),
                1,
                SchedulerCoord_SchedulerCoord1_Client::new,
            )
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
            Arc::new(RwLock::new(
                scheduler.map(|client| SchedulerClient::new(client.clone(), client)),
            )),
            Arc::new(RwLock::new(indexer)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(GatewayConfig::default()),
            GatewayMetrics::new("test", None).expect("bug: test metrics should register"),
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
}
