//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.

use groundhog_indexer::indexer::TxInfoResult;
use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::{
    receipt::{ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_xdr::transaction::SignedTransactionHash;
use xdr_common::VecM;

use crate::context::GatewayContext;
use crate::error::GatewayError;

use super::parse_event::parse_emitted_event;

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

    let hash = SignedTransactionHash::from(tx_hash.0 .0.into_inner());
    let result = client
        .get_transaction_info(&hash)
        .await
        .map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;

    match result {
        TxInfoResult::TxInfoFound { info } => {
            let events = info
                .events
                .events
                .iter()
                .filter_map(parse_emitted_event)
                .collect();

            Ok(Some(TransactionReceipt {
                tx_hash,
                height: info.height,
                // TODO: map ExecResult → ExecStatus when indexer provides it.
                // Currently TxInfo only has height + events, no exec result.
                status: ExecStatus::Ok,
                events,
            }))
        }
        _ => Ok(None),
    }
}

/// Lightweight status check.
///
/// Lookup order:
/// 1. **Scheduler (mempool)** -> if found, return `Pending`
/// 2. **Indexer** -> if found, return `Committed { height, block_hash }`
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
        let hash = SignedTransactionHash::from(tx_hash.0 .0.into_inner());
        let batch: VecM<SignedTransactionHash> = VecM(vec![hash]);

        match client.schedcoord_lookup_transactions(&batch).await {
            Ok(results) => {
                if results.iter().next().is_some_and(|opt| opt.is_some()) {
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
        let hash = SignedTransactionHash::from(tx_hash.0 .0.into_inner());
        match client.get_transaction_info(&hash).await {
            Ok(result) => {
                if let TxInfoResult::TxInfoFound { info } = result {
                    // TODO: include block_hash when indexer provides it.
                    // Currently TxInfo only has height, not block hash.
                    return Ok(TransactionStatus::Committed {
                        height: info.height,
                        block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
                    });
                }
            }
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
