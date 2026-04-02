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

    /// Height range too large for getContractEvents.
    pub const RANGE_TOO_LARGE: i32 = -32011;

    /// Backend (hogd indexer or scheduler) unavailable.
    pub const BACKEND_UNAVAILABLE: i32 = -32050;

    /// Request timed out waiting for backend response.
    pub const BACKEND_TIMEOUT: i32 = -32051;

    /// Method exists but is not yet implemented in this gateway version.
    pub const NOT_IMPLEMENTED: i32 = -32052;
}
