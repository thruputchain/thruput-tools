//! `groundhog_sendRawTransaction` handler.
//!
//! Receives raw signed transaction bytes, computes the canonical tx hash
//! (`sha256(xdrs::txs::encode(signed_tx.tx))`), and hands off to the
//! per-gateway `BatchSubmitter` for adaptive batching.  The submitter
//! aggregates concurrent client submissions into a single
//! `schedulercoord_submit_unvetted` RPC per flush, amortizing the
//! per-call overhead (RPC framing, mempool lock acquire, downstream
//! coordinator notify).
//!
//! See `crate::submitter` for the batching design.
//!
//! The backend choice is a deployment/topology decision:
//! - `hogd_scheduler`: gateway talks directly to mempool admission
//! - `chuckd_coordinator`: gateway talks to chuckd, which relays to hogd
//!
//! Client-facing semantics are unchanged by batching: a returned
//! `TxHash` means the submission was accepted into the mempool's
//! batch-ingest path, exactly as before.
//!
//! TODO: implement `chuckd_coordinator` path when chuckd x23 submit surface
//! is defined in protocol-rs.

use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::transaction::TxHash;
use sha2::Digest;

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Submit a signed transaction. Returns the canonical tx hash.
///
/// Flow:
/// 1. XDR-deserialize `tx_bytes` as `SignedTransaction`
/// 2. Compute `sha256(xdrs::txs::encode(signed_tx.tx))` -> `TxHash`
/// 3. Hand to `BatchSubmitter` for adaptive batching; await the fanout
/// 4. Return `TxHash` on mempool acceptance
#[tracing::instrument(skip_all, fields(tx_bytes_len = tx_bytes.len()))]
pub async fn send_raw_transaction(
    ctx: &GatewayContext,
    tx_bytes: Vec<u8>,
) -> Result<TxHash, GatewayError> {
    // 1. Deserialize to validate and extract the inner Transaction.
    let signed_tx: groundhog_xdr::transaction::SignedTransaction = xdrs::txs::decode(&tx_bytes)
        .map_err(|e| GatewayError::TxInvalid(format!("invalid XDR: {e}")))?;

    // 2. Compute TxHash = sha256(xdrs::txs::encode(signed_tx.tx)).
    let tx_xdr = xdrs::txs::encode(&signed_tx.tx)
        .map_err(|e| GatewayError::TxInvalid(format!("cannot re-encode tx: {e}")))?;
    let hash_bytes: [u8; 32] = sha2::Sha256::digest(&tx_xdr).into();
    let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from(hash_bytes)));

    // 3. Early-reject oversized txs so we don't push them into a batch
    //    that will later fail to encode.
    if u32::try_from(tx_bytes.len()).is_err() {
        return Err(GatewayError::TxInvalid("transaction too large".into()));
    }

    // 4. Forward to the batch submitter, moving ownership to avoid a copy.
    ctx.batch_submitter.submit(tx_bytes, tx_hash).await?;

    Ok(tx_hash)
}
