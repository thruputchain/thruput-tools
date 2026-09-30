//! Canonical transaction hashing.

use crate::SigningError;
use groundhog_xdr::transaction::Transaction;
use groundhog_xdr::types::SHA256Hash;
use sha2::{Digest, Sha256};

/// Hashes the canonical XDR encoding of the inner transaction body.
pub fn transaction_hash(tx: &Transaction) -> Result<SHA256Hash, SigningError> {
    let tx_bytes = xdrs::txs::encode(tx).map_err(|e| SigningError::XdrEncode(e.to_string()))?;
    let digest: [u8; 32] = Sha256::digest(&tx_bytes).into();
    Ok(SHA256Hash::from(digest))
}
