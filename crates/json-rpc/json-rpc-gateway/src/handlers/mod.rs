pub mod blocks;
pub mod events;
mod parse_event;
pub mod receipt;
pub mod transactions;
pub mod view;

use std::time::{Duration, Instant};

use groundhog_indexer::{
    EventPage as IndexerEventPage, EventPageResult, IndexerQueryOutcome,
    TransactionRefPage as IndexerTransactionRefPage, TransactionRefPageResult,
};
use groundhog_json_rpc_server::RpcRequestContext;

use crate::error::GatewayError;

fn backend_deadline(
    context: RpcRequestContext,
    timeout: Duration,
) -> Result<Instant, GatewayError> {
    let deadline = context
        .capped_from_start(timeout)
        .deadline()
        .expect("bug: backend timeout should establish a deadline");
    if deadline <= Instant::now() {
        Err(GatewayError::BackendTimeout)
    } else {
        Ok(deadline)
    }
}

fn indexer_query_error(outcome: i32) -> GatewayError {
    let message = if outcome == IndexerQueryOutcome::IndexerQueryNotReady.0 {
        "indexer query backend is not ready".to_string()
    } else if outcome == IndexerQueryOutcome::IndexerQueryInternalFailure.0 {
        "indexer query backend failed internally".to_string()
    } else {
        format!("indexer query returned unexpected outcome: {outcome}")
    };
    GatewayError::BackendUnavailable(message)
}

fn event_page_from_result(result: EventPageResult) -> Result<IndexerEventPage, GatewayError> {
    match result {
        EventPageResult::IndexerQueryOk { page } => Ok(page),
        EventPageResult::Default { outcome } => Err(indexer_query_error(outcome)),
        _ => Err(GatewayError::BackendUnavailable(
            "indexer query returned an unknown event-page result".into(),
        )),
    }
}

fn transaction_ref_page_from_result(
    result: TransactionRefPageResult,
) -> Result<IndexerTransactionRefPage, GatewayError> {
    match result {
        TransactionRefPageResult::IndexerQueryOk { page } => Ok(page),
        TransactionRefPageResult::Default { outcome } => Err(indexer_query_error(outcome)),
        _ => Err(GatewayError::BackendUnavailable(
            "indexer query returned an unknown transaction-page result".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use groundhog_json_rpc_server::RpcRequestContext;

    use groundhog_indexer::{EventPageResult, IndexerQueryOutcome, TransactionRefPageResult};

    use super::{backend_deadline, event_page_from_result, transaction_ref_page_from_result};

    #[test]
    fn backend_deadline_is_measured_from_request_start() {
        let context = RpcRequestContext::new(Some(Duration::from_secs(10)));
        let expected = context
            .started_at()
            .checked_add(Duration::from_secs(5))
            .expect("test deadline should fit");

        assert_eq!(
            backend_deadline(context, Duration::from_secs(5)).unwrap(),
            expected
        );
    }

    #[test]
    fn event_page_backend_failure_is_not_an_empty_page() {
        let error = event_page_from_result(EventPageResult::Default {
            outcome: IndexerQueryOutcome::IndexerQueryNotReady.0,
        })
        .unwrap_err();

        assert!(matches!(
            error,
            crate::error::GatewayError::BackendUnavailable(_)
        ));
        assert!(error.to_string().contains("not ready"));
    }

    #[test]
    fn transaction_page_backend_failure_is_not_an_empty_page() {
        let error = transaction_ref_page_from_result(TransactionRefPageResult::Default {
            outcome: IndexerQueryOutcome::IndexerQueryInternalFailure.0,
        })
        .unwrap_err();

        assert!(matches!(
            error,
            crate::error::GatewayError::BackendUnavailable(_)
        ));
        assert!(error.to_string().contains("failed internally"));
    }
}
