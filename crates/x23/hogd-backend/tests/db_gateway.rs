use thruput_db_gateway::DbGatewayItemResult;
use thruput_db_gateway::DbGatewayPayload;
use thruput_db_gateway::DbGatewayResult;
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::address_auth::{AuthPublicKey, AuthSigner};
use thruput_json_rpc_types::base58::Base58;
use thruput_scheduler::RPCError;
use thruput_xdr::address_auth::{AddressAuthData, PublicKeyConfig, PublicKeyConfigOpt};
use thruput_xdr::storage_item::WireStorageItem;
use thruput_xdr::types::{Ed25519PublicKey, PublicKey};
use xdr_common::VecM;

use thruput_hogd_backend::db_gateway::*;

#[test]
fn a_missing_key_is_a_read_at_a_known_height_not_a_failure() {
    let read = storage_item_read(
        "DbGateway",
        7.into(),
        DbGatewayItemResult::Default {
            error: RPCError::kRPCNotFound.0,
        },
    )
    .expect("a missing key is a successful read");

    assert_eq!(read.block_height, 7.into());
    assert!(read.item.is_none());
}

#[test]
fn any_other_item_error_fails_the_read() {
    let error = storage_item_read(
        "hogd-dev DbGateway",
        7.into(),
        DbGatewayItemResult::Default {
            error: RPCError::kRPCStorageWriteFailed.0,
        },
    )
    .unwrap_err();

    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
    assert!(error.to_string().contains("hogd-dev DbGateway"), "{error}");
}

#[test]
fn an_outer_batch_error_is_named_with_the_service() {
    let error = payload(
        "DbGateway",
        DbGatewayResult::Default {
            error: RPCError::kRPCNotFound.0,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("DbGateway"), "{error}");
}

#[test]
fn a_single_key_read_takes_the_first_item_and_fails_on_a_short_batch() {
    let read = single_item_read(
        "DbGateway",
        DbGatewayPayload {
            block_height: 3.into(),
            items: VecM(vec![DbGatewayItemResult::Default {
                error: RPCError::kRPCNotFound.0,
            }]),
        },
    )
    .expect("one item answers a one-key batch");
    assert_eq!(read.block_height, 3.into());
    assert!(read.item.is_none());

    let error = single_item_read(
        "hogd-dev DbGateway",
        DbGatewayPayload {
            block_height: 3.into(),
            items: VecM(vec![]),
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("fewer items"), "{error}");
    assert!(error.to_string().contains("hogd-dev DbGateway"), "{error}");
}

fn auth_data_with_slots(slots: &[(usize, u8, u32)], required_weight: u32) -> AddressAuthData {
    let mut public_keys: [PublicKeyConfigOpt; 21] =
        std::array::from_fn(|_| PublicKeyConfigOpt::new(None));
    for &(slot, byte, max_weight) in slots {
        public_keys[slot] = PublicKeyConfigOpt::new(Some(PublicKeyConfig {
            key: PublicKey::Ed25519 {
                ed25519: Ed25519PublicKey::from([byte; 32]),
            },
            max_weight,
        }));
    }
    AddressAuthData {
        public_keys,
        required_weight,
        last_written_height: 1.into(),
    }
}

fn address(byte: u8) -> ContractAddress {
    Base58(thruput_xdr::types::ContractAddress::from([byte; 32]))
}

#[test]
fn registered_auth_data_projects_occupied_slots_with_their_key_ids() {
    let read = address_auth_data_read(
        "DbGateway",
        address(4),
        9.into(),
        DbGatewayItemResult::kRPCNone {
            item: WireStorageItem::kItemTypeAuthData {
                auth_data: auth_data_with_slots(&[(0, 0xaa, 1), (5, 0xbb, 3)], 2),
            },
        },
    )
    .expect("registered auth data is a successful read");

    assert_eq!(read.address, address(4));
    assert_eq!(read.height, 9.into());
    let auth_data = read.auth_data.expect("auth data is present");
    assert_eq!(auth_data.required_weight, 2);
    assert_eq!(
        auth_data.signers,
        vec![
            AuthSigner {
                key_id: 0,
                public_key: AuthPublicKey::Ed25519 {
                    key: Base58(Ed25519PublicKey::from([0xaa; 32])),
                },
                max_weight: 1,
            },
            AuthSigner {
                key_id: 5,
                public_key: AuthPublicKey::Ed25519 {
                    key: Base58(Ed25519PublicKey::from([0xbb; 32])),
                },
                max_weight: 3,
            },
        ]
    );
}

#[test]
fn missing_auth_data_is_an_unconfigured_address_at_a_known_height() {
    let read = address_auth_data_read(
        "DbGateway",
        address(4),
        9.into(),
        DbGatewayItemResult::Default {
            error: RPCError::kRPCNotFound.0,
        },
    )
    .expect("an address without auth data is a successful read");

    assert_eq!(read.address, address(4));
    assert_eq!(read.height, 9.into());
    assert!(read.auth_data.is_none());
}

#[test]
fn auth_data_reads_fail_on_item_errors_and_foreign_item_kinds() {
    let error = address_auth_data_read(
        "hogd-dev DbGateway",
        address(4),
        9.into(),
        DbGatewayItemResult::Default {
            error: RPCError::kRPCUnrecoverable.0,
        },
    )
    .unwrap_err();
    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
    assert!(error.to_string().contains("hogd-dev DbGateway"), "{error}");

    let error = address_auth_data_read(
        "DbGateway",
        address(4),
        9.into(),
        DbGatewayItemResult::kRPCNone {
            item: WireStorageItem::kItemTypeReplayCacheData,
        },
    )
    .unwrap_err();
    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
    assert!(
        error.to_string().contains("unexpected item type"),
        "{error}"
    );
}

#[test]
fn a_single_key_batch_yields_its_item_and_snapshot_height() {
    let (height, item) = single_item(
        "DbGateway",
        DbGatewayPayload {
            block_height: 6.into(),
            items: VecM(vec![DbGatewayItemResult::Default {
                error: RPCError::kRPCNotFound.0,
            }]),
        },
    )
    .expect("one item answers a one-key batch");
    assert_eq!(height, 6.into());
    assert!(matches!(item, DbGatewayItemResult::Default { .. }));
}
