use groundhog_db_gateway::DbGatewayItemResult;
use groundhog_db_gateway::DbGatewayPayload;
use groundhog_db_gateway::DbGatewayResult;
use groundhog_json_rpc_server::RpcServerError;
use groundhog_scheduler::RPCError;
use xdr_common::VecM;

use groundhog_hogd_backend::db_gateway::*;

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
