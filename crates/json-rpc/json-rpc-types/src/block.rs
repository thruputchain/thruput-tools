use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hex::Hex0x;
use crate::{BlockHash, BlockHeader, BlockHeight, DbHash, SHA256Hash};

/// Current chain tip. Returned by chainTip.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainTip {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
}

/// Block data returned by getBlockByNumber.
///
/// Maps the XDR `BlockHeader` to RPC-friendly serialization
/// (camelCase fields, `0x`-prefixed hex). Add RPC-specific fields
/// here without modifying the wire type.
///
/// The `From<BlockHeader>` impl uses exhaustive destructuring so
/// adding a field to `BlockHeader` causes a compile error here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockInfo {
    pub network_id: u64,
    pub block_height: BlockHeight,
    pub txset_hash: SHA256Hash,
    pub db_hash: DbHash,
    pub events_hash: SHA256Hash,
    pub prev_block_hash: BlockHash,
}

impl From<BlockHeader> for BlockInfo {
    fn from(
        BlockHeader {
            network_id,
            block_height,
            txset_hash,
            db_hash,
            events_hash,
            prev_block_hash,
        }: BlockHeader,
    ) -> Self {
        Self {
            network_id,
            block_height,
            txset_hash: Hex0x(txset_hash),
            db_hash: Hex0x(db_hash),
            events_hash: Hex0x(events_hash),
            prev_block_hash: Hex0x(prev_block_hash.into_inner()),
        }
    }
}
