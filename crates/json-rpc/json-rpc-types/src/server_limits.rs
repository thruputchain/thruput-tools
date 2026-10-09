use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Endpoint limits for one shared-snapshot read. Per-call view limits also apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewBatchLimits {
    pub max_calls: u32,
    pub max_calldata_bytes: u32,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub max_gas: u64,
    /// Maximum encoded backend result including events, before JSON encoding.
    pub max_response_bytes: u32,
    /// Upper bound on the response wait; transport and backend limits also apply.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub timeout_ms: u64,
}

/// Request bounds this JSON-RPC endpoint enforces. Returned by
/// `serverLimits`.
///
/// These are operator settings local to the serving endpoint, not consensus
/// parameters: two endpoints for the same chain may report different values,
/// and a value may change when the endpoint is reconfigured. Consensus-checked
/// execution parameters are reported by `chainConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct ServerLimits {
    /// Absent on servers that do not advertise shared-read limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_batch: Option<ViewBatchLimits>,
    /// Largest `gasLimit` a `view` request may ask for, and the budget a view
    /// runs under when it names none.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub max_view_gas: u64,
    /// Largest `view` calldata, in bytes.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub max_view_calldata_bytes: u64,
    /// Largest `limit` a `getEvents` page may ask for.
    pub max_events_per_page: u32,
    /// Largest `limit` a `getTransactionsByAddress` page may ask for.
    pub max_transactions_per_page: u32,
    /// Longest opaque pagination cursor accepted, in bytes.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub max_cursor_bytes: u64,
}

impl ServerLimits {
    /// Describe the bounds an endpoint enforces.
    #[must_use]
    pub const fn new(
        max_view_gas: u64,
        max_view_calldata_bytes: u64,
        max_events_per_page: u32,
        max_transactions_per_page: u32,
        max_cursor_bytes: u64,
    ) -> Self {
        Self {
            view_batch: None,
            max_view_gas,
            max_view_calldata_bytes,
            max_events_per_page,
            max_transactions_per_page,
            max_cursor_bytes,
        }
    }

    #[must_use]
    pub const fn with_view_batch(mut self, limits: ViewBatchLimits) -> Self {
        self.view_batch = Some(limits);
        self
    }
}
