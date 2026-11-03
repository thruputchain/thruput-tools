use std::time::Instant;

use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::{SHA256Hash, responses::ContractArtifactResult};
use tokio::sync::SemaphorePermit;
use tracing::Instrument;

use super::{DB_GATEWAY, backend_deadline};
use crate::{config::HealthBackend, context::GatewayContext, metrics::BackendRequestOutcome};

async fn acquire_permits(
    ctx: &GatewayContext,
    deadline: Instant,
) -> Result<(SemaphorePermit<'_>, SemaphorePermit<'_>), RpcServerError> {
    let admission = ctx.artifact_read_admission.try_acquire().map_err(|_| {
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
    let started = Instant::now();
    let result = read_artifact(ctx, context, hash).await;
    // One observation includes admission/queue/lock waits as well as the RPC.
    let outcome = match &result {
        Ok(_) | Err(RpcServerError::ContractArtifactUnavailable(_)) => BackendRequestOutcome::Ok,
        Err(RpcServerError::BackendTimeout) => BackendRequestOutcome::Timeout,
        Err(RpcServerError::Overloaded(_)) => BackendRequestOutcome::Rejected,
        Err(_) => BackendRequestOutcome::Error,
    };
    ctx.metrics.observe_backend_request(
        HealthBackend::DbGateway,
        "dbgateway_get_contract",
        outcome,
        started.elapsed(),
    );
    result
}

async fn read_artifact(
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
    let result = tokio::time::timeout_at(deadline.into(), client.dbgateway_get_contract(&hash.0))
        .instrument(crate::telemetry::backend_span(
            DB_GATEWAY,
            "dbgateway_get_contract",
        ))
        .await
        .map_err(|_| RpcServerError::BackendTimeout)?
        .map_err(|error| RpcServerError::BackendUnavailable(format!("{DB_GATEWAY}: {error}")))?;
    thruput_hogd_backend::native::contract_artifact(hash, result)
}

#[cfg(test)]
mod tests;
