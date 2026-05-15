use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::events::EmittedEvent;
use crate::hex::HexBytes;
use crate::{BlockHeight, ContractAddress};

/// Request for read-only contract execution against the latest committed state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewRequest {
    /// Contract address to execute.
    pub contract: ContractAddress,

    /// Contract entrypoint id. Must be exactly 8 bytes.
    pub entrypoint: HexBytes,

    /// Opaque call input bytes.
    pub calldata: HexBytes,

    /// Optional simulated caller address observed by `get_msg_sender`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<ContractAddress>,
}

/// Result of read-only contract execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewResponse {
    /// Root invocation output.
    pub output: RootInvocationOutput,

    /// Events emitted during view execution.
    ///
    /// Hogd captures emitted events from the simulated frame; wallet-preview
    /// callers see what events a real submit would produce.
    pub events: Vec<EmittedEvent>,

    /// Block height of the committed snapshot used for the view.
    pub height: BlockHeight,
}

/// Root invocation output with raw buffers preserved as hex.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(
    tag = "result",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RootInvocationOutput {
    /// Contract returned normally.
    Return { return_data: HexBytes },

    /// Contract aborted/reverted deterministically.
    Abort { abort_data: HexBytes },

    /// Execution faulted before a return or abort payload was produced.
    Fault,
}
