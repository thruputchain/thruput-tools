use serde::{Deserialize, Serialize};

use crate::transaction::TxHash;
use crate::{BlockHeight, SHA256Hash};

/// Current chain tip. Returned by chainTip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainTip {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
}

/// Block summary. Returned by getBlockByNumber.
///
/// Intentionally minimal for Phase 1. Doesn't include full
/// transaction list (that's a separate query if needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockInfo {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
    pub parent_hash: SHA256Hash,
    pub tx_count: u32,
    pub tx_hashes: Vec<TxHash>,
    // TODO: timestamp? CV bytes? consensus metadata?
    // Keep minimal until we know what block explorers need.
}
