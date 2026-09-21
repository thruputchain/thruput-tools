use groundhog_indexer::EventPageResult;
use groundhog_indexer::IndexerQueryOutcome;
use groundhog_indexer::TransactionRefPageResult;
use groundhog_indexer::indexer::TxInfoOutcome;
use groundhog_indexer::indexer::TxInfoResult;
use groundhog_json_rpc_server::RpcServerError;

use groundhog_hogd_backend::indexer::*;

#[test]
fn query_failures_are_named_and_never_read_as_an_empty_page() {
    let error = event_page(
        "indexer",
        EventPageResult::Default {
            outcome: IndexerQueryOutcome::IndexerQueryNotReady.0,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("not ready"), "{error}");

    let error = transaction_ref_page(
        "hogd-dev indexer",
        TransactionRefPageResult::Default {
            outcome: IndexerQueryOutcome::IndexerQueryInternalFailure.0,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("failed internally"), "{error}");
    assert!(error.to_string().contains("hogd-dev indexer"), "{error}");
}

#[test]
fn only_the_not_found_outcome_reads_as_a_miss() {
    assert!(matches!(
        tx_lookup(
            "indexer",
            TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            },
        ),
        Ok(TxLookup::NotFound)
    ));

    let error = tx_lookup("indexer", TxInfoResult::Default { outcome: 4242 }).unwrap_err();
    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
    assert!(error.to_string().contains("4242"), "{error}");
}
