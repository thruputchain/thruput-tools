use serde_json::json;
use thruput_json_rpc_types::{
    block::ChainTip, events::BlockAnchor, server_limits::ServerLimits,
    transaction::TransactionStatus, view::ViewRequest,
};

#[test]
fn protocol_heights_roundtrip_at_precision_and_signed_boundaries() {
    for height in [
        i64::MIN,
        -1,
        0,
        (1 << 53) - 1,
        1 << 53,
        (1 << 53) + 1,
        i64::MAX,
    ] {
        let text = height.to_string();
        let tip = json!({"height": text, "blockHash": format!("0x{}", "00".repeat(32))});
        let decoded: ChainTip = serde_json::from_str(&tip.to_string()).unwrap();
        assert_eq!(*decoded.height, height);
        assert_eq!(serde_json::to_value(decoded).unwrap(), tip);

        let anchor = json!({"kind": "atHeight", "height": text});
        let decoded: BlockAnchor = serde_json::from_str(&anchor.to_string()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), anchor);

        let status = json!({"status": "committed", "height": text});
        let decoded: TransactionStatus = serde_json::from_str(&status.to_string()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), status);
    }
}

#[test]
fn optional_gas_is_lossless_and_numeric_json_is_rejected() {
    let request = json!({
        "contract": format!("0x{}", "00".repeat(32)),
        "entrypoint": "0x0000000000000000", "calldata": "0x"
    });
    let decoded: ViewRequest = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(decoded.gas_limit, None);
    for value in [json!(null), json!("0"), json!(u64::MAX.to_string())] {
        let mut wire = request.clone();
        wire["gasLimit"] = value;
        let decoded: ViewRequest = serde_json::from_value(wire.clone()).unwrap();
        if !wire["gasLimit"].is_null() {
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
        }
    }
    for value in [
        json!(0),
        json!(42),
        json!(u64::MAX),
        json!("01"),
        json!("1\n"),
        json!("18446744073709551616"),
    ] {
        let mut wire = request.clone();
        wire["gasLimit"] = value.clone();
        assert!(
            serde_json::from_value::<ViewRequest>(wire).is_err(),
            "{value}"
        );
        assert!(
            serde_json::from_value::<BlockAnchor>(json!({"kind": "atHeight", "height": value}))
                .is_err()
        );
    }
}

#[test]
fn wide_limits_are_strings_while_page_limits_stay_numbers() {
    let limits = ServerLimits::new(u64::MAX, u64::MAX, u32::MAX, u32::MAX, u64::MAX);
    let wire = serde_json::to_value(limits).unwrap();
    assert_eq!(wire["maxViewGas"], u64::MAX.to_string());
    assert_eq!(wire["maxViewCalldataBytes"], u64::MAX.to_string());
    assert_eq!(wire["maxCursorBytes"], u64::MAX.to_string());
    assert_eq!(wire["maxEventsPerPage"], u32::MAX);
    assert_eq!(wire["maxTransactionsPerPage"], u32::MAX);
    assert_eq!(
        serde_json::from_value::<ServerLimits>(wire).unwrap(),
        limits
    );
}
