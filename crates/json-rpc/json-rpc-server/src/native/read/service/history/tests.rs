use super::*;
use crate::RpcServerConfig;
use thruput_json_rpc_types::{base58::Base58, transaction::TxHash};

fn query() -> input::EventQuery {
    serde_json::from_value(serde_json::json!({
        "filters": [{"addresses": [Base58(thruput_xdr::types::ContractAddress::from([1;32]))]}],
        "start": {"order": "ascending", "anchor": {"kind": "atBeginning"}}, "limit": 2
    }))
    .unwrap()
}
fn event(byte: u8) -> out::IdentifiedEvent {
    out::IdentifiedEvent {
        transaction_hash: TxHash(Base58([byte; 32].into())),
        block_height: 9.into(),
        sequence_number: 0,
        event: out::EmittedEvent {
            address: Base58([1; 32].into()),
            topics: vec![],
            data: HexBytes(vec![]),
        },
    }
}
fn events() -> out::EventPage {
    out::EventPage {
        events: vec![event(0x7f), event(0x80)],
        next_cursor: Some(HexBytes(vec![2])),
        oldest_indexed_block_height: 0.into(),
        latest_indexed_block_height: 10.into(),
    }
}

#[test]
fn direct_requests_require_exclusive_start_and_bounded_positive_limit() {
    let mut q = query();
    q.limit = None;
    prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).unwrap();
    assert_eq!(q.limit, Some(17));
    q.limit = Some(0);
    assert!(prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).is_err());
    q.limit = Some(18);
    assert!(prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).is_err());
    q.limit = Some(1);
    q.cursor = Some(HexBytes(vec![1]));
    assert!(prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).is_err());
    q.start = None;
    prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).unwrap();
    q.cursor = Some(HexBytes(vec![]));
    assert!(prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).is_err());
    q.cursor = Some(HexBytes(vec![0; 257]));
    assert!(prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).is_err());
    q.cursor = None;
    assert!(prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).is_err());
}

#[test]
fn direct_filters_and_anchors_cannot_bypass_deserialization_checks() {
    let mut q = query();
    validate_filters(&q.filters).unwrap();
    q.filters[0].addresses.clear();
    assert!(validate_filters(&q.filters).is_err());
    assert!(validate_filters(&[]).is_err());
    let mut q = query();
    q.filters[0].topics = Some(input::TopicFilter {
        topic0: Some(vec![
            thruput_json_rpc_types::hex::Topic([0; 32]);
            events::MAX_TOPIC_VALUES_PER_POSITION as usize + 1
        ]),
        topic1: None,
        topic2: None,
    });
    assert!(validate_filters(&q.filters).is_err());
    assert!(
        anchor(&input::HistoryStart::Ascending {
            anchor: input::AscendingAnchor::AtHeight {
                block_height: (-1).into()
            }
        })
        .is_err()
    );
}

#[test]
fn event_pages_allow_empty_progress_but_reject_bad_metadata() {
    let config = RpcServerConfig::default();
    let mut result = events();
    result.events.clear();
    validate_event_page(&query(), &result, config.max_cursor_bytes).unwrap();
    let mut request = query();
    request.start = None;
    request.cursor = result.next_cursor.clone();
    assert!(validate_event_page(&request, &result, config.max_cursor_bytes).is_err());
    result.next_cursor = None;
    validate_event_page(&request, &result, config.max_cursor_bytes).unwrap();
    result.oldest_indexed_block_height = (-1).into();
    assert!(validate_event_page(&request, &result, config.max_cursor_bytes).is_err());
    result.oldest_indexed_block_height = 11.into();
    assert!(validate_event_page(&request, &result, config.max_cursor_bytes).is_err());
    result = events();
    result.next_cursor = Some(HexBytes(vec![]));
    assert!(validate_event_page(&query(), &result, config.max_cursor_bytes).is_err());
    result.next_cursor = Some(HexBytes(vec![0; 257]));
    assert!(validate_event_page(&query(), &result, config.max_cursor_bytes).is_err());
}

