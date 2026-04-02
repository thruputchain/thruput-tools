//! `groundhog_getTransactionReceipt` and `groundhog_getTransactionStatus`
//! handlers.

use groundhog_rpc_api_types::{
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};
use groundhog_xdr::transaction::SignedTransactionHash;
use xdr_common::VecM;

use crate::context::GatewayContext;
use crate::error::{self, GatewayError};

/// Full receipt for a committed transaction.
///
/// **Deferred** — requires hogd indexer client (not available in Rust yet).
///
/// Implementation plan:
/// 1. Query hogd indexer `get_transaction_info(tx_hash)`
/// 2. If not found, return `None`
/// 3. Parse each `TxEmittedEvent.serialized_event` into topics + data
///    (see `docs/system-design.md` "Event Parsing")
/// 4. Map `ExecResult` → `ExecStatus`
/// 5. Build `TransactionReceipt`
#[tracing::instrument(skip_all)]
pub async fn get_transaction_receipt(
    _ctx: &GatewayContext,
    _tx_hash: TxHash,
) -> Result<Option<TransactionReceipt>, GatewayError> {
    Err(error::not_implemented("getTransactionReceipt"))
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
///
/// TODO: wire indexer query (step 2) when Rust client is available.
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn get_transaction_status(
    ctx: &GatewayContext,
    tx_hash: TxHash,
) -> Result<TransactionStatus, GatewayError> {
    // 1. Check scheduler (best-effort).
    let guard = ctx.scheduler.read().await;
    if let Some(client) = guard.as_ref() {
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

    // 2. Indexer check — deferred.
    // When available: query indexer, return Committed{height, block_hash} or Unknown.

    Ok(TransactionStatus::Unknown)
}
