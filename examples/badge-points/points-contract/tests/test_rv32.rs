mod common;

use std::path::PathBuf;
use thruput_sdk::*;

fn thruput_artifact(file_name: &str) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join("thruput")
        .join(file_name)
}

fn setup() -> (TestEnvironmentHandle, SHA256Hash) {
    let contract = load_rv32_contract(thruput_artifact("points_contract.rv32a"));
    let env = TestEnvironment::no_cost();
    let hash = env.setup_create_contract(&contract);
    (env, hash)
}

#[test]
#[ignore = "requires RV32 build — run via `thruput test`"]
fn test_awards_rv32() {
    let (env, hash) = setup();
    common::awards_accumulate(&env, &hash);
}

#[test]
#[ignore = "requires RV32 build — run via `thruput test`"]
fn test_bad_awards_rv32() {
    let (env, hash) = setup();
    common::bad_awards_are_rejected(&env, &hash);
}

#[test]
#[ignore = "requires RV32 build — run via `thruput test`"]
fn test_award_event_rv32() {
    let (env, hash) = setup();
    common::award_emits_event(&env, &hash);
}
