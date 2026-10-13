use std::time::Duration;
use thruput_json_rpc_client::receipt::ReceiptPollConfig;

#[test]
fn budget_config_caps_attempts_at_the_spans_it_covers() {
    let config = ReceiptPollConfig::within(Duration::from_secs(1), Duration::from_millis(250));
    assert_eq!(config.max_attempts(), 5);
    assert_eq!(config.budget(), Some(Duration::from_secs(1)));
}

#[test]
fn budget_config_polls_at_least_once_without_an_interval() {
    let config = ReceiptPollConfig::within(Duration::ZERO, Duration::from_millis(250));
    assert_eq!(config.max_attempts(), 1);
}

#[test]
fn zero_interval_leaves_the_budget_to_bound_the_loop() {
    let config = ReceiptPollConfig::within(Duration::from_secs(1), Duration::ZERO);
    assert_eq!(config.max_attempts(), usize::MAX);
}

#[test]
fn attempts_config_has_no_budget() {
    let config = ReceiptPollConfig::attempts(7, Duration::from_millis(5));
    assert_eq!(config.max_attempts(), 7);
    assert_eq!(config.budget(), None);
}
