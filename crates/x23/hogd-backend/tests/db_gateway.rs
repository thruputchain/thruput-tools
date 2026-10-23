use thruput_db_gateway::DbGatewayResult;
use thruput_scheduler::RPCError;

use thruput_hogd_backend::db_gateway::*;

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
