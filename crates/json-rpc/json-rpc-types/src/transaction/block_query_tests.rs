use super::*;
use serde_json::json;

#[test]
fn block_query_preserves_full_height_and_opaque_bytes() {
    let json =
        json!({"height":"9223372036854775807","order":"ascending","cursor":"0x00ff80","limit":200});
    let query: BlockTransactionQuery = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(*query.height, i64::MAX);
    assert_eq!(query.cursor.0, [0, 255, 128]);
    assert_eq!(serde_json::to_value(&query).unwrap(), json);
    let wire: thruput_indexer::BlockTransactionQuery = query.into();
    assert_eq!(*wire.height, i64::MAX);
}

#[test]
fn query_rejects_unknown_fields_noncanonical_heights_and_invalid_order() {
    let valid = json!({"height":"1","order":"ascending","cursor":"0x","limit":1});
    for height in [
        json!(1),
        json!("01"),
        json!("+1"),
        json!("9223372036854775808"),
    ] {
        let mut bad = valid.clone();
        bad["height"] = height;
        assert!(serde_json::from_value::<BlockTransactionQuery>(bad).is_err());
    }
    for (field, value) in [
        ("unknown", json!(true)),
        ("order", json!("forward")),
        ("limit", json!(-1)),
        ("cursor", json!("0xabc")),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        assert!(serde_json::from_value::<BlockTransactionQuery>(bad).is_err());
    }
}
