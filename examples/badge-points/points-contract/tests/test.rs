mod common;

use points_contract::register_mock_contract;
use thruput_sdk::*;

fn setup() -> (TestEnvironmentHandle, SHA256Hash) {
    let env = TestEnvironment::no_cost();
    let contract = register_mock_contract(&env);
    let hash = *contract.as_hash();
    (env, hash)
}

#[test]
fn test_awards() {
    let (env, hash) = setup();
    common::awards_accumulate(&env, &hash);
}

#[test]
fn test_bad_awards() {
    let (env, hash) = setup();
    common::bad_awards_are_rejected(&env, &hash);
}

#[test]
fn test_award_event() {
    let (env, hash) = setup();
    common::award_emits_event(&env, &hash);
}
