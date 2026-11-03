use crate::error::RpcErrorInfo;

/// Native allocations. Framework size errors retain their own codes.
pub mod codes {
    /// Invalid named request or continuation token.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Invalid transaction framing, envelope structure or executor bounds.
    pub const TRANSACTION_INVALID: i32 = -32000;
    /// Local expiry or explicit scheduler authentication refusal for this attempt.
    pub const TRANSACTION_REJECTED: i32 = -32001;
    /// The selected simulation snapshot is below the requested floor.
    pub const STATE_HEIGHT_UNAVAILABLE: i32 = -32012;
    /// The backend proved that required historical data cannot be served.
    pub const HISTORY_UNAVAILABLE: i32 = -32013;
    /// The funding service stopped this attempt; this does not prove nonexecution.
    pub const FUNDING_ATTEMPT_STOPPED: i32 = -32030;
    /// Retained signed funding evidence without a resolved outcome.
    pub const LOCAL_FUNDING_UNRESOLVED: i32 = -32031;
    /// A required backend or dependency is unavailable; admission is separate.
    pub const BACKEND_UNAVAILABLE: i32 = -32050;
    /// The operation exceeded its deadline; writes may have been dispatched.
    pub const BACKEND_TIMEOUT: i32 = -32051;
    /// Gateway or backend refusal, including scheduler capacity or not-ready state.
    pub const OVERLOADED: i32 = -32053;
    /// The client exceeded its request budget before reaching a backend.
    pub const RATE_LIMITED: i32 = -32055;
    /// The backend data cannot satisfy the method contract.
    pub const BACKEND_DATA_INVALID: i32 = -32054;
}

/// One component per numeric code; `InvalidParams` has two data refinements.
pub const ERROR_INFOS: &[RpcErrorInfo] = &[
    RpcErrorInfo {
        name: "InvalidParams",
        code: codes::INVALID_PARAMS,
        message: "Invalid params",
        description: "Invalid named request or continuation token.",
    },
    RpcErrorInfo {
        name: "TransactionInvalid",
        code: codes::TRANSACTION_INVALID,
        message: "Invalid transaction",
        description: "Invalid transaction framing, envelope structure or executor bounds.",
    },
    RpcErrorInfo {
        name: "TransactionRejected",
        code: codes::TRANSACTION_REJECTED,
        message: "Transaction rejected",
        description: "Local expiry or explicit scheduler authentication refusal for this attempt.",
    },
    RpcErrorInfo {
        name: "StateHeightUnavailable",
        code: codes::STATE_HEIGHT_UNAVAILABLE,
        message: "Requested state height unavailable",
        description: "The selected simulation snapshot is below the requested floor.",
    },
    RpcErrorInfo {
        name: "HistoryUnavailable",
        code: codes::HISTORY_UNAVAILABLE,
        message: "History unavailable",
        description: "The backend proved that required historical data cannot be served.",
    },
    RpcErrorInfo {
        name: "FundingAttemptStopped",
        code: codes::FUNDING_ATTEMPT_STOPPED,
        message: "Funding attempt stopped",
        description: "The funding service stopped this attempt; this does not prove nonexecution.",
    },
    RpcErrorInfo {
        name: "LocalFundingUnresolved",
        code: codes::LOCAL_FUNDING_UNRESOLVED,
        message: "Local funding unresolved",
        description: "Retained signed funding evidence without a resolved outcome.",
    },
    RpcErrorInfo {
        name: "BackendUnavailable",
        code: codes::BACKEND_UNAVAILABLE,
        message: "Backend unavailable",
        description: "A required backend or dependency is unavailable; admission is separate.",
    },
    RpcErrorInfo {
        name: "BackendTimeout",
        code: codes::BACKEND_TIMEOUT,
        message: "Backend timeout",
        description: "The operation exceeded its deadline; writes may have been dispatched.",
    },
    RpcErrorInfo {
        name: "Overloaded",
        code: codes::OVERLOADED,
        message: "Temporary refusal",
        description: "Gateway or backend refusal, including scheduler capacity or not-ready state.",
    },
    RpcErrorInfo {
        name: "RateLimited",
        code: codes::RATE_LIMITED,
        message: "Rate limited",
        description: "The client exceeded its request budget. Retry after data.retryAfterMs milliseconds. The request never reached a backend.",
    },
    RpcErrorInfo {
        name: "BackendDataInvalid",
        code: codes::BACKEND_DATA_INVALID,
        message: "Invalid backend data",
        description: "The backend data cannot satisfy the method contract.",
    },
];

