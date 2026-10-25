use super::*;
use thruput_scheduler::RPCError;
use thruput_xdr::address_auth::{AddressAuthData, PublicKeyConfig, PublicKeyConfigOpt};
use xdr_common::VecM;

fn absent() -> DbGatewayItemResult {
    DbGatewayItemResult::Default {
        error: RPCError::kRPCNotFound.0,
    }
}
fn item(item: WireStorageItem) -> DbGatewayItemResult {
    DbGatewayItemResult::kRPCNone { item }
}
fn assert_invalid<T: std::fmt::Debug>(result: Result<T, RpcServerError>, expected: Reason) {
    assert!(
        matches!(result.expect_err("invalid backend data must fail"), RpcServerError::BackendDataInvalid { reason, .. } if reason == expected)
    );
}
#[test]
fn single_read_requires_one_answer_and_nonnegative_height() {
    for count in [0, 2] {
        assert_invalid(
            single_item(DbGatewayPayload {
                block_height: 7.into(),
                items: VecM(vec![absent(); count]),
            }),
            Reason::InvalidCardinality,
        );
    }
    assert_invalid(
        single_item(DbGatewayPayload {
            block_height: (-2).into(),
            items: VecM(vec![absent()]),
        }),
        Reason::InconsistentResult,
    );
    assert!(storage(7.into(), absent()).unwrap().item.is_none());
    // Genesis commits at height 0; only -1 means no committed snapshot yet.
    assert!(storage(0.into(), absent()).unwrap().item.is_none());
    assert!(matches!(
        storage((-1).into(), absent()),
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(
        storage(
            7.into(),
            DbGatewayItemResult::Default {
                error: RPCError::kRPCUnrecoverable.0
            }
        )
        .is_err()
    );
}
#[test]
fn negative_nonnegative_integer_is_corrupt_not_a_balance() {
    assert_invalid(
        storage(
            7.into(),
            item(WireStorageItem::kItemTypeNonnegativeInt {
                nnint: thruput_xdr::types::int128_xdr {
                    highbits: -1,
                    lowbits: 0,
                },
            }),
        ),
        Reason::InconsistentResult,
    );
}
#[test]
fn auth_keeps_slot_numbers_and_distinguishes_absence_from_wrong_type() {
    let address = Base58([3; 32].into());
    let mut slots = std::array::from_fn(|_| PublicKeyConfigOpt::new(None));
    slots[5] = PublicKeyConfigOpt::new(Some(PublicKeyConfig {
        key: PublicKey::Ed25519 {
            ed25519: [9; 32].into(),
        },
        max_weight: 3,
    }));
    let result = registered_auth(
        address,
        7.into(),
        item(WireStorageItem::kItemTypeAuthData {
            auth_data: AddressAuthData {
                public_keys: slots,
                required_weight: 2,
                last_written_height: 1.into(),
            },
        }),
    )
    .unwrap();
    let configuration = result.configuration.unwrap();
    assert_eq!(configuration.required_weight, 2);
    assert_eq!(configuration.signers.len(), 1);
    assert_eq!(configuration.signers[0].key_id, 5);
    assert_eq!(configuration.signers[0].max_weight, 3);
    assert!(
        registered_auth(address, 7.into(), absent())
            .unwrap()
            .configuration
            .is_none()
    );
    assert_invalid(
        registered_auth(
            address,
            7.into(),
            item(WireStorageItem::kItemTypeReplayCacheData),
        ),
        Reason::InconsistentResult,
    );
}

#[test]
fn pregenesis_state_is_unavailable_not_absent_or_corrupt() {
    assert!(matches!(
        single_item(DbGatewayPayload {
            block_height: (-1).into(),
            items: VecM(vec![absent()])
        }),
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(matches!(
        storage((-1).into(), absent()),
        Err(RpcServerError::BackendUnavailable(_))
    ));
}

#[test]
fn wide_nonnegative_values_and_deployment_identity_are_lossless() {
    for (highbits, lowbits, expected) in [
        (0, 0, 0),
        (1, 0, 1i128 << 64),
        (i64::MAX, u64::MAX, i128::MAX),
    ] {
        let result = storage(
            7.into(),
            item(WireStorageItem::kItemTypeNonnegativeInt {
                nnint: thruput_xdr::types::int128_xdr { highbits, lowbits },
            }),
        )
        .unwrap();
        assert!(
            matches!(result.item,Some(out::StorageItem::NonnegativeInt { value }) if value == expected)
        );
    }
    let result = storage(
        7.into(),
        item(WireStorageItem::kItemTypeDeployedContractHash {
            deployed_contract_hash: thruput_xdr::contract::DeployedContractHash {
                hash: [9; 32].into(),
                contract_size: 123,
                contract_type: thruput_xdr::contract::ContractType::kRv32,
            },
        }),
    )
    .unwrap();
    assert!(
        matches!(result.item,Some(out::StorageItem::DeployedContract { hash,size:123,contract_type }) if hash.0.as_ref() == [9;32] && contract_type == thruput_xdr::contract::ContractType::kRv32.0)
    );
}
