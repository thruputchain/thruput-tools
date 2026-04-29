use groundhog_rpc_api_types::{hex::Hex0x, transaction::TxHash};
use sha2::Digest;

use crate::{ValidatedTransaction, error::RpcServerError};

/// Decode, validate, and hash a raw signed transaction parameter.
pub fn validate_raw_transaction(tx_hex: &str) -> Result<ValidatedTransaction, RpcServerError> {
    let hex_str = tx_hex
        .strip_prefix("0x")
        .or_else(|| tx_hex.strip_prefix("0X"))
        .unwrap_or(tx_hex);
    let tx_bytes =
        hex::decode(hex_str).map_err(|e| RpcServerError::TxInvalid(format!("invalid hex: {e}")))?;

    validate_raw_transaction_bytes(tx_bytes)
}

/// Validate and hash raw signed transaction XDR bytes.
pub fn validate_raw_transaction_bytes(
    tx_bytes: Vec<u8>,
) -> Result<ValidatedTransaction, RpcServerError> {
    if u32::try_from(tx_bytes.len()).is_err() {
        return Err(RpcServerError::TxInvalid("transaction too large".into()));
    }

    let signed_tx: groundhog_xdr::transaction::SignedTransaction = xdrs::txs::decode(&tx_bytes)
        .map_err(|e| RpcServerError::TxInvalid(format!("invalid XDR: {e}")))?;

    let tx_xdr = xdrs::txs::encode(&signed_tx.tx)
        .map_err(|e| RpcServerError::TxInvalid(format!("cannot re-encode tx: {e}")))?;
    let hash_bytes: [u8; 32] = sha2::Sha256::digest(&tx_xdr).into();
    let hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from(hash_bytes)));

    Ok(ValidatedTransaction {
        bytes: tx_bytes,
        hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
000000040000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000\
00000000000000000000000001000000000000000000000000706f58acd087550f315030db4beeb1527e86\
f0e12d9781957cdcc522f33e5d719b93c200381cff5bfb26a1ee8ce7874ffd485322cd5a0806605d2ffa\
51c8cf0f";

    #[test]
    fn invalid_hex_is_rejected() {
        let err = validate_raw_transaction("not-valid-hex")
            .expect_err("bug: invalid hex should be rejected");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn invalid_xdr_is_rejected() {
        let err = validate_raw_transaction("0x00").expect_err("bug: invalid XDR should fail");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn computes_canonical_hash_from_inner_transaction() {
        let tx = validate_raw_transaction(SIGNED_TX_XDR)
            .expect("bug: signed tx fixture should validate");

        assert_eq!(
            hex::encode(tx.hash.0.0.as_ref()),
            "6e94695b6af0fbc13d96876f7fb8e940cd9db4d247e53614a45da252b3a7853a"
        );
    }
}
