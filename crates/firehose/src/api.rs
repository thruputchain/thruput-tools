//! Experimental standalone subscription surface, not registered by the gateway.
use crate::types::{Cursor, Notification};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};

/// Verifiable committed records with inclusive resume and explicit consumption ACKs.
#[rpc(client, server, namespace = "firehose")]
pub trait FirehoseApi {
    /// Follow from an inclusive height or the next publication at the tip.
    #[subscription(name = "subscribe" => "block", unsubscribe = "unsubscribe", item = Notification)]
    async fn follow_blocks(&self, cursor: Cursor);

    /// Acknowledge the outstanding height on the same WebSocket connection.
    #[method(name = "ack", with_extensions)]
    fn acknowledge(&self, subscriber_id: u64, height: i64) -> RpcResult<()>;

    /// Process totals and active subscriber diagnostics; experimental schema.
    #[method(name = "stats")]
    fn statistics(&self) -> RpcResult<serde_json::Value>;
}
