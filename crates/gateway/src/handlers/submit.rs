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
//!
//! TODO: implement chuckd_coordinator path when chuckd x23 submit surface
//! is defined in protocol-rs.

use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::transaction::TxHash;
use groundhog_scheduler::parallel::ParallelSignedTransactionBatch;
use groundhog_scheduler::ParallelBatch;
use sha2::Digest;
use xdr_common::{BytesM, VecM};

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Submit a signed transaction. Returns the canonical tx hash.
///
/// Flow:
/// 1. XDR-deserialize `tx_bytes` as `SignedTransaction`
/// 2. Compute `sha256(xdrs::txs::encode(signed_tx.tx))` -> `TxHash`
/// 3. Forward to configured submission backend via `submit_unvetted`
/// 4. Return `TxHash` on mempool acceptance
#[tracing::instrument(skip_all, fields(tx_bytes_len = tx_bytes.len()))]
pub async fn send_raw_transaction(
    ctx: &GatewayContext,
    tx_bytes: &[u8],
) -> Result<TxHash, GatewayError> {
    // 1. Deserialize to validate and extract the inner Transaction.
    let signed_tx: groundhog_xdr::transaction::SignedTransaction = xdrs::txs::decode(tx_bytes)
        .map_err(|e| GatewayError::TxInvalid(format!("invalid XDR: {e}")))?;

    // 2. Compute TxHash = sha256(xdrs::txs::encode(signed_tx.tx)).
    let tx_xdr = xdrs::txs::encode(&signed_tx.tx)
        .map_err(|e| GatewayError::TxInvalid(format!("cannot re-encode tx: {e}")))?;
    let hash_bytes: [u8; 32] = sha2::Sha256::digest(&tx_xdr).into();
    let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from(hash_bytes)));

    // 3. Forward to scheduler (hogd_scheduler backend).
    // TODO: branch on ctx.config.submission_backend for chuckd_coordinator path.
    let guard = ctx.scheduler.read().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| GatewayError::BackendUnavailable("scheduler not connected".into()))?;

    let tx_len = tx_bytes.len() as u32;
    let batch = ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: VecM(vec![1]),
        segment_bytes: VecM(vec![tx_len]),
        xdr: BytesM(tx_bytes.to_vec()),
    });

    client
        .schedulercoord_submit_unvetted(&batch)
        .await
        .map_err(|e| GatewayError::TxRejected(e.to_string()))?;

    Ok(tx_hash)
}
