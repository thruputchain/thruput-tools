use thruput_json_rpc_types::protocol_token::*;
use thruput_xdr::PROTOCOL_VERSION;

#[test]
fn this_builds_token_round_trips() {
    assert_eq!(PROTOCOL_VERSION.len(), PROTOCOL_COMMIT_LEN);
    assert_eq!(
        protocol_from_text(&protocol_text(&PROTOCOL_VERSION)),
        Ok(PROTOCOL_VERSION)
    );
}

#[test]
fn short_text_is_rejected() {
    let error = protocol_from_text("hogp/test").expect_err("bug: short token must not convert");
    assert_eq!(error.actual, 9);
    assert_eq!(error.value, "hogp/test");
}

#[test]
fn overlong_text_is_rejected() {
    let text = "hogp/0123456789abcdefghij";
    let error = protocol_from_text(text).expect_err("bug: overlong token must not convert");
    assert_eq!(error.actual, text.len());
}

#[test]
fn empty_text_is_rejected() {
    assert_eq!(protocol_from_text("").unwrap_err().actual, 0);
}

#[test]
fn error_names_the_offending_value_and_both_widths() {
    let message = protocol_from_text("hogp/test").unwrap_err().to_string();
    assert!(message.contains("20"), "{message}");
    assert!(message.contains("hogp/test"), "{message}");
    assert!(message.contains('9'), "{message}");
}
