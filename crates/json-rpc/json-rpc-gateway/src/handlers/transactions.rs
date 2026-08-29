//! `groundhog_getTransactionsByAddress` handler.

use groundhog_indexer::TransactionRefPageResult;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::transaction::{AddressTransactionQuery, TransactionRefPage};
use std::time::Instant;

use crate::config::HealthBackend;
use crate::context::GatewayContext;
use crate::error::GatewayError;
use crate::metrics::BackendRequestOutcome;

use super::{backend_deadline, transaction_ref_page_from_result};

/// Implementation of the `groundhog_getTransactionsByAddress` JSON-RPC
/// method.  Forwards the paginated `AddressTransactionQuery` to the
/// hogd indexer and returns one page of transaction refs to the caller.
#[tracing::instrument(skip_all, fields(?query.address, limit = query.limit))]
pub async fn get_transactions_by_address(
    ctx: &GatewayContext,
    context: RpcRequestContext,
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
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let rpc_result = tokio::time::timeout_at(
        deadline.into(),
        client.get_transactions_by_address(&wire_query),
    )
    .await;
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "get_transactions_by_address",
        match &rpc_result {
            Err(_) => BackendRequestOutcome::Timeout,
            Ok(Ok(TransactionRefPageResult::IndexerQueryOk { .. })) => BackendRequestOutcome::Ok,
            Ok(Ok(_) | Err(_)) => BackendRequestOutcome::Error,
        },
        started_at.elapsed(),
    );
    let result = rpc_result
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;
    let page = transaction_ref_page_from_result(result)?;

    Ok(page.into())
}
