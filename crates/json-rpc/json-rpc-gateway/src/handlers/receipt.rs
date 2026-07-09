//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.

use groundhog_indexer::indexer::TxInfoResult;
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
use crate::context::GatewayContext;
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, REASON_PARSE_FAILED};

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
    let rpc_result = client.get_transaction_info(&hash).await;
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "get_transaction_info",
        if rpc_result.is_ok() {
            BackendRequestOutcome::Ok
        } else {
            BackendRequestOutcome::Error
        },
        started_at.elapsed(),
    );
    let result =
        rpc_result.map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;

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
/// - Scheduler unreachable, indexer up → can return `Committed` or
///   `Unknown` (cannot detect `Pending`). Log warning, don't fail.
/// - Indexer unreachable → return `BackendUnavailable` error.
///   The indexer is the authoritative source.
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_status(
    ctx: &GatewayContext,
    tx_hash: TxHash,
) -> Result<TransactionStatus, GatewayError> {
    // 1. Check scheduler (best-effort).
    let maybe_scheduler = ctx.scheduler.read().await.clone();
    if let Some(client) = maybe_scheduler.as_ref() {
        let hash = SignedTransactionHash::from(tx_hash.0.0.into_inner());
        let batch: VecM<SignedTransactionHash> = VecM(vec![hash]);

        let started_at = Instant::now();
        let rpc_result = client.schedcoord_lookup_transactions(&batch).await;
        ctx.metrics.observe_backend_request(
            HealthBackend::Scheduler,
            "schedcoord_lookup_transactions",
            if rpc_result.is_ok() {
                BackendRequestOutcome::Ok
            } else {
                BackendRequestOutcome::Error
            },
            started_at.elapsed(),
        );
        match rpc_result {
            Ok(results) => {
                if results.iter().next().is_some_and(Option::is_some) {
                    return Ok(TransactionStatus::Pending);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "scheduler unreachable, cannot detect pending txs");
            }
        }
    } else {
        tracing::warn!("scheduler not connected, cannot detect pending txs");
    }

    // 2. Check indexer (authoritative).
    let maybe_indexer = ctx.indexer.read().await.clone();
    if let Some(client) = maybe_indexer.as_ref() {
        let hash = SignedTransactionHash::from(tx_hash.0.0.into_inner());
        let started_at = Instant::now();
        let rpc_result = client.get_transaction_info(&hash).await;
        ctx.metrics.observe_backend_request(
            HealthBackend::Indexer,
            "get_transaction_info",
            if rpc_result.is_ok() {
                BackendRequestOutcome::Ok
            } else {
                BackendRequestOutcome::Error
            },
            started_at.elapsed(),
        );
        match rpc_result {
            Ok(TxInfoResult::TxInfoFound { info }) => {
                return Ok(TransactionStatus::Committed {
                    height: info.height,
                });
            }
            Ok(TxInfoResult::TxInfoRecentFailure { recent_failure }) => {
                return Ok(TransactionStatus::from(&recent_failure));
            }
            Ok(_) => {}
            Err(e) => {
                return Err(GatewayError::BackendUnavailable(format!("indexer: {e}")));
            }
        }
    } else {
        return Err(GatewayError::BackendUnavailable(
            "indexer not connected".into(),
        ));
    }

    Ok(TransactionStatus::Unknown)
}
