use super::{
    NativeReadError, Reason, TxHash, TxInfoResult, identity, indexed_status, invalid, out,
    tx_hash_to_wire,
};
use sha2::{Digest, Sha256};
use thruput_json_rpc_types::hex::HexBytes;

/// Return the retained Transaction body, the exact preimage of its public hash.
pub fn input_lookup(
    hash: TxHash,
    result: TxInfoResult,
) -> Result<out::TransactionInputResult, NativeReadError> {
    let TxInfoResult::TxInfoFound { info } = result else {
        indexed_status(hash, result)?;
        return Ok(out::TransactionInputResult::Unavailable {});
    };
    identity(hash, &info.height, &info.events)?;
    let bytes = xdrs::txs::encode(&info.tx.signed_tx.tx).map_err(|_| {
        invalid(
            Reason::InvalidEncoding,
            "cannot encode retained Transaction XDR",
        )
    })?;
    if bytes.len() > out::MAX_TRANSACTION_INPUT_BYTES {
        return Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
            "retained transaction exceeds input response limit".into(),
        )
        .into());
    }
    if Sha256::digest(&bytes).as_slice() != tx_hash_to_wire(hash).as_ref() {
        return Err(invalid(
            Reason::InconsistentResult,
            "retained transaction body hash mismatch",
        )
        .into());
    }
    Ok(out::TransactionInputResult::Available {
        transaction_hash: hash,
        block_height: info.height,
        transaction_xdr: HexBytes(bytes),
    })
}
