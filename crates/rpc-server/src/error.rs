use groundhog_rpc_api_types::error::codes;
use jsonrpsee::types::ErrorObjectOwned;

/// Errors returned by the shared JSON-RPC server layer.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum RpcServerError {
    /// Transaction was malformed.
    #[error("invalid transaction: {0}")]
    TxInvalid(String),

    /// Transaction was well-formed but rejected by the backend.
    #[error("transaction rejected: {0}")]
    TxRejected(String),

    /// Transaction hash was not found in the backend.
    #[error("transaction not found")]
    TxNotFound,

    /// Block height was not found in the backend.
    #[error("block not found")]
    BlockNotFound,

    /// Invalid method parameter value.
    #[error("invalid params: {0}")]
    InvalidParam(String),

    /// Requested height range is too large.
    #[error("range too large: {0}")]
    RangeTooLarge(String),

    /// Backend is unavailable.
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),

    /// Backend request timed out.
    #[error("backend timeout")]
    BackendTimeout,

    /// Gateway or backend is overloaded.
    #[error("overloaded: {0}")]
    Overloaded(String),

    /// Method exists but is not implemented by this backend.
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

impl From<RpcServerError> for ErrorObjectOwned {
    fn from(err: RpcServerError) -> Self {
        let (code, msg) = match &err {
            RpcServerError::TxInvalid(_) => (codes::TX_INVALID, err.to_string()),
            RpcServerError::TxRejected(_) => (codes::TX_REJECTED, err.to_string()),
            RpcServerError::TxNotFound => (codes::TX_NOT_FOUND, err.to_string()),
            RpcServerError::BlockNotFound => (codes::BLOCK_NOT_FOUND, err.to_string()),
            RpcServerError::InvalidParam(_) => (codes::INVALID_PARAMS, err.to_string()),
            RpcServerError::RangeTooLarge(_) => (codes::RANGE_TOO_LARGE, err.to_string()),
            RpcServerError::BackendUnavailable(_) => (codes::BACKEND_UNAVAILABLE, err.to_string()),
            RpcServerError::BackendTimeout => (codes::BACKEND_TIMEOUT, err.to_string()),
            RpcServerError::Overloaded(_) => (codes::OVERLOADED, err.to_string()),
            RpcServerError::NotImplemented(_) => (codes::NOT_IMPLEMENTED, err.to_string()),
        };
        ErrorObjectOwned::owned(code, msg, None::<()>)
    }
}
