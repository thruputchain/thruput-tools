use std::time::Instant;

use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::{SHA256Hash, responses::ContractArtifactResult};
use tokio::sync::SemaphorePermit;

use super::{DB_GATEWAY, backend_deadline, timed_backend_call};
use crate::{config::HealthBackend, context::GatewayContext, metrics::BackendRequestOutcome};

async fn acquire_permits(
    ctx: &GatewayContext,
    deadline: Instant,
) -> Result<(SemaphorePermit<'_>, SemaphorePermit<'_>), RpcServerError> {
    let admission = ctx.artifact_read_admission.try_acquire().map_err(|_| {
        ctx.metrics.observe_backend_request(
            HealthBackend::DbGateway,
            "dbgateway_get_contract",
            BackendRequestOutcome::Rejected,
            std::time::Duration::ZERO,
        );
        RpcServerError::Overloaded("contract artifact read capacity exhausted".into())
    })?;
    let execution = tokio::time::timeout_at(deadline.into(), ctx.artifact_read_execution.acquire())
        .await
        .map_err(|_| RpcServerError::BackendTimeout)?
        .map_err(|_| RpcServerError::BackendUnavailable("artifact reads closed".into()))?;
    if Instant::now() >= deadline {
        return Err(RpcServerError::BackendTimeout);
    }
    Ok((admission, execution))
}

pub async fn artifact(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    hash: SHA256Hash,
) -> Result<ContractArtifactResult, RpcServerError> {
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let (_admission, _execution) = acquire_permits(ctx, deadline).await?;
    // Resolve the current connection after waiting; the request deadline also
    // bounds lock acquisition. Keep permits through artifact conversion.
    let client = tokio::time::timeout_at(deadline.into(), ctx.db_gateway.read())
        .await
        .map_err(|_| RpcServerError::BackendTimeout)?
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("DbGateway not connected".into()))?;
    if Instant::now() >= deadline {
        return Err(RpcServerError::BackendTimeout);
    }
    timed_backend_call(
        &ctx.metrics,
        HealthBackend::DbGateway,
        "dbgateway_get_contract",
        DB_GATEWAY,
        deadline,
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

#[cfg(test)]
mod tests;
