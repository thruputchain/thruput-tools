use groundhog_indexer::EventPage;
use groundhog_indexer::EventPageResult;
use groundhog_indexer::IndexerQueryOutcome;
use groundhog_indexer::TransactionRefPageResult;
use groundhog_indexer::indexer::TxInfoOutcome;
use groundhog_indexer::indexer::TxInfoResult;
use groundhog_json_rpc_server::RpcServerError;
use groundhog_xdr::event::{IdentifiedEvent, TxEmittedEvent};
use xdr_common::{BytesM, VecM};

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

#[test]
fn a_miss_has_no_committed_status() {
    assert!(committed_status(TxLookup::NotFound).is_none());
}

#[test]
fn a_page_keeps_good_events_and_names_the_dropped_ones() {
    let event = |sequence_number: u32, num_topics: u32, len: usize| IdentifiedEvent {
        tx_hash: [0x33; 32].into(),
        block_height: 5.into(),
        sequence_number,
        emitted_event: TxEmittedEvent {
            contract_address: [7; 32].into(),
            num_topics,
            serialized_event: BytesM(vec![0xab; len]),
        },
    };
    let decoded = public_event_page(EventPage {
        events: VecM(vec![event(0, 1, 34), event(1, 3, 40), event(2, 0, 3)]),
        cursor: BytesM(vec![9, 9]),
        latest_block: 6.into(),
        oldest_block: 0.into(),
    });

    let page = decoded.page;
    assert_eq!(
        page.events
            .iter()
            .map(|e| e.sequence_number)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(page.events[0].block_height, 5.into());
    assert_eq!(page.cursor.0, vec![9, 9]);
    assert_eq!(page.latest_block, 6.into());
    assert_eq!(decoded.dropped.len(), 1);
    assert_eq!(decoded.dropped[0].sequence_number, 1);
    assert_eq!(decoded.dropped[0].block_height, 5.into());
}
