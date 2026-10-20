use super::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn accepts<T: DeserializeOwned>(value: Value) {
    let diagnostic = value.to_string();
    assert!(serde_json::from_value::<T>(value).is_ok(), "{diagnostic}");
}

fn rejects<T: DeserializeOwned>(value: Value) {
    let diagnostic = value.to_string();
    assert!(serde_json::from_value::<T>(value).is_err(), "{diagnostic}");
}

fn address() -> String {
    "1".repeat(32)
}

fn invocation() -> Value {
    json!({"address": address(), "entrypoint": "0x0102030405060708", "calldata": "0x"})
}

#[test]
fn height_and_decimal_boundaries() {
    for height in ["0", "812", "9007199254740993", "9223372036854775807"] {
        accepts::<BlockQuery>(json!({"blockHeight": height}));
    }
    for height in [
        "-1",
        "-0",
        "+1",
        "01",
        "1.0",
        "1e3",
        " 1",
        "1\n",
        "9223372036854775808",
    ] {
        rejects::<BlockQuery>(json!({"blockHeight": height}));
    }
    for value in [
        json!({}),
        json!({"blockHeight": null}),
        json!({"blockHeight": 812}),
        json!({"blockHeight": "1", "extra": true}),
    ] {
        rejects::<BlockQuery>(value);
    }
}

#[test]
fn simulation_rejects_aliases_nulls_and_nested_unknowns() {
    accepts::<SimulateInvocationRequest>(invocation());
    for field in ["caller", "assumedAuth", "gasLimit", "minStateBlockHeight"] {
        let mut request = invocation();
        request[field] = Value::Null;
        rejects::<SimulateInvocationRequest>(request);
    }
    for text in [
        "0X0102030405060708",
        "0102030405060708",
        "0xAA02030405060708",
        "0x1",
        "0x",
    ] {
        let mut request = invocation();
        request["entrypoint"] = json!(text);
        rejects::<SimulateInvocationRequest>(request);
    }
    for gas in ["0", "01", "18446744073709551616"] {
        let mut request = invocation();
        request["gasLimit"] = json!(gas);
        rejects::<SimulateInvocationRequest>(request);
    }
    let mut request = invocation();
    request["assumedAuth"] = json!({"kind":"pubkeySignature", "pubkeyHash":address(), "accountNonce":"18446744073709551615"});
    accepts::<SimulateInvocationRequest>(request.clone());
    request["assumedAuth"]["nonce"] = json!("1");
    rejects::<SimulateInvocationRequest>(request);
    rejects::<SimulateInvocationsRequest>(json!({"invocations":[]}));
    let mut entry = invocation();
    entry["minStateBlockHeight"] = json!("1");
    rejects::<SimulateInvocationsRequest>(json!({"invocations":[entry]}));
}

#[test]
fn history_requires_exclusive_start_or_cursor_and_strict_anchors() {
    let initial =
        json!({"address":address(),"start":{"order":"ascending","anchor":{"kind":"atBeginning"}}});
    accepts::<RootAddressQuery>(initial.clone());
    for anchor in [
        json!({"kind":"atEnd"}),
        json!({"kind":"atBeginning","blockHeight":"1"}),
        json!({"kind":"atHeight","height":"1"}),
    ] {
        let mut request = initial.clone();
        request["start"]["anchor"] = anchor;
        rejects::<RootAddressQuery>(request);
    }
    let mut both = initial.clone();
    both["cursor"] = json!("0xaa");
    rejects::<RootAddressQuery>(both);
    rejects::<RootAddressQuery>(json!({"address":address()}));
    for cursor in [json!("0x"), Value::Null, json!("0xA0"), json!("0x1")] {
        rejects::<RootAddressQuery>(json!({"address":address(),"cursor":cursor}));
    }
    for limit in [json!(0), Value::Null, json!("1")] {
        let mut request = initial.clone();
        request["limit"] = limit;
        rejects::<RootAddressQuery>(request);
    }
    accepts::<BlockPageRequest>(json!({"blockHeight":"0","start":{"order":"descending"}}));
    rejects::<BlockPageRequest>(
        json!({"blockHeight":"0","start":{"order":"descending","anchor":{"kind":"atEnd"}}}),
    );
}

#[test]
fn event_bounds_and_wildcards_are_strict() {
    let initial = json!({"filters":[{"addresses":[address()]}],"cursor":"0xaa"});
    accepts::<EventQuery>(initial.clone());
    for topics in [
        json!({}),
        json!({"topic0":[]}),
        json!({"topic2":[format!("0x{}", "00".repeat(32))]}),
    ] {
        let mut request = initial.clone();
        request["filters"][0]["topics"] = topics;
        accepts::<EventQuery>(request);
    }
    for topics in [
        Value::Null,
        json!({"topic0":null}),
        json!({"topic3":[]}),
        json!({"topic0":["0x00"]}),
    ] {
        let mut request = initial.clone();
        request["filters"][0]["topics"] = topics;
        rejects::<EventQuery>(request);
    }
    let mut request = initial.clone();
    request["filters"][0]["addresses"] = json!([]);
    rejects::<EventQuery>(request);
    let mut request = initial.clone();
    request["filters"][0]["addresses"] = json!(vec![address(); 6]);
    rejects::<EventQuery>(request);
    let mut request = initial;
    request["filters"] = json!([]);
    rejects::<EventQuery>(request);
}

#[test]
fn nested_requests_reject_positional_sequences() {
    // A normal serde struct visitor accepts these positional shapes even with
    // deny_unknown_fields. Native objects must remain maps at every depth.
    rejects::<Invocation>(json!([address(), "0x0102030405060708", "0x"]));
    rejects::<SimulateInvocationsRequest>(
        json!({"invocations":[[address(), "0x0102030405060708", "0x"]]}),
    );
    rejects::<BlockStart>(json!(["ascending"]));
    rejects::<BlockPageRequest>(json!({"blockHeight":"1", "start":["ascending"]}));
    rejects::<EventFilter>(json!([[address()]]));
    rejects::<TopicFilter>(json!([]));
    rejects::<EventQuery>(json!({"filters":[[[address()]]],"cursor":"0xaa"}));
    rejects::<EventQuery>(
        json!({"filters":[{"addresses":[address()],"topics":[]}],"cursor":"0xaa"}),
    );
    rejects::<AssumedAuth>(json!(["sourceAddress", 1, 1]));
    rejects::<AscendingAnchor>(json!(["atBeginning"]));
    rejects::<DescendingAnchor>(json!(["atHeight", "1"]));
    rejects::<HistoryStart>(json!(["ascending", {"kind":"atBeginning"}]));
}
