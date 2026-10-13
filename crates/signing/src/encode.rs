//! XDR encoding helpers.

use crate::SigningError;
use thruput_xdr::transaction::{SignedTransaction, Transaction};

/// Encodes a transaction to XDR bytes.
pub fn encode_transaction_xdr(tx: &Transaction) -> Result<Vec<u8>, SigningError> {
    xdrs::txs::encode(tx).map_err(|e| SigningError::XdrEncode(e.to_string()))
}

/// Encodes a signed transaction to XDR bytes.
pub fn encode_signed_transaction_xdr(tx: &SignedTransaction) -> Result<Vec<u8>, SigningError> {
    xdrs::txs::encode(tx).map_err(|e| SigningError::XdrEncode(e.to_string()))
}
