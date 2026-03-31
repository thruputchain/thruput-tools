//! `groundhog_sendRawTransaction` handler.
//!
//! Receives raw signed transaction bytes, computes the canonical tx hash
//! (`sha256(xdrs::txs::encode(signed_tx.tx))`), and forwards to the configured
//! private-network submission backend.
//!
//! The backend choice is a deployment/topology decision:
//! - `hogd_scheduler`: gateway talks directly to mempool admission
//! - `chuckd_coordinator`: gateway talks to chuckd, which relays to hogd
//!
//! Client-facing semantics should stay the same in both modes: success means
//! the transaction was accepted by the authoritative downstream mempool path,
//! not merely queued by an intermediate relay.

use groundhog_rpc_api_types::transaction::TxHash;

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Submit a signed transaction. Returns the canonical tx hash.
///
/// Flow:
/// 1. XDR-deserialize `tx_bytes` as `SignedTransaction`
/// 2. Compute `sha256(xdrs::txs::encode(signed_tx.tx))` → `TxHash`
/// 3. Forward to configured submission backend via `submit_unvetted`
/// 4. Return `TxHash` on mempool acceptance
#[tracing::instrument(skip_all, fields(tx_bytes_len = tx_bytes.len()))]
pub async fn send_raw_transaction(
    _ctx: &GatewayContext,
    tx_bytes: &[u8],
) -> Result<TxHash, GatewayError> {
    // TODO: XDR-deserialize tx_bytes into groundhog_xdr::transaction::SignedTransaction
    //   let signed_tx: groundhog_xdr::SignedTransaction =
    //       xdrs::txs::decode(tx_bytes)
    //           .map_err(|e| GatewayError::TxInvalid(e.to_string()))?;

    // TODO: compute TxHash = sha256(xdrs::txs::encode(&signed_tx.tx))
    //   let tx_xdr = xdrs::txs::encode(&signed_tx.tx)
    //       .map_err(|e| GatewayError::TxInvalid(e.to_string()))?;
    //   let hash = groundhog_xdr::SHA256Hash::from(
    //       sha2::Sha256::digest(tx_xdr).into()
    //   );

    // TODO: forward based on ctx.config.submission_backend
    //   - HogdScheduler: pack signed_tx into
    //     groundhog_scheduler::ParallelSignedTransactionBatch and call
    //     ctx.scheduler.schedulercoord_submit_unvetted(&batch).await
    //   - ChuckdCoordinator: ctx.coordinator.submit_unvetted(&[signed_tx]).await
    //
    // In both cases, preserve the same external contract: only return success
    // once the downstream mempool path has accepted the tx.

    let _ = tx_bytes;
    todo!("blocked on x23 scheduler/coordinator clients")
}
