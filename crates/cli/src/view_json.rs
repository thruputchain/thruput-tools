use serde::Serialize;
use thruput_json_rpc_types::{
    BlockHeight, ContractAddress, events::EmittedEvent, view::RootInvocationOutput,
};

/// Schema version for the `thruput view --json` payload.
///
/// Bump this when the shape or semantics of `ViewJsonPayload` change in a
/// way that could break existing machine consumers.
pub const VIEW_JSON_SCHEMA_VERSION: u32 = 3;

/// Structured machine-readable report for `thruput view --json`.
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
    #[serde(serialize_with = "thruput_json_rpc_types::decimal::serialize::<BlockHeight, _>")]
    pub height: &'a BlockHeight,
    /// Gas the viewed invocation consumed.
    #[serde(with = "thruput_json_rpc_types::decimal")]
    pub gas_used: u64,
    pub output: &'a RootInvocationOutput,
    pub events: &'a [EmittedEvent],
    pub return_value: Option<&'a serde_json::Value>,
    /// Exception type name decoded from an abort output, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_exception: Option<&'a str>,
    /// Crate defining the abort exception, when it is a known standard
    /// exception rather than one declared by the viewed contract's spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_exception_crate: Option<&'a str>,
    /// Major version of the defining crate baked into the abort selector,
    /// when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_exception_version: Option<&'a str>,
    /// Exception payload decoded from an abort output, when decodable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_value: Option<&'a serde_json::Value>,
    /// Why an abort output could not be decoded, when decoding failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_decode_error: Option<String>,
}
