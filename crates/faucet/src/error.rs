use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Structured faucet error body returned by API handlers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FaucetErrorBody {
    error: FaucetErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FaucetErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "groundhog_json_rpc_types::decimal::option")]
    retry_after_seconds: Option<u64>,
}

pub(crate) fn error_response(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> Response {
    error_response_with_retry_after(status, code, message, None)
}

pub(crate) fn error_response_with_retry_after(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
    retry_after_seconds: Option<u64>,
) -> Response {
    (
        status,
        Json(FaucetErrorBody {
            error: FaucetErrorDetail {
                code,
                message: message.into(),
                retry_after_seconds,
            },
        }),
    )
        .into_response()
}
