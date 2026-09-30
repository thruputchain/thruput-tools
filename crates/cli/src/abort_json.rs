use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::codec::{AbortDecodeError, DecodedAbort};

/// Decoded-abort fields shared by typed CLI report payloads.
///
/// Flattened into report structs so a decoded abort contributes
/// `abortException` / `abortValue` (plus crate attribution when the exception
/// resolved through the describe registry), a failed decode contributes
/// `abortDecodeError`, and a missing abort contributes no keys at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AbortReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_exception: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_exception_crate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_exception_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_decode_error: Option<String>,
}

impl AbortReport {
    /// Project a receipt's abort-decode outcome into report fields.
    pub(crate) fn from_decode(
        decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    ) -> Self {
        match decoded_abort {
            Some(Ok(abort)) => Self {
                abort_exception: abort.exception.clone(),
                abort_exception_crate: abort.exception_crate.map(str::to_owned),
                abort_exception_version: abort.exception_version.map(str::to_owned),
                abort_value: Some(abort.value.clone()),
                abort_decode_error: None,
            },
            Some(Err(error)) => Self {
                abort_decode_error: Some(error.to_string()),
                ..Self::default()
            },
            None => Self::default(),
        }
    }
}
