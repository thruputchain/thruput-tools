//! Gateway error types — maps internal errors to JSON-RPC `ErrorObject`.
//! See json-rpc-types/src/error.rs for error code constants.

use groundhog_json_rpc_types::error::codes;
use jsonrpsee::types::ErrorObjectOwned;

/// Top-level gateway error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GatewayError {
    /// Transaction was malformed (bad XDR, missing fields).
    #[error("invalid transaction: {0}")]
    TxInvalid(String),

    /// Transaction was well-formed but rejected by the scheduler.
    #[error("transaction rejected: {0}")]
    TxRejected(String),

    /// Transaction hash not found in mempool or indexer.
    #[error("transaction not found")]
    TxNotFound,

    /// Block height out of range.
    #[error("block not found")]
    BlockNotFound,

    /// Invalid parameter value (malformed input, inverted ranges, etc.).
    #[error("invalid params: {0}")]
    InvalidParam(String),

    /// Backend (hogd indexer or scheduler) unavailable.
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),

    /// Request timed out waiting for backend response.
    ///
    /// The call left the gateway; outcome at the backend is ambiguous.
    /// For the idempotent submit path this is still retry-safe (the
    /// mempool dedupes by tx hash), but clients may prefer to poll
    /// `getTransactionStatus` first.
    #[error("backend timeout")]
    BackendTimeout,

    /// Gateway or backend is overloaded.  The gateway did not admit this
    /// request far enough for it to have any side effect, so retrying
    /// with backoff is always safe.
    #[error("overloaded: {0}")]
    Overloaded(String),

    /// Method exists but is not yet implemented in this gateway version.
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

/// Return a "method not implemented" JSON-RPC error.
pub fn not_implemented(method: &str) -> GatewayError {
    GatewayError::NotImplemented(method.to_string())
}

impl From<GatewayError> for ErrorObjectOwned {
    fn from(err: GatewayError) -> Self {
        let (code, msg) = match &err {
            GatewayError::TxInvalid(_) => (codes::TX_INVALID, err.to_string()),
            GatewayError::TxRejected(_) => (codes::TX_REJECTED, err.to_string()),
            GatewayError::TxNotFound => (codes::TX_NOT_FOUND, err.to_string()),
            GatewayError::BlockNotFound => (codes::BLOCK_NOT_FOUND, err.to_string()),
            GatewayError::InvalidParam(_) => (codes::INVALID_PARAMS, err.to_string()),
            GatewayError::BackendUnavailable(_) => (codes::BACKEND_UNAVAILABLE, err.to_string()),
            GatewayError::BackendTimeout => (codes::BACKEND_TIMEOUT, err.to_string()),
            GatewayError::Overloaded(_) => (codes::OVERLOADED, err.to_string()),
            GatewayError::NotImplemented(_) => (codes::NOT_IMPLEMENTED, err.to_string()),
        };
        ErrorObjectOwned::owned(code, msg, None::<()>)
    }
}
