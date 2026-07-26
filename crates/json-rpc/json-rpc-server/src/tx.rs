use groundhog_json_rpc_types::{hex::Hex0x, transaction::TxHash};
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

    // `SignedTransaction` is `{ tx, signatures }`, so the inner `Transaction`
    // occupies a contiguous prefix of `tx_bytes`. Hash that prefix directly
    // instead of re-encoding the decoded value: it avoids an allocation on the
    // submit path, and it hashes exactly the bytes the client signed rather
    // than trusting XDR to round-trip bit-for-bit.
    let mut reader = xdrs::txs::buf_reader::BufReader::new(&tx_bytes);

    let tx: groundhog_xdr::transaction::Transaction = xdrs::read_xdr(&mut reader)
        .map_err(|e| RpcServerError::TxInvalid(format!("invalid XDR: {e}")))?;
    let tx_end = reader.position();

    // Decode the signatures too, and reject trailing bytes, so this accepts
    // exactly what a whole-buffer `xdrs::txs::decode::<SignedTransaction>`
    // would have accepted.
    let _signatures: xdrs::VecM<groundhog_xdr::transaction::DecoratedSignature> =
        xdrs::read_xdr(&mut reader)
            .map_err(|e| RpcServerError::TxInvalid(format!("invalid XDR: {e}")))?;
    if reader.remaining() > 0 {
        return Err(RpcServerError::TxInvalid(
            "invalid XDR: trailing bytes after signed transaction".into(),
        ));
    }

    let hash_bytes: [u8; 32] = sha2::Sha256::digest(&tx_bytes[..tx_end]).into();
    let hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from(hash_bytes)));

    Ok(ValidatedTransaction {
        bytes: tx_bytes,
        hash,
        block_expiry_height: tx.uniquifier.block_expiry_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical XDR/hash vector only. This transaction is structurally valid
    // and has a stable hash, but it is not auth-valid against empty hogd-dev
    // genesis.
    const CANONICAL_HASH_VECTOR_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
0000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000000000\
000000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695\
031c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d";

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
    fn exposes_block_expiry_height_from_decoded_transaction() {
        let tx = validate_raw_transaction(CANONICAL_HASH_VECTOR_SIGNED_TX_XDR)
            .expect("bug: canonical hash vector should validate as XDR");

        assert_eq!(*tx.block_expiry_height, 1_000_000);
    }

    #[test]
    fn computes_canonical_hash_from_inner_transaction() {
        let tx = validate_raw_transaction(CANONICAL_HASH_VECTOR_SIGNED_TX_XDR)
            .expect("bug: canonical hash vector should validate as XDR");

        assert_eq!(
            hex::encode(tx.hash.0.0.as_ref()),
            "9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3"
        );
    }

    #[test]
    fn uppercase_0x_prefix_is_accepted() {
        let tx = validate_raw_transaction(&format!("0X{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .expect("bug: uppercase 0X prefix should be accepted");

        assert_eq!(
            hex::encode(tx.hash.0.0.as_ref()),
            "9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3"
        );
    }

    #[test]
    fn trailing_bytes_after_signed_transaction_are_rejected() {
        let err = validate_raw_transaction(&format!("{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}00"))
            .expect_err("bug: trailing bytes should be rejected");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn truncated_signatures_are_rejected() {
        let truncated =
            &CANONICAL_HASH_VECTOR_SIGNED_TX_XDR[..CANONICAL_HASH_VECTOR_SIGNED_TX_XDR.len() - 8];
        let err = validate_raw_transaction(truncated)
            .expect_err("bug: truncated signatures should be rejected");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn empty_hex_input_is_rejected() {
        let err =
            validate_raw_transaction("").expect_err("bug: empty hex input should fail XDR decode");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }
}
