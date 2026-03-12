//! `groundhog_sendRawTransaction` handler.
//!
//! Receives raw signed transaction bytes, computes the canonical tx hash
//! (`sha256(xdr_serialize(signed_tx.tx))`), and forwards to the hogd
//! scheduler via x23 `submit_unvetted`.

use groundhog_rpc_api_types::transaction::TxHash;

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Submit a signed transaction. Returns the canonical tx hash.
///
/// Flow:
/// 1. XDR-deserialize `tx_bytes` as `SignedTransaction`
/// 2. Compute `sha256(xdr_serialize(signed_tx.tx))` → `TxHash`
/// 3. Forward to scheduler via `submit_unvetted`
/// 4. Return `TxHash` on mempool acceptance
#[tracing::instrument(skip_all, fields(tx_bytes_len = tx_bytes.len()))]
pub async fn send_raw_transaction(
    _ctx: &GatewayContext,
    tx_bytes: &[u8],
) -> Result<TxHash, GatewayError> {
    // TODO: XDR-deserialize tx_bytes into SignedTransaction
    //   let signed_tx = SignedTransaction::from_xdr(tx_bytes)
    //       .map_err(|e| GatewayError::TxInvalid(e.to_string()))?;

    // TODO: compute TxHash = sha256(xdr_serialize(signed_tx.tx))
    //   let hash = SHA256Hash::from(sha2::Sha256::digest(
    //       signed_tx.tx.to_xdr()
    //   ));

    // TODO: forward to ctx.scheduler.submit_unvetted()
    //   ctx.scheduler.submit_unvetted(&[signed_tx]).await
    //       .map_err(|e| GatewayError::TxRejected(e.to_string()))?;

    let _ = tx_bytes;
    todo!("blocked on x23 scheduler client")
}
