use groundhog_json_rpc_types::airdrop::LocalFundingErrorData;
use groundhog_json_rpc_types::error::{TxInvalidDetail, codes};
use jsonrpsee::types::ErrorObjectOwned;

/// Errors returned by the shared JSON-RPC server layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcServerError {
    /// Retained local funding evidence whose outcome still needs reconciliation.
    #[error("local funding unresolved: {reason}")]
    LocalFundingUnresolved {
        /// Failure encountered while observing or submitting the retained transaction.
        reason: String,
        /// Signed evidence, independent of admission or execution status.
        evidence: LocalFundingErrorData,
    },
    /// No sufficiently recent committed snapshot was available after one refresh.
    #[error("view height unavailable: requested {}, observed {}", *.0.requested, *.0.observed)]
    ViewHeightUnavailable(groundhog_json_rpc_types::view::ViewHeightUnavailable),
    /// Transaction was malformed.
    #[error("invalid transaction: {0}")]
    TxInvalid(String),

    /// Transaction decoded but breaks a bound the executor checks before
    /// running it. Maps to the `TxInvalid` code with the detail as
    /// structured error `data`.
    #[error("invalid transaction: {0}")]
    TxInvalidDetailed(TxInvalidDetail),

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

impl RpcServerError {
    /// The JSON-RPC error code this error is reported under.
    pub fn code(&self) -> i32 {
        match self {
            Self::LocalFundingUnresolved { .. } => codes::LOCAL_FUNDING_UNRESOLVED,
            Self::ViewHeightUnavailable(_) => codes::VIEW_HEIGHT_UNAVAILABLE,
            Self::TxInvalid(_) | Self::TxInvalidDetailed(_) => codes::TX_INVALID,
            Self::TxRejected(_) => codes::TX_REJECTED,
            Self::TxNotFound => codes::TX_NOT_FOUND,
            Self::BlockNotFound => codes::BLOCK_NOT_FOUND,
            Self::InvalidParam(_) => codes::INVALID_PARAMS,
            Self::BackendUnavailable(_) => codes::BACKEND_UNAVAILABLE,
            Self::BackendTimeout => codes::BACKEND_TIMEOUT,
            Self::Overloaded(_) => codes::OVERLOADED,
            Self::NotImplemented(_) => codes::NOT_IMPLEMENTED,
        }
    }
}

impl From<RpcServerError> for ErrorObjectOwned {
    fn from(err: RpcServerError) -> Self {
        let code = err.code();
        if let RpcServerError::LocalFundingUnresolved { evidence, .. } = &err {
            return ErrorObjectOwned::owned(code, err.to_string(), Some(evidence));
        }
        if let RpcServerError::ViewHeightUnavailable(detail) = &err {
            return ErrorObjectOwned::owned(code, err.to_string(), Some(detail));
        }
        if let RpcServerError::TxInvalidDetailed(detail) = &err {
            return ErrorObjectOwned::owned(code, err.to_string(), Some(detail));
        }
        ErrorObjectOwned::owned(code, err.to_string(), None::<()>)
    }
}
