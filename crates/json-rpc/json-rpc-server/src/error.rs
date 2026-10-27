use thruput_json_rpc_types::airdrop::LocalFundingErrorData;
use thruput_json_rpc_types::error::{TxInvalidDetail, codes};

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
        /// Typed failure retained before wrapping durable signed evidence.
        cause: Option<Box<RpcServerError>>,
    },
    /// The selected committed snapshot did not meet the freshness floor.
    #[error("view height unavailable: requested {}, observed {}", *.0.requested, *.0.observed)]
    ViewHeightUnavailable(thruput_json_rpc_types::view::ViewHeightUnavailable),
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

    /// Invalid method parameter value.
    #[error("invalid params: {0}")]
    InvalidParam(String),

    /// Backend-owned cursor validation failed without a finer classification.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// The backend cannot serve the historical range required by the request.
    #[error("history unavailable: {0}")]
    HistoryUnavailable(String),

    /// Backend is unavailable.
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),

    /// Backend data cannot satisfy its declared contract. Legacy callers retain
    /// the unavailable wire error; native read handlers preserve the reason.
    #[error("backend unavailable: {message}")]
    BackendDataInvalid {
        /// Stable classification for native read failures.
        reason: thruput_json_rpc_types::error::native::BackendDataInvalidReason,
        /// Diagnostic context; not a machine-readable discriminator.
        message: String,
    },

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
    /// Preserve a decoder failure without changing the legacy wire error.
    pub fn invalid_data(
        reason: thruput_json_rpc_types::error::native::BackendDataInvalidReason,
        message: impl Into<String>,
    ) -> Self {
        Self::BackendDataInvalid {
            reason,
            message: message.into(),
        }
    }

    /// The JSON-RPC error code this error is reported under.
    pub fn code(&self) -> i32 {
        match self {
            Self::LocalFundingUnresolved { .. } => codes::LOCAL_FUNDING_UNRESOLVED,
            Self::ViewHeightUnavailable(_) => codes::VIEW_HEIGHT_UNAVAILABLE,
            Self::TxInvalid(_) | Self::TxInvalidDetailed(_) => codes::TX_INVALID,
            Self::TxRejected(_) => codes::TX_REJECTED,
            Self::InvalidParam(_) => codes::INVALID_PARAMS,
            Self::BackendUnavailable(_)
            | Self::BackendDataInvalid { .. }
            | Self::InvalidCursor(_)
            | Self::HistoryUnavailable(_) => codes::BACKEND_UNAVAILABLE,
            Self::BackendTimeout => codes::BACKEND_TIMEOUT,
            Self::Overloaded(_) => codes::OVERLOADED,
            Self::NotImplemented(_) => codes::NOT_IMPLEMENTED,
        }
    }
}
