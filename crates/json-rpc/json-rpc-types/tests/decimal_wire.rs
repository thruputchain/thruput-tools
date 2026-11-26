use serde_json::json;
use thruput_json_rpc_types::{
    block::ChainTip, decimal::Decimal, events::BlockAnchor, transaction::TransactionStatus,
    view::ViewRequest,
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
        let tip = json!({"height": text, "blockHash": thruput_encoding::encode(&[0x00; 32])});
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
        "contract": thruput_encoding::encode(&[0x00; 32]),
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
fn roundtrips_exact_wide_integer_boundaries() {
    for value in [0, 1, (1 << 53) - 1, 1 << 53, (1 << 53) + 1, u64::MAX] {
        let json = serde_json::to_string(&Decimal(value)).unwrap();
        assert_eq!(json, format!("\"{value}\""));
        assert_eq!(
            serde_json::from_str::<Decimal<u64>>(&json).unwrap().0,
            value
        );
    }
    for value in [i128::MIN, -1, 0, i128::MAX] {
        let json = serde_json::to_string(&Decimal(value)).unwrap();
        assert_eq!(
            serde_json::from_str::<Decimal<i128>>(&json).unwrap().0,
            value
        );
    }
}

#[test]
fn rejects_numbers_noncanonical_strings_and_overflow() {
    for json in [
        "0",
        "9007199254740993",
        "null",
        "true",
        "\"\"",
        "\"00\"",
        "\"+1\"",
        "\"-0\"",
        "\" 1\"",
        "\"1e3\"",
        "\"1.0\"",
        "\"18446744073709551616\"",
    ] {
        assert!(
            serde_json::from_str::<Decimal<u64>>(json).is_err(),
            "{json}"
        );
    }
    assert!(serde_json::from_str::<Decimal<i64>>("\"9223372036854775808\"").is_err());
    assert!(serde_json::from_str::<Decimal<i64>>("\"-9223372036854775809\"").is_err());
}