#[test]
fn events_require_order_membership_and_declared_bounds() {
    let config = RpcServerConfig::default();
    let mut result = events();
    validate_event_page(&query(), &result, config.max_cursor_bytes).unwrap();
    result.events.reverse();
    assert!(validate_event_page(&query(), &result, config.max_cursor_bytes).is_err());
    let mut request = query();
    request.start = None;
    request.cursor = Some(HexBytes(vec![1]));
    validate_event_page(&request, &result, config.max_cursor_bytes).unwrap();
    result.events[1] = result.events[0].clone();
    assert!(validate_event_page(&request, &result, config.max_cursor_bytes).is_err());
    result = events();
    result.events[0].event.address = Base58([2; 32].into());
    assert!(validate_event_page(&query(), &result, config.max_cursor_bytes).is_err());
    result = events();
    result.events[0].block_height = 11.into();
    assert!(validate_event_page(&query(), &result, config.max_cursor_bytes).is_err());
    result = events();
    result.events.push(event(0x90));
    assert!(validate_event_page(&query(), &result, config.max_cursor_bytes).is_err());
    request = query();
    request.filters[0].topics = Some(input::TopicFilter {
        topic0: Some(vec![thruput_json_rpc_types::hex::Topic([0; 32])]),
        topic1: None,
        topic2: None,
    });
    assert!(validate_event_page(&request, &events(), config.max_cursor_bytes).is_err());
    request.filters[0].topics.as_mut().unwrap().topic0 = Some(vec![]);
    validate_event_page(&request, &events(), config.max_cursor_bytes).unwrap();
}

#[test]
fn successful_range_pages_cannot_silently_clamp_an_explicit_anchor() {
    let mut request = query();
    request.start = Some(input::HistoryStart::Ascending {
        anchor: input::AscendingAnchor::AtHeight {
            block_height: 2.into(),
        },
    });
    let mut result = events();
    result.oldest_indexed_block_height = 3.into();
    assert!(matches!(
        validate_event_page(
            &request,
            &result,
            RpcServerConfig::default().max_cursor_bytes
        ),
        Err(RpcServerError::BackendDataInvalid { .. })
    ));
    request.start = Some(input::HistoryStart::Descending {
        anchor: input::DescendingAnchor::AtHeight {
            block_height: 2.into(),
        },
    });
    result.events.clear();
    result.next_cursor = None;
    assert!(matches!(
        validate_event_page(
            &request,
            &result,
            RpcServerConfig::default().max_cursor_bytes
        ),
        Err(RpcServerError::BackendDataInvalid { .. })
    ));
}

#[test]
fn identities_cannot_repeat_at_different_heights() {
    let mut result = events();
    result.events[1] = result.events[0].clone();
    result.events[1].block_height = 10.into();
    assert!(
        validate_event_page(
            &query(),
            &result,
            RpcServerConfig::default().max_cursor_bytes
        )
        .is_err()
    );
    let hash = TxHash(Base58([1; 32].into()));
    let refs = out::TransactionRefPage {
        transactions: [9, 10]
            .into_iter()
            .map(|height| out::TransactionRef {
                transaction_hash: hash,
                block_height: height.into(),
            })
            .collect(),
        next_cursor: None,
        oldest_indexed_block_height: 0.into(),
        latest_indexed_block_height: 10.into(),
    };
    assert!(validate_refs(&refs, Some(events::IterationOrder::Ascending)).is_err());
}

fn invalid_param(result: &Result<(), RpcServerError>) -> bool {
    matches!(result, Err(RpcServerError::InvalidParam(_)))
}
fn invalid_data(result: &Result<(), RpcServerError>) -> bool {
    matches!(result, Err(RpcServerError::BackendDataInvalid { .. }))
}

