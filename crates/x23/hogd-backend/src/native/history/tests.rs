use super::*;
use serde_json::json;

fn address() -> thruput_json_rpc_types::ContractAddress {
    Base58(thruput_xdr::types::ContractAddress::from([7; 32]))
}

fn identified(
    sequence_number: u32,
    num_topics: u32,
    bytes: Vec<u8>,
) -> thruput_xdr::event::IdentifiedEvent {
    thruput_xdr::event::IdentifiedEvent {
        tx_hash: thruput_xdr::types::SHA256Hash::from([9; 32]),
        block_height: 12.into(),
        sequence_number,
        emitted_event: TxEmittedEvent {
            contract_address: address().0,
            num_topics,
            serialized_event: BytesM(bytes),
        },
    }
}

fn events(
    events: Vec<thruput_xdr::event::IdentifiedEvent>,
    cursor: Vec<u8>,
) -> wire::EventPageResult {
    wire::EventPageResult::IndexerQueryOk {
        page: wire::EventPage {
            events: VecM(events),
            cursor: BytesM(cursor),
            latest_block: 20.into(),
            oldest_block: 2.into(),
        },
    }
}

#[test]
fn query_conversions_preserve_predicates_and_explicit_initial_position() {
    let request: input::EventQuery = serde_json::from_value(json!({
        "filters": [
            {"addresses": [address()], "topics": {"topic1": [format!("0x{}", "ab".repeat(32))]}},
            {"addresses": [address()]}
        ],
        "start": {"order": "descending", "anchor": {"kind": "atHeight", "blockHeight": "17"}},
        "limit": 7
    }))
    .unwrap();
    let query = event_query(&request).unwrap();
    assert_eq!(
        query.order,
        pagination::IterationOrder::IterationOrderDescending
    );
    assert_eq!(
        query.anchor,
        pagination::BlockAnchor::AnchorAtHeight { height: 17.into() }
    );
    assert!(query.cursor.0.is_empty());
    assert_eq!(query.limit, 7);
    assert_eq!(query.filters.0.len(), 2);
    assert_eq!(query.filters.0[0].contract_addresses.0, vec![address().0]);
    assert!(query.filters.0[0].topics.topic0.0.is_empty());
    assert_eq!(query.filters.0[0].topics.topic1.0, vec![[0xab; 32].into()]);
    assert!(query.filters.0[0].topics.topic2.0.is_empty());
    assert!(query.filters.0[1].topics.topic1.0.is_empty());
}

#[test]
fn all_range_anchors_keep_the_requested_direction() {
    for (start, ascending, anchor) in [
        (
            json!({"order":"ascending","anchor":{"kind":"atBeginning"}}),
            true,
            pagination::BlockAnchor::AnchorAtBeginning,
        ),
        (
            json!({"order":"ascending","anchor":{"kind":"atHeight","blockHeight":"3"}}),
            true,
            pagination::BlockAnchor::AnchorAtHeight { height: 3.into() },
        ),
        (
            json!({"order":"descending","anchor":{"kind":"atEnd"}}),
            false,
            pagination::BlockAnchor::AnchorAtEnd,
        ),
        (
            json!({"order":"descending","anchor":{"kind":"atHeight","blockHeight":"4"}}),
            false,
            pagination::BlockAnchor::AnchorAtHeight { height: 4.into() },
        ),
    ] {
        let request: input::RootAddressQuery = serde_json::from_value(json!({
            "address":address(), "start": start, "limit": 4
        }))
        .unwrap();
        let query = root_query(&request).unwrap();
        assert_eq!(query.address, address().0);
        assert_eq!(query.anchor, anchor);
        assert_eq!(
            query.order,
            if ascending {
                pagination::IterationOrder::IterationOrderAscending
            } else {
                pagination::IterationOrder::IterationOrderDescending
            }
        );
        assert_eq!(query.limit, 4);
    }
}

#[test]
fn continuations_forward_cursor_bytes_verbatim() {
    let event: input::EventQuery = serde_json::from_value(json!({
        "filters": [{"addresses": [address()]}], "cursor": "0x00ff1200", "limit": 13
    }))
    .unwrap();
    let root: input::RootAddressQuery = serde_json::from_value(json!({
        "address": address(), "cursor": "0x00ff1200", "limit": 13
    }))
    .unwrap();
    let block: input::BlockPageRequest = serde_json::from_value(json!({
        "blockHeight":"99", "cursor": "0x00ff1200", "limit": 13
    }))
    .unwrap();
    assert_eq!(event_query(&event).unwrap().cursor.0, vec![0, 255, 18, 0]);
    assert_eq!(root_query(&root).unwrap().cursor.0, vec![0, 255, 18, 0]);
    let query = block_query(&block);
    assert_eq!(query.cursor.0, vec![0, 255, 18, 0]);
    assert_eq!(query.height, 99.into());
    assert_eq!(query.limit, 13);
    let block: input::BlockPageRequest = serde_json::from_value(json!({
        "blockHeight":"99", "start":{"order":"descending"}, "limit": 2
    }))
    .unwrap();
    assert_eq!(
        block_query(&block).order,
        pagination::IterationOrder::IterationOrderDescending
    );
}

