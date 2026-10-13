//! Experimental standalone firehose wire types; independent of gateway methods.
use serde::{Deserialize, Serialize};
use thruput_json_rpc_types::hex::HexBytes;

/// Inclusive archive height, or the first publication after subscription registration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "camelCase")]
#[non_exhaustive]
pub enum Cursor {
    /// Replay this height, then every subsequent height.
    Height { height: i64 },
    /// Follow new publications only.
    Tip,
}

/// Original archive bytes with enough metadata to route without decoding XDR.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    /// Block height in the immutable archive.
    pub height: i64,
    /// Consensus commitment, in canonical hexadecimal byte encoding.
    pub consensus_hash: HexBytes,
    /// Raw XDR length before hexadecimal transport encoding.
    pub byte_length: usize,
    /// Exact stored `EventIndexerPulledBlock` bytes, not a JSON projection.
    pub record: HexBytes,
    /// Publication time after durable append, in Unix microseconds; absent for archive reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingested_at_unix_micros: Option<u64>,
}

/// A block awaits acknowledgement; a close notification ends this subscription.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[non_exhaustive]
pub enum Notification<T = Record> {
    /// A single outstanding block; acknowledge only after processing it durably.
    #[serde(rename_all = "camelCase")]
    Block { subscriber_id: u64, block: T },
    /// Terminal application close reason, with the inclusive resumable height.
    #[serde(rename_all = "camelCase")]
    Closed { reason: String, next_height: i64 },
}
