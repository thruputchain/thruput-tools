//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.
//!
//! Queries hogd indexer (`get_transaction_info`) and scheduler
//! (`lookup_transactions`) via x23 RPC.

use groundhog_rpc_api_types::{
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Full receipt for a committed transaction.
///
/// Flow:
/// 1. Query hogd indexer `get_transaction_info(tx_hash)`
/// 2. If not found, return `None`
/// 3. Parse each `TxEmittedEvent.serialized_event` into topics + data
///    (see `docs/system-design.md` "Event Parsing")
/// 4. Map `ExecResult` → `ExecStatus`
/// 5. Build `TransactionReceipt`
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_receipt(
    _ctx: &GatewayContext,
    tx_hash: TxHash,
) -> Result<Option<TransactionReceipt>, GatewayError> {
    // TODO: query indexer
    //   let tx_info = ctx.indexer
    //       .get_transaction_info(tx_hash.0).await
    //       .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;
    //   let Some(info) = tx_info else { return Ok(None); };
    //
    // TODO: parse events from serialized_event
    //   let events = info.event_log.events.iter()
    //       .map(|e| parse_emitted_event(e))
    //       .collect();
    //
    // TODO: map exec result
    //   let status = map_exec_result(&info.event_log.exec_result);

    let _ = tx_hash;
    todo!("blocked on x23 indexer client")
}

/// Lightweight status check.
///
/// Lookup order:
/// 1. **Scheduler (mempool)** → if found, return `Pending`
/// 2. **Indexer** → if found, return `Committed { height, block_hash }`
/// 3. **Neither** → return `Unknown`
///
/// Degraded-mode behavior:
/// - Scheduler unreachable, indexer up → can return `Committed` or
///   `Unknown` (cannot detect `Pending`). Log warning, don't fail.
/// - Indexer unreachable → return `BackendUnavailable` error.
///   The indexer is the authoritative source.
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_status(
    _ctx: &GatewayContext,
    tx_hash: TxHash,
) -> Result<TransactionStatus, GatewayError> {
    // TODO: 1. Check scheduler (best-effort)
    //   match ctx.scheduler.lookup_transactions(&[tx_hash.0]).await {
    //       Ok(results) if results[0].is_some() => {
    //           return Ok(TransactionStatus::Pending);
    //       }
    //       Err(e) => {
    //           tracing::warn!(error = %e, "scheduler unreachable");
    //       }
    //       _ => {}
    //   }
    //
    // TODO: 2. Check indexer (authoritative)
    //   let tx_info = ctx.indexer
    //       .get_transaction_info(tx_hash.0).await
    //       .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;
    //   match tx_info {
    //       Some(info) => Ok(TransactionStatus::Committed { ... }),
    //       None => Ok(TransactionStatus::Unknown),
    //   }

    let _ = tx_hash;
    todo!("blocked on x23 indexer + scheduler clients")
}
