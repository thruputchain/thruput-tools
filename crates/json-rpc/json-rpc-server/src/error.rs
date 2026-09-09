use groundhog_json_rpc_types::error::{TxInvalidDetail, codes};
use jsonrpsee::types::ErrorObjectOwned;

/// Errors returned by the shared JSON-RPC server layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcServerError {
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

impl From<RpcServerError> for ErrorObjectOwned {
    fn from(err: RpcServerError) -> Self {
        if let RpcServerError::TxInvalidDetailed(detail) = &err {
            return ErrorObjectOwned::owned(codes::TX_INVALID, err.to_string(), Some(detail));
        }
        let (code, msg) = match &err {
            RpcServerError::TxInvalid(_) | RpcServerError::TxInvalidDetailed(_) => {
                (codes::TX_INVALID, err.to_string())
            }
            RpcServerError::TxRejected(_) => (codes::TX_REJECTED, err.to_string()),
            RpcServerError::TxNotFound => (codes::TX_NOT_FOUND, err.to_string()),
            RpcServerError::BlockNotFound => (codes::BLOCK_NOT_FOUND, err.to_string()),
            RpcServerError::InvalidParam(_) => (codes::INVALID_PARAMS, err.to_string()),
            RpcServerError::BackendUnavailable(_) => (codes::BACKEND_UNAVAILABLE, err.to_string()),
            RpcServerError::BackendTimeout => (codes::BACKEND_TIMEOUT, err.to_string()),
            RpcServerError::Overloaded(_) => (codes::OVERLOADED, err.to_string()),
            RpcServerError::NotImplemented(_) => (codes::NOT_IMPLEMENTED, err.to_string()),
        };
        ErrorObjectOwned::owned(code, msg, None::<()>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_maps_to_its_documented_code() {
        let cases = [
            (RpcServerError::TxInvalid("x".into()), codes::TX_INVALID),
            (
                RpcServerError::TxInvalidDetailed(TxInvalidDetail::TooManyContracts {
                    attached: 5,
                    max: 4,
                }),
                codes::TX_INVALID,
            ),
            (RpcServerError::TxRejected("x".into()), codes::TX_REJECTED),
            (RpcServerError::TxNotFound, codes::TX_NOT_FOUND),
            (RpcServerError::BlockNotFound, codes::BLOCK_NOT_FOUND),
            (
                RpcServerError::InvalidParam("x".into()),
                codes::INVALID_PARAMS,
            ),
            (
                RpcServerError::BackendUnavailable("x".into()),
                codes::BACKEND_UNAVAILABLE,
            ),
            (RpcServerError::BackendTimeout, codes::BACKEND_TIMEOUT),
            (RpcServerError::Overloaded("x".into()), codes::OVERLOADED),
            (
                RpcServerError::NotImplemented("x".into()),
                codes::NOT_IMPLEMENTED,
            ),
        ];

        for (err, expected_code) in cases {
            let label = err.to_string();
            let obj: ErrorObjectOwned = err.into();
            assert_eq!(obj.code(), expected_code, "wrong code for `{label}`");
        }
    }

    #[test]
    fn detailed_tx_invalid_carries_structured_data() {
        let err = RpcServerError::TxInvalidDetailed(TxInvalidDetail::ContractTooLarge {
            index: 1,
            size_bytes: 300_000,
            max_bytes: 262_144,
        });

        let obj: ErrorObjectOwned = err.into();

        assert_eq!(obj.code(), codes::TX_INVALID);
        assert_eq!(
            obj.message(),
            "invalid transaction: contract 1 in contractsToDeploy is 300000 bytes; \
             limit is 262144"
        );
        let data: serde_json::Value =
            serde_json::from_str(obj.data().expect("detail should be attached").get()).unwrap();
        assert_eq!(
            data,
            serde_json::json!({
                "reason": "contractTooLarge",
                "index": 1,
                "sizeBytes": 300_000,
                "maxBytes": 262_144,
            })
        );
    }

    #[test]
    fn plain_tx_invalid_carries_no_data() {
        let obj: ErrorObjectOwned = RpcServerError::TxInvalid("invalid hex".into()).into();

        assert_eq!(obj.code(), codes::TX_INVALID);
        assert!(obj.data().is_none());
    }
}
