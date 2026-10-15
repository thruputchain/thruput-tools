//! Address-history and exact-block transaction-reference handlers.

use thruput_hogd_backend::indexer as shared_indexer;
use thruput_indexer::TransactionRefPageResult;
use thruput_json_rpc_server::RpcRequestContext;
use thruput_json_rpc_types::transaction::{
    AddressTransactionQuery, BlockTransactionQuery, TransactionRefPage,
};

use crate::config::HealthBackend;
use crate::context::GatewayContext;
use thruput_json_rpc_server::RpcServerError;

use super::{INDEXER, backend_deadline, timed_backend_call};

/// Read one exact-height reference page from the indexer.
#[tracing::instrument(skip_all, fields(?query.height, limit = query.limit))]
pub async fn get_transactions_by_block(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    query: BlockTransactionQuery,
) -> Result<TransactionRefPage, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;
    let wire_query: thruput_indexer::BlockTransactionQuery = query.clone().into();
    timed_backend_call(
        &ctx.metrics,
        HealthBackend::Indexer,
        "get_transactions_by_block",
        INDEXER,
        backend_deadline(context, ctx.config.backend_request_timeout())?,
        async {
            client
                .get_transactions_by_block(&wire_query)
                .await
                .map(|result| {
                    let page = shared_indexer::transaction_ref_page(INDEXER, result)?.into();
                    thruput_json_rpc_server::validate_block_transaction_page(
                        &query,
                        &page,
                        ctx.config.max_cursor_bytes,
                    )?;
                    Ok(page)
                })
        },
        Result::is_ok,
    )
    .await?
}

/// Implementation of the `groundhog_getTransactionsByAddress` JSON-RPC
/// method.  Forwards the paginated `AddressTransactionQuery` to the
/// hogd indexer and returns one page of transaction refs to the caller.
#[tracing::instrument(skip_all, fields(?query.address, limit = query.limit))]
pub async fn get_transactions_by_address(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    query: AddressTransactionQuery,
) -> Result<TransactionRefPage, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;

    let wire_query: thruput_indexer::indexer::AddressTransactionQuery = query.into();
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let result = timed_backend_call(
        &ctx.metrics,
        HealthBackend::Indexer,
        "get_transactions_by_address",
        INDEXER,
        deadline,
        client.get_transactions_by_address(&wire_query),
        |result| matches!(result, TransactionRefPageResult::IndexerQueryOk { .. }),
    )
    .await?;
    let page = shared_indexer::transaction_ref_page(INDEXER, result)?;

    Ok(page.into())
}
