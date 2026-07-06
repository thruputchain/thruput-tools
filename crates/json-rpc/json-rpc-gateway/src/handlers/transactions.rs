//! `groundhog_getTransactionsByAddress` handler.

use groundhog_json_rpc_types::transaction::{AddressTransactionQuery, TransactionRefPage};
use std::time::Instant;

use crate::config::HealthBackend;
use crate::context::GatewayContext;
use crate::error::GatewayError;
use crate::metrics::BackendRequestOutcome;

/// Implementation of the `groundhog_getTransactionsByAddress` JSON-RPC
/// method.  Forwards the paginated `AddressTransactionQuery` to the
/// hogd indexer and returns one page of transaction refs to the caller.
#[tracing::instrument(skip_all, fields(?query.address, limit = query.limit))]
pub async fn get_transactions_by_address(
    ctx: &GatewayContext,
    query: AddressTransactionQuery,
) -> Result<TransactionRefPage, GatewayError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("indexer not connected".into()))?;

    let wire_query: groundhog_indexer::indexer::AddressTransactionQuery = query.into();
    let started_at = Instant::now();
    let rpc_result = client.get_transactions_by_address(&wire_query).await;
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "get_transactions_by_address",
        if rpc_result.is_ok() {
            BackendRequestOutcome::Ok
        } else {
            BackendRequestOutcome::Error
        },
        started_at.elapsed(),
    );
    let page = rpc_result.map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;

    Ok(page.into())
}
