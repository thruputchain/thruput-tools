//! Transaction-hash conversion for hogd receipt and status lookups.

use thruput_json_rpc_types::transaction::TxHash;
use thruput_xdr::transaction::TransactionHash;

/// Convert the RPC-layer transaction hash to its wire form.
pub fn tx_hash_to_wire(tx_hash: TxHash) -> TransactionHash {
    TransactionHash::from(tx_hash.0.0.into_inner())
}
