use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress, events::EmittedEvent, view::RootInvocationOutput,
};
use serde::Serialize;

/// Schema version for the `groundhog view --json` payload.
///
/// Bump this when the shape or semantics of `ViewJsonPayload` change in a
/// way that could break existing machine consumers.
pub const VIEW_JSON_SCHEMA_VERSION: u32 = 1;

/// Structured machine-readable report for `groundhog view --json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewJsonPayload<'a> {
    pub schema_version: u32,
    pub rpc_url: &'a str,
    pub contract: &'a str,
    pub method: &'a str,
    pub entrypoint_id: &'a str,
    pub calldata: &'a str,
    pub caller: &'a Option<ContractAddress>,
    pub selector: serde_json::Value,
    pub height: &'a BlockHeight,
    pub output: &'a RootInvocationOutput,
    pub events: &'a [EmittedEvent],
    pub return_value: Option<&'a serde_json::Value>,
}