pub fn error_info_by_code(code: i32) -> Option<&'static RpcErrorInfo> {
    ERROR_INFOS.iter().find(|info| info.code == code)
}

/// Core methods plus the three selected optional methods. This does not
/// register or advertise any method on a running server.
pub const METHODS: &[&str] = &[
    "getVersionInfo",
    "getNetworkInfo",
    "getChainTip",
    "getChainConfig",
    "getServerLimits",
    "sendTransaction",
    "getTransactionStatus",
    "getTransactionReceipt",
    "simulateInvocation",
    "simulateInvocations",
    "getStorage",
    "getBalance",
    "getRegisteredAuth",
    "getEvents",
    "getTransactionsByRootAddress",
    "getBlockByHeight",
    "getTransactionsByBlock",
    "faucet_requestAirdrop",
    "dev_fundAccount",
    "dev_getPendingTransactionCount",
];

/// Declared application codes. Standard dispatcher/transport errors may occur
/// independently, including on otherwise infallible discovery methods.
pub fn method_error_codes(method: &str) -> Option<&'static [i32]> {
    use codes::{
        BACKEND_DATA_INVALID, BACKEND_TIMEOUT, BACKEND_UNAVAILABLE, FUNDING_ATTEMPT_STOPPED,
        HISTORY_UNAVAILABLE, INVALID_PARAMS, LOCAL_FUNDING_UNRESOLVED, OVERLOADED, RATE_LIMITED,
        STATE_HEIGHT_UNAVAILABLE, TRANSACTION_INVALID, TRANSACTION_REJECTED,
    };
    Some(match method {
        "getVersionInfo" | "getNetworkInfo" | "getServerLimits" => &[INVALID_PARAMS, OVERLOADED],
        "sendTransaction" => &[
            INVALID_PARAMS,
            TRANSACTION_INVALID,
            TRANSACTION_REJECTED,
            BACKEND_UNAVAILABLE,
            BACKEND_TIMEOUT,
            OVERLOADED,
            RATE_LIMITED,
            BACKEND_DATA_INVALID,
        ],
        "simulateInvocation" | "simulateInvocations" => &[
            INVALID_PARAMS,
            STATE_HEIGHT_UNAVAILABLE,
            BACKEND_UNAVAILABLE,
            BACKEND_TIMEOUT,
            OVERLOADED,
            RATE_LIMITED,
            BACKEND_DATA_INVALID,
        ],
        "getEvents"
        | "getTransactionsByRootAddress"
        | "getBlockByHeight"
        | "getTransactionsByBlock" => &[
            INVALID_PARAMS,
            HISTORY_UNAVAILABLE,
            BACKEND_UNAVAILABLE,
            BACKEND_TIMEOUT,
            OVERLOADED,
            RATE_LIMITED,
            BACKEND_DATA_INVALID,
        ],
        "faucet_requestAirdrop" => &[
            INVALID_PARAMS,
            FUNDING_ATTEMPT_STOPPED,
            BACKEND_UNAVAILABLE,
            BACKEND_TIMEOUT,
            OVERLOADED,
            RATE_LIMITED,
            BACKEND_DATA_INVALID,
        ],
        "dev_fundAccount" => &[
            INVALID_PARAMS,
            FUNDING_ATTEMPT_STOPPED,
            LOCAL_FUNDING_UNRESOLVED,
            BACKEND_UNAVAILABLE,
            BACKEND_TIMEOUT,
            OVERLOADED,
            RATE_LIMITED,
            BACKEND_DATA_INVALID,
        ],
        "getChainTip"
        | "getChainConfig"
        | "getTransactionStatus"
        | "getTransactionReceipt"
        | "getStorage"
        | "getBalance"
        | "getRegisteredAuth"
        | "dev_getPendingTransactionCount" => &[
            INVALID_PARAMS,
            BACKEND_UNAVAILABLE,
            BACKEND_TIMEOUT,
            OVERLOADED,
            RATE_LIMITED,
            BACKEND_DATA_INVALID,
        ],
        _ => return None,
    })
}

