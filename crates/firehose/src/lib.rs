//! Experimental, procedure-10-only committed block archive.
pub mod api;
pub mod client;
pub mod metrics;
pub mod store;
pub mod subscription;
pub mod types;
mod verify;

/// Failures that stop ingestion rather than silently advancing the cursor.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A blocking archive worker failed to complete.
    #[error("archive worker: {0}")]
    Worker(#[from] tokio::task::JoinError),
    /// Local archive I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A record cannot be encoded or decoded.
    #[error("XDR: {0}")]
    Xdr(String),
    /// The archive or incoming block violates a commitment or continuity rule.
    #[error("archive validation: {0}")]
    Invalid(String),
    /// The RPC transport failed.
    #[error(transparent)]
    Rpc(#[from] rpc_driver::RpcError),
    /// Connection setup failed.
    #[error(transparent)]
    Connect(#[from] groundhog_x23_transport::ConnectError),
}

fn encode<T: xdrs::WriteXDR>(value: &T) -> Result<Vec<u8>, Error> {
    xdrs::txs::encode(value).map_err(|e| Error::Xdr(e.to_string()))
}

fn decode<'a, T: xdrs::ReadXDR<'a>>(bytes: &'a [u8]) -> Result<T, Error> {
    xdrs::txs::decode(bytes).map_err(|e| Error::Xdr(e.to_string()))
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.to_owned())
}

/// The height every module keys records by, six levels inside the pulled record.
fn block_height(pulled: &groundhog_indexer::EventIndexerPulledBlock) -> i64 {
    *pulled
        .block
        .payload
        .consensus_value
        .proposal_header
        .block_height
}
