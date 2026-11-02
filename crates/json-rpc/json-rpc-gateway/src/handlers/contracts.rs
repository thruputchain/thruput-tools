use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::{SHA256Hash, responses::ContractArtifactResult};

use super::{DB_GATEWAY, backend_deadline, timed_backend_call};
use crate::{config::HealthBackend, context::GatewayContext};

pub async fn artifact(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    hash: SHA256Hash,
) -> Result<ContractArtifactResult, RpcServerError> {
    let client = ctx
        .db_gateway
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("DbGateway not connected".into()))?;
    timed_backend_call(
        &ctx.metrics,
        HealthBackend::DbGateway,
        "dbgateway_get_contract",
        DB_GATEWAY,
        backend_deadline(context, ctx.config.backend_request_timeout())?,
        async {
            client
                .dbgateway_get_contract(&hash.0)
                .await
                .map(|result| thruput_hogd_backend::native::contract_artifact(hash, result))
        },
        Result::is_ok,
    )
    .await?
}
