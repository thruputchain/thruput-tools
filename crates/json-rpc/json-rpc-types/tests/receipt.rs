use thruput_json_rpc_types::receipt::ExecStatus;

#[test]
fn serializes_nondeterministic_error_variant() {
    let json = serde_json::to_string(&ExecStatus::NonDeterministicError)
        .expect("bug: nondeterministic status should serialize");
    assert_eq!(json, r#"{"result":"nonDeterministicError"}"#);
}
