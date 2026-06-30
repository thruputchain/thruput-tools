use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use groundhog_json_rpc_types::ContractAddress;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AirdropRequest {
    recipient: ContractAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: &'static str,
}

pub(crate) async fn request_airdrop(Json(_request): Json<AirdropRequest>) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorBody {
            error: ErrorDetail {
                code: "airdrop_not_implemented",
                message: "airdrop submission is not implemented yet",
            },
        }),
    )
        .into_response()
}