pub(super) fn kind_code(kind: &str) -> Option<i32> {
    use codes::{
        BACKEND_DATA_INVALID, BACKEND_TIMEOUT, BACKEND_UNAVAILABLE, FUNDING_ATTEMPT_STOPPED,
        HISTORY_UNAVAILABLE, INVALID_PARAMS, LOCAL_FUNDING_UNRESOLVED, OVERLOADED, RATE_LIMITED,
        STATE_HEIGHT_UNAVAILABLE, TRANSACTION_INVALID, TRANSACTION_REJECTED,
    };
    Some(match kind {
        "invalidParams" | "invalidCursor" => INVALID_PARAMS,
        "transactionInvalid" => TRANSACTION_INVALID,
        "transactionRejected" => TRANSACTION_REJECTED,
        "stateHeightUnavailable" => STATE_HEIGHT_UNAVAILABLE,
        "historyUnavailable" => HISTORY_UNAVAILABLE,
        "fundingAttemptStopped" => FUNDING_ATTEMPT_STOPPED,
        "localFundingUnresolved" => LOCAL_FUNDING_UNRESOLVED,
        "backendUnavailable" => BACKEND_UNAVAILABLE,
        "backendTimeout" => BACKEND_TIMEOUT,
        "overloaded" => OVERLOADED,
        "rateLimited" => RATE_LIMITED,
        "backendDataInvalid" => BACKEND_DATA_INVALID,
        _ => return None,
    })
}

/// Evidence fields have operation-specific meanings. Unknown ordinary fields
/// remain additive; reserved evidence cannot acquire meaning on another method.
pub(super) const EVIDENCE_FIELDS: &[&str] = &[
    "admission",
    "transactionHash",
    "signedTransaction",
    "observation",
    "receipt",
    "cause",
    "serviceCode",
    "error",
];

pub(super) fn allowed_evidence(method: &str, kind: &str, field: &str) -> bool {
    match field {
        "admission" | "transactionHash" if method == "sendTransaction" => matches!(
            kind,
            "transactionInvalid"
                | "transactionRejected"
                | "backendUnavailable"
                | "backendTimeout"
                | "overloaded"
                | "backendDataInvalid"
        ),
        "observation" => method == "getTransactionReceipt" && kind == "backendDataInvalid",
        "transactionHash" | "signedTransaction" => {
            matches!(kind, "fundingAttemptStopped" | "localFundingUnresolved")
        }
        "receipt" => kind == "fundingAttemptStopped",
        "cause" => kind == "localFundingUnresolved",
        "serviceCode" => {
            matches!(method, "faucet_requestAirdrop" | "dev_fundAccount")
                && matches!(kind, "fundingAttemptStopped" | "backendUnavailable")
        }
        "error" => {
            method == "sendTransaction"
                && matches!(
                    kind,
                    "transactionRejected" | "backendUnavailable" | "overloaded"
                )
        }
        _ => false,
    }
}

pub(super) fn kind_allowed(method: &str, kind: &str) -> bool {
    let Some(code) = kind_code(kind) else {
        return false;
    };
    method_error_codes(method).is_some_and(|codes| codes.contains(&code))
        && (kind != "invalidCursor"
            || matches!(
                method,
                "getEvents" | "getTransactionsByRootAddress" | "getTransactionsByBlock"
            ))
}

/// Bounded outcome labels for native application and framework errors.
pub const fn outcome_for_code(code: i32) -> &'static str {
    match code {
        codes::INVALID_PARAMS | -32700 | -32600 | -32005 | -32007 => "invalid_params",
        codes::TRANSACTION_INVALID => "tx_invalid",
        codes::TRANSACTION_REJECTED => "tx_rejected",
        codes::STATE_HEIGHT_UNAVAILABLE => "state_height_unavailable",
        codes::HISTORY_UNAVAILABLE => "history_unavailable",
        codes::FUNDING_ATTEMPT_STOPPED => "funding_attempt_stopped",
        codes::LOCAL_FUNDING_UNRESOLVED => "local_funding_unresolved",
        codes::BACKEND_UNAVAILABLE => "backend_unavailable",
        codes::BACKEND_TIMEOUT => "backend_timeout",
        codes::OVERLOADED | -32006 | -32009 => "overloaded",
        codes::RATE_LIMITED => "rate_limited",
        codes::BACKEND_DATA_INVALID => "backend_data_invalid",
        -32601 => "not_implemented",
        _ => "internal_error",
    }
}

#[cfg(test)]
mod metric_tests {
    use super::*;

    #[test]
    fn native_codes_have_bounded_specific_labels() {
        for info in ERROR_INFOS {
            assert_ne!(
                outcome_for_code(info.code),
                "internal_error",
                "{}",
                info.name
            );
        }
        assert_eq!(outcome_for_code(-32013), "history_unavailable");
        assert_eq!(outcome_for_code(-32054), "backend_data_invalid");
        assert_eq!(outcome_for_code(-32601), "not_implemented");
        assert_eq!(outcome_for_code(i32::MIN), "internal_error");
    }
}