#[test]
fn projections_preserve_event_identity_topics_payload_and_bounds() {
    let mut bytes = vec![1; 32];
    bytes.extend([2; 32]);
    bytes.extend([3, 4, 5]);
    let page = event_page(events(vec![identified(7, 2, bytes)], vec![0, 255])).unwrap();
    assert_eq!(page.next_cursor, Some(HexBytes(vec![0, 255])));
    assert_eq!(page.oldest_indexed_block_height, 2.into());
    assert_eq!(page.latest_indexed_block_height, 20.into());
    let item = &page.events[0];
    assert_eq!(
        item.transaction_hash,
        TxHash(Base58(thruput_xdr::types::SHA256Hash::from([9; 32])))
    );
    assert_eq!(item.block_height, 12.into());
    assert_eq!(item.sequence_number, 7);
    assert_eq!(item.event.address, address());
    assert_eq!(item.event.topics, vec![Topic([1; 32]), Topic([2; 32])]);
    assert_eq!(item.event.data, HexBytes(vec![3, 4, 5]));
}

#[test]
fn malformed_event_fails_the_entire_page_instead_of_skipping_it() {
    for malformed in [
        identified(2, 4, vec![0; 128]),
        identified(2, 1, vec![0; 31]),
        identified(2, 3, vec![0; 95]),
        identified(2, 0, vec![0; kMaxEventLength as usize + 1]),
    ] {
        // A malformed row invalidates the page even between valid rows or
        // on a terminal page. Neither a partial answer nor exhaustion is safe.
        for position in 0..=2 {
            for cursor in [vec![], vec![1]] {
                let mut rows = vec![identified(1, 0, vec![7]), identified(3, 0, vec![8])];
                rows.insert(position, malformed.clone());
                let result = event_page(events(rows, cursor));
                assert!(matches!(
                    result,
                    Err(RpcServerError::BackendDataInvalid {
                        reason: Reason::InvalidEncoding,
                        ..
                    })
                ));
            }
        }
    }
}

#[test]
fn valid_topic_and_payload_boundaries_are_not_rejected() {
    for (num_topics, bytes) in [
        (0, vec![]),
        (3, vec![7; 96]),
        (3, vec![7; kMaxEventLength as usize]),
    ] {
        let expected_data_len = bytes.len() - num_topics as usize * 32;
        let page = event_page(events(vec![identified(0, num_topics, bytes)], vec![])).unwrap();
        assert_eq!(page.events[0].event.topics.len(), num_topics as usize);
        assert_eq!(page.events[0].event.data.0.len(), expected_data_len);
    }
}

#[test]
fn empty_nonterminal_pages_remain_nonterminal() {
    let page = event_page(events(vec![], vec![3, 7, 0])).unwrap();
    assert!(page.events.is_empty());
    assert_eq!(page.next_cursor, Some(HexBytes(vec![3, 7, 0])));
    assert!(
        event_page(events(vec![], vec![]))
            .unwrap()
            .next_cursor
            .is_none()
    );
    for cursor in [vec![], vec![3, 7, 0]] {
        let page = transaction_page(wire::TransactionRefPageResult::IndexerQueryOk {
            page: wire::TransactionRefPage {
                refs: VecM(vec![]),
                cursor: BytesM(cursor.clone()),
                latest_block: 20.into(),
                oldest_block: 2.into(),
            },
        })
        .unwrap();
        assert!(page.transactions.is_empty());
        assert_eq!(
            page.next_cursor,
            (!cursor.is_empty()).then_some(HexBytes(cursor))
        );
        assert_eq!(page.oldest_indexed_block_height, 2.into());
        assert_eq!(page.latest_indexed_block_height, 20.into());
    }
}

#[test]
fn transaction_references_keep_their_hash_and_height() {
    let hash = thruput_xdr::types::SHA256Hash::from([9; 32]);
    let page = transaction_page(wire::TransactionRefPageResult::IndexerQueryOk {
        page: wire::TransactionRefPage {
            refs: VecM(vec![wire::TransactionRef {
                tx_hash: hash,
                height: 12.into(),
            }]),
            cursor: BytesM(vec![]),
            latest_block: 20.into(),
            oldest_block: 2.into(),
        },
    })
    .unwrap();
    assert_eq!(page.transactions.len(), 1);
    assert_eq!(page.transactions[0].transaction_hash, TxHash(Base58(hash)));
    assert_eq!(page.transactions[0].block_height, 12.into());
}

#[test]
fn typed_outcomes_never_turn_into_empty_pages() {
    use wire::IndexerQueryOutcome as Outcome;
    for outcome in [
        Outcome::IndexerQueryInvalidCursor,
        Outcome::IndexerQueryHistoryUnavailable,
        Outcome::IndexerQueryNotReady,
        Outcome::IndexerQueryInternalFailure,
        Outcome::IndexerQueryOk,
        Outcome::from(i32::MAX),
    ] {
        let event = event_page(wire::EventPageResult::Default { outcome: outcome.0 }).unwrap_err();
        let transaction =
            transaction_page(wire::TransactionRefPageResult::Default { outcome: outcome.0 })
                .unwrap_err();
        assert_eq!(event, transaction);
        if outcome == Outcome::IndexerQueryInvalidCursor {
            assert!(matches!(event, RpcServerError::InvalidCursor(_)));
        } else if outcome == Outcome::IndexerQueryHistoryUnavailable {
            assert!(matches!(event, RpcServerError::HistoryUnavailable(_)));
        } else if outcome == Outcome::IndexerQueryNotReady
            || outcome == Outcome::IndexerQueryInternalFailure
        {
            assert!(matches!(event, RpcServerError::BackendUnavailable(_)));
        } else {
            assert!(matches!(
                event,
                RpcServerError::BackendDataInvalid {
                    reason: Reason::UnsupportedValue,
                    ..
                }
            ));
        }
    }
}
