use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, UnwindSafe},
};

use points_contract::{AwardedEvent, PointsClient, PointsError, MAX_AWARD};
use thruput_sdk::*;

static NONCE: SHA256Hash = thruput_sdk::sha256_static!("POINTS_CONTRACT_NONCE");

/// A badge id as the app encodes it: the id's ASCII bytes, big-endian.
pub fn badge(id: &str) -> u64 {
    assert!(!id.is_empty() && id.len() <= 8);
    id.bytes().fold(0, |acc, byte| (acc << 8) | u64::from(byte))
}

fn deploy(env: &TestEnvironmentHandle, hash: &SHA256Hash) -> ContractAddress {
    let address = Cell::new(ContractAddress::default());
    env.transact(|tx| {
        tx.call(&ContractAddress::named("deployer"))
            .run(AssertUnwindSafe(|| {
                let deployed = deploy_contract(&NONCE, hash);
                PointsClient::new(&deployed).open();
                address.set(deployed);
            }))
    })
    .assert_success();
    env.advance_block();
    address.get()
}

fn call(
    env: &TestEnvironmentHandle,
    address: &ContractAddress,
    operation: impl FnOnce(&PointsClient) + UnwindSafe,
) -> TransactResult {
    let result = env.transact(|tx| {
        tx.call(&ContractAddress::named("caller"))
            .run(|| operation(&PointsClient::new(address)))
    });
    // Transactions in one block share a committed snapshot. Advance the block
    // so the next transaction reads this transaction's committed changes.
    env.advance_block();
    result
}

pub fn awards_accumulate(env: &TestEnvironmentHandle, hash: &SHA256Hash) {
    let address = deploy(env, hash);
    let (alice, bob, carol) = (badge("7K2Q9M"), badge("AB12CD"), badge("e1"));
    call(env, &address, |client| {
        assert_eq!(client.points(bob), 0);
        client.award(alice, bob, 3);
    })
    .assert_success();
    call(env, &address, |client| client.award(carol, bob, MAX_AWARD)).assert_success();
    call(env, &address, |client| client.award(bob, alice, 1)).assert_success();
    call(env, &address, |client| {
        assert_eq!(client.points(bob), 3 + MAX_AWARD);
        assert_eq!(client.given(bob), 1);
        assert_eq!(client.points(alice), 1);
        assert_eq!(client.given(alice), 3);
        assert_eq!(client.points(carol), 0);
        assert_eq!(client.given(carol), MAX_AWARD);
    })
    .assert_success();
}

pub fn bad_awards_are_rejected(env: &TestEnvironmentHandle, hash: &SHA256Hash) {
    let address = deploy(env, hash);
    let (alice, bob) = (badge("7K2Q9M"), badge("AB12CD"));
    let rejected = call(env, &address, |client| {
        let expect = |result: Result<(), _>, want: PointsError| {
            let err = result.expect_err("award must fail");
            match_exn!(err, {
                PointsError::SelfAward => assert!(matches!(want, PointsError::SelfAward)),
                PointsError::BadAmount => assert!(matches!(want, PointsError::BadAmount)),
                PointsError::BadBadge => assert!(matches!(want, PointsError::BadBadge)),
                _ => panic!("unexpected exception: {err:?}"),
            });
        };
        expect(client.try_award(alice, alice, 1), PointsError::SelfAward);
        expect(client.try_award(alice, bob, 0), PointsError::BadAmount);
        expect(client.try_award(alice, bob, MAX_AWARD + 1), PointsError::BadAmount);
        expect(client.try_award(0, bob, 1), PointsError::BadBadge);
        expect(client.try_award(alice, 0, 1), PointsError::BadBadge);
    });
    rejected.assert_success();
    rejected.assert_no_events();
    call(env, &address, |client| {
        assert_eq!(client.points(alice), 0);
        assert_eq!(client.points(bob), 0);
        assert_eq!(client.given(alice), 0);
    })
    .assert_success();
}

pub fn award_emits_event(env: &TestEnvironmentHandle, hash: &SHA256Hash) {
    let address = deploy(env, hash);
    let (alice, bob) = (badge("7K2Q9M"), badge("AB12CD"));
    let result = call(env, &address, |client| client.award(alice, bob, 2));
    result.assert_success();
    result.assert_event_count(1);
    let events = result.filter_events::<AwardedEvent>(&address);
    assert_eq!(events.len(), 1);
    let from: Topic = alice.into_topic().into();
    let to: Topic = bob.into_topic().into();
    assert_eq!(events[0].topic(1), Some(&from));
    assert_eq!(events[0].topic(2), Some(&to));
}