#[test]
fn filter_bounds_accept_each_protocol_maximum() {
    let mut q = query();
    q.filters = vec![q.filters[0].clone(); events::MAX_EVENT_FILTERS_PER_QUERY as usize];
    validate_filters(&q.filters).unwrap();
    q.filters.push(q.filters[0].clone());
    assert!(invalid_param(&validate_filters(&q.filters)));

    let mut q = query();
    let address = q.filters[0].addresses[0];
    q.filters[0].addresses =
        vec![address; events::MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER as usize];
    validate_filters(&q.filters).unwrap();
    q.filters[0].addresses.push(address);
    assert!(invalid_param(&validate_filters(&q.filters)));

    let mut q = query();
    q.filters[0].topics = Some(input::TopicFilter {
        topic0: None,
        topic1: None,
        topic2: Some(vec![
            thruput_json_rpc_types::hex::Topic([0; 32]);
            events::MAX_TOPIC_VALUES_PER_POSITION as usize
        ]),
    });
    validate_filters(&q.filters).unwrap();
}

#[test]
fn cursors_may_fill_the_configured_maximum() {
    let mut q = query();
    q.start = None;
    q.cursor = Some(HexBytes(vec![1; 256]));
    prepare(q.start.as_ref(), q.cursor.as_ref(), &mut q.limit, 17, 256).unwrap();
    let mut result = events();
    result.next_cursor = Some(HexBytes(vec![2; 256]));
    validate_event_page(&query(), &result, 256).unwrap();
}

#[test]
fn zero_is_a_valid_anchor_height() {
    let start = input::HistoryStart::Ascending {
        anchor: input::AscendingAnchor::AtHeight {
            block_height: 0.into(),
        },
    };
    assert_eq!(anchor(&start).unwrap(), Some(&0.into()));
}

#[test]
fn descending_pages_must_descend_from_an_inclusive_anchor() {
    let max = RpcServerConfig::default().max_cursor_bytes;
    let at = |height: i64| {
        Some(input::HistoryStart::Descending {
            anchor: input::DescendingAnchor::AtHeight {
                block_height: height.into(),
            },
        })
    };
    let mut request = query();
    let mut result = events();
    result.events.reverse();
    request.start = Some(input::HistoryStart::Descending {
        anchor: input::DescendingAnchor::AtEnd {},
    });
    validate_event_page(&request, &result, max).unwrap();
    request.start = at(9);
    validate_event_page(&request, &result, max).unwrap();
    request.start = at(8);
    assert!(invalid_data(&validate_event_page(&request, &result, max)));
    request.start = at(9);
    result.events.reverse();
    assert!(invalid_data(&validate_event_page(&request, &result, max)));
}

#[test]
fn indexed_range_and_anchor_retention_bounds_are_inclusive() {
    let max = RpcServerConfig::default().max_cursor_bytes;
    let mut result = events();
    result.oldest_indexed_block_height = 9.into();
    result.latest_indexed_block_height = 9.into();
    validate_event_page(&query(), &result, max).unwrap();

    let mut result = events();
    result.events[1].block_height = 10.into();
    validate_event_page(&query(), &result, max).unwrap();

    let mut request = query();
    request.start = Some(input::HistoryStart::Ascending {
        anchor: input::AscendingAnchor::AtHeight {
            block_height: 3.into(),
        },
    });
    let mut result = events();
    result.oldest_indexed_block_height = 3.into();
    validate_event_page(&request, &result, max).unwrap();
}

#[test]
fn events_carry_at_most_three_topics() {
    let max = RpcServerConfig::default().max_cursor_bytes;
    let topic = thruput_json_rpc_types::hex::Topic([0; 32]);
    let mut result = events();
    result.events[0].event.topics = vec![topic; 3];
    validate_event_page(&query(), &result, max).unwrap();
    result.events[0].event.topics.push(topic);
    assert!(invalid_data(&validate_event_page(&query(), &result, max)));
}
