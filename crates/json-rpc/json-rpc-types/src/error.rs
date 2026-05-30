/// Groundhog-specific JSON-RPC error codes.
/// Standard JSON-RPC reserves -32000 to -32099 for server errors.
/// We follow Substrate's pattern of per-module ranges.
///
/// -32000 to -32009: Transaction errors
/// -32010 to -32019: Query errors
/// -32020 to -32029: Subscription errors (Phase 2)
pub mod codes {
    /// Standard JSON-RPC: invalid method parameter(s).
    pub const INVALID_PARAMS: i32 = -32602;

    /// Transaction was malformed (bad XDR, missing fields).
    pub const TX_INVALID: i32 = -32000;

    /// Transaction was well-formed but rejected by the scheduler.
    pub const TX_REJECTED: i32 = -32001;

    /// Transaction hash not found in mempool or indexer.
    pub const TX_NOT_FOUND: i32 = -32002;

    /// Block height out of range (negative, or beyond current tip).
    pub const BLOCK_NOT_FOUND: i32 = -32010;

    /// Backend (hogd indexer or scheduler) unavailable.
    pub const BACKEND_UNAVAILABLE: i32 = -32050;

    /// Request timed out waiting for backend response.
    pub const BACKEND_TIMEOUT: i32 = -32051;

    /// Method exists but is not yet implemented in this gateway version.
    pub const NOT_IMPLEMENTED: i32 = -32052;

    /// Gateway or backend is overloaded; clients should retry with backoff.
    ///
    /// Distinct from `BACKEND_UNAVAILABLE` (connection gone) and
    /// `BACKEND_TIMEOUT` (ambiguous outcome). `OVERLOADED` always means
    /// "the request never reached the backend, retrying is safe."
    pub const OVERLOADED: i32 = -32053;

    /// All documented Groundhog JSON-RPC error codes.
    pub const ALL: &[i32] = &[
        INVALID_PARAMS,
        TX_INVALID,
        TX_REJECTED,
        TX_NOT_FOUND,
        BLOCK_NOT_FOUND,
        BACKEND_UNAVAILABLE,
        BACKEND_TIMEOUT,
        NOT_IMPLEMENTED,
        OVERLOADED,
    ];
}

/// Static documentation for one JSON-RPC error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcErrorInfo {
    /// Stable component key used in generated `OpenRPC` artifacts.
    pub name: &'static str,
    /// JSON-RPC error code.
    pub code: i32,
    /// Short message describing the error class.
    pub message: &'static str,
    /// Longer consumer-facing explanation.
    pub description: &'static str,
}

/// Public JSON-RPC error codes known to the Groundhog API surface.
pub const ERROR_INFOS: &[RpcErrorInfo] = &[
    RpcErrorInfo {
        name: "InvalidParams",
        code: codes::INVALID_PARAMS,
        message: "Invalid params",
        description: concat!(
            "One or more JSON-RPC parameters are malformed, out of range, ",
            "or otherwise invalid."
        ),
    },
    RpcErrorInfo {
        name: "TxInvalid",
        code: codes::TX_INVALID,
        message: "Invalid transaction",
        description: concat!(
            "The submitted transaction is malformed, has invalid XDR, or ",
            "fails local validation before backend submission."
        ),
    },
    RpcErrorInfo {
        name: "TxRejected",
        code: codes::TX_REJECTED,
        message: "Transaction rejected",
        description: concat!(
            "The submitted transaction is well-formed but was rejected by ",
            "the scheduler or execution backend."
        ),
    },
    RpcErrorInfo {
        name: "TxNotFound",
        code: codes::TX_NOT_FOUND,
        message: "Transaction not found",
        description: concat!(
            "The requested transaction hash was not found in the mempool ",
            "or committed index."
        ),
    },
    RpcErrorInfo {
        name: "BlockNotFound",
        code: codes::BLOCK_NOT_FOUND,
        message: "Block not found",
        description: concat!(
            "The requested block height is out of range or beyond the ",
            "current indexed chain tip."
        ),
    },
    RpcErrorInfo {
        name: "BackendUnavailable",
        code: codes::BACKEND_UNAVAILABLE,
        message: "Backend unavailable",
        description: concat!(
            "The gateway could not reach a required backend such as hogd, ",
            "the scheduler, or the indexer."
        ),
    },
    RpcErrorInfo {
        name: "BackendTimeout",
        code: codes::BACKEND_TIMEOUT,
        message: "Backend timeout",
        description: concat!(
            "The request timed out while waiting for a backend response; ",
            "the backend outcome may be ambiguous."
        ),
    },
    RpcErrorInfo {
        name: "NotImplemented",
        code: codes::NOT_IMPLEMENTED,
        message: "Not implemented",
        description: concat!(
            "The JSON-RPC method exists but is not implemented by this ",
            "gateway or backend capability shape."
        ),
    },
    RpcErrorInfo {
        name: "Overloaded",
        code: codes::OVERLOADED,
        message: "Overloaded",
        description: concat!(
            "The gateway or backend is overloaded and rejected the request ",
            "before it reached side-effecting backend execution."
        ),
    },
];

/// Look up documented error metadata by JSON-RPC code.
pub fn error_info_by_code(code: i32) -> Option<&'static RpcErrorInfo> {
    ERROR_INFOS.iter().find(|info| info.code == code)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn documented_error_codes_are_unique_and_lookupable() {
        let mut codes = HashSet::new();
        let mut names = HashSet::new();

        for info in ERROR_INFOS {
            assert!(codes.insert(info.code), "duplicate code {}", info.code);
            assert!(names.insert(info.name), "duplicate name {}", info.name);
            assert_eq!(error_info_by_code(info.code), Some(info));
            assert!(!info.message.is_empty());
            assert!(!info.description.is_empty());
        }

        let documented_codes: HashSet<i32> = ERROR_INFOS.iter().map(|info| info.code).collect();
        let public_codes: HashSet<i32> = self::codes::ALL.iter().copied().collect();
        assert_eq!(
            documented_codes, public_codes,
            "codes::ALL and ERROR_INFOS must stay one-to-one"
        );
    }
}
