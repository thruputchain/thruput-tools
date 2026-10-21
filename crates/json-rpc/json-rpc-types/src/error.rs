/// Native RPC error contracts.
pub mod native;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

    /// Local funding has retained signed evidence but no resolved outcome.
    pub const LOCAL_FUNDING_UNRESOLVED: i32 = -32003;

    /// Block height out of range (negative, or beyond current tip).
    pub const BLOCK_NOT_FOUND: i32 = -32010;

    /// The committed snapshot remains below the requested minimum height.
    pub const VIEW_HEIGHT_UNAVAILABLE: i32 = -32011;

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
        LOCAL_FUNDING_UNRESOLVED,
        BLOCK_NOT_FOUND,
        VIEW_HEIGHT_UNAVAILABLE,
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
        name: "LocalFundingUnresolved",
        code: codes::LOCAL_FUNDING_UNRESOLVED,
        message: "Local funding unresolved",
        description: "Local funding retained a signed transaction but could not resolve its outcome. Data contains its transaction hash and exact signed bytes, not proof of admission or execution. Reconcile that hash and retry the original request; do not create a replacement transfer.",
    },
    RpcErrorInfo {
        name: "ViewHeightUnavailable",
        code: codes::VIEW_HEIGHT_UNAVAILABLE,
        message: "View height unavailable",
        description: "The selected committed snapshot is below minHeight. Data contains requested and observed heights. The request does not wait or force a snapshot refresh.",
    },
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
            "fails local validation before backend submission. When the ",
            "rejection comes from a pre-submission bound check, `data` ",
            "carries a `TxInvalidDetail` object whose `reason` names the ",
            "violated bound."
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

/// Machine-readable detail carried in the `data` field of a `TxInvalid`
/// error raised by pre-submission bound checks.
///
/// `reason` discriminates the variant; the remaining fields are the
/// numbers a client needs to repair the transaction, which the error
/// `message` restates in prose. A `TxInvalid` error without `data` had no
/// structured detail to offer (for example, undecodable XDR).
///
/// These checks are advisory: they mirror what the executor rejects
/// deterministically before running a transaction, so a rejection here
/// saves a round trip, but passing them does not guarantee acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[serde(tag = "reason", rename_all = "camelCase")]
#[non_exhaustive]
pub enum TxInvalidDetail {
    /// `contractsToDeploy` attaches more contracts than the chain permits
    /// per transaction.
    #[error("transaction attaches {attached} contracts; chain permits {max}")]
    TooManyContracts {
        /// Contracts attached to the transaction.
        attached: u32,
        /// Per-transaction cap in force at the chain tip.
        max: u32,
    },

    /// One attached contract's code exceeds the per-contract size cap.
    #[error("contract {index} in contractsToDeploy is {size_bytes} bytes; limit is {max_bytes}")]
    #[serde(rename_all = "camelCase")]
    ContractTooLarge {
        /// Zero-based position in `contractsToDeploy`.
        index: u32,
        /// Code size of the offending contract.
        size_bytes: u32,
        /// Per-contract cap.
        max_bytes: u32,
    },

    /// The same contract appears twice in `contractsToDeploy`.
    #[error("contract {index} in contractsToDeploy duplicates contract {first_index}")]
    #[serde(rename_all = "camelCase")]
    DuplicateContract {
        /// Zero-based position of the repeated attachment.
        index: u32,
        /// Zero-based position of its first occurrence.
        first_index: u32,
    },

    /// One attached RV32 artifact declares more guest data memory than the chain admits.
    #[error(
        "contract {index} in contractsToDeploy declares {memory_bytes} bytes of guest memory; \
         limit is {max_bytes}"
    )]
    #[serde(rename_all = "camelCase")]
    Rv32MemoryTooLarge {
        /// Zero-based position in `contractsToDeploy`.
        index: u32,
        /// `memory_bytes` from the artifact header.
        memory_bytes: u32,
        /// Guest memory cap in force at the chain tip.
        max_bytes: u32,
    },

    /// One attached RV32 artifact's code section exceeds the chain's code limit.
    #[error(
        "contract {index} in contractsToDeploy carries {code_bytes} bytes of code; limit is \
         {max_bytes}"
    )]
    #[serde(rename_all = "camelCase")]
    Rv32CodeTooLarge {
        /// Zero-based position in `contractsToDeploy`.
        index: u32,
        /// `code_size` from the artifact header.
        code_bytes: u32,
        /// Code cap in force at the chain tip.
        max_bytes: u32,
    },
}

/// Bounded Prometheus outcome label for a JSON-RPC error code.
///
/// This intentionally accepts any `i32`, because gateway RPC middleware also
/// observes jsonrpsee protocol errors outside Groundhog's application catalog.
pub const fn outcome_for_code(code: i32) -> &'static str {
    match code {
        codes::INVALID_PARAMS | -32700 | -32600 | -32005 | -32007 => "invalid_params",
        codes::TX_INVALID => "tx_invalid",
        codes::TX_REJECTED => "tx_rejected",
        codes::TX_NOT_FOUND | codes::BLOCK_NOT_FOUND => "not_found",
        codes::BACKEND_UNAVAILABLE => "backend_unavailable",
        codes::BACKEND_TIMEOUT => "backend_timeout",
        codes::VIEW_HEIGHT_UNAVAILABLE => "view_height_unavailable",
        codes::NOT_IMPLEMENTED | -32601 => "not_implemented",
        codes::OVERLOADED | -32006 | -32009 => "overloaded",
        _ => "internal_error",
    }
}
