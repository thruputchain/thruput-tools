use thruput_json_rpc_types::events::*;

fn address_json() -> serde_json::Value {
    serde_json::Value::String(format!("0x{}", "11".repeat(32)))
}

fn topic_json() -> serde_json::Value {
    serde_json::Value::String(format!("0x{}", "22".repeat(32)))
}

fn filter_json(addresses: usize, topic0_values: usize) -> serde_json::Value {
    serde_json::json!({
        "contractAddresses": vec![address_json(); addresses],
        "topics": {
            "topic0": vec![topic_json(); topic0_values],
            "topic1": [],
            "topic2": [],
        },
    })
}

fn query_json(filters: usize, addresses: usize, topic0_values: usize) -> serde_json::Value {
    serde_json::json!({
        "filters": vec![filter_json(addresses, topic0_values); filters],
        "order": "ascending",
        "anchor": { "kind": "atBeginning" },
        "cursor": "0x",
        "limit": 0,
    })
}

// The RPC boundary is JSON: every `getEvents` request is deserialized
// through these types before any handler runs, so the `VecM` bounds
// below are the request-size limits the server enforces.

#[test]
fn event_query_at_the_filter_list_bounds_deserializes() {
    let query = query_json(
        MAX_EVENT_FILTERS_PER_QUERY as usize,
        MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER as usize,
        MAX_TOPIC_VALUES_PER_POSITION as usize,
    );

    let parsed: EventQuery =
        serde_json::from_value(query).expect("bug: bounded query should deserialize");

    assert_eq!(
        parsed.filters.0.0.len(),
        MAX_EVENT_FILTERS_PER_QUERY as usize
    );
}

#[test]
fn event_query_with_too_many_filters_is_rejected_at_deserialization() {
    let query = query_json(MAX_EVENT_FILTERS_PER_QUERY as usize + 1, 1, 1);

    let error = serde_json::from_value::<EventQuery>(query)
        .expect_err("bug: oversized filter list should fail to deserialize");

    assert!(error.to_string().contains("length"), "{error}");
}

#[test]
fn event_filter_with_too_many_addresses_is_rejected_at_deserialization() {
    let query = query_json(1, MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER as usize + 1, 1);

    let error = serde_json::from_value::<EventQuery>(query)
        .expect_err("bug: oversized address list should fail to deserialize");

    assert!(error.to_string().contains("length"), "{error}");
}

#[test]
fn topic_position_with_too_many_values_is_rejected_at_deserialization() {
    let query = query_json(1, 1, MAX_TOPIC_VALUES_PER_POSITION as usize + 1);

    let error = serde_json::from_value::<EventQuery>(query)
        .expect_err("bug: oversized topic list should fail to deserialize");

    assert!(error.to_string().contains("length"), "{error}");
}
