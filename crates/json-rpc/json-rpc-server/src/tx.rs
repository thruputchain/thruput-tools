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
    fn empty_hex_input_is_rejected() {
        let err =
            validate_raw_transaction("").expect_err("bug: empty hex input should fail XDR decode");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }
}
