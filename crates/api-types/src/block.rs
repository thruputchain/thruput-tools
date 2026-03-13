use serde::{Deserialize, Serialize};

use crate::{BlockHeader, BlockHeight, SHA256Hash};

/// Current chain tip. Returned by chainTip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainTip {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
}

/// Block data returned by getBlockByNumber.
///
/// Wraps the XDR `BlockHeader` so the RPC layer can add fields
/// (e.g. tx list, metadata) without modifying the wire type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockInfo {
    /// Block header from the consensus layer.
    #[serde(flatten)]
    pub header: BlockHeader,
}
