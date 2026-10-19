//! Native projections before legacy response adapters can discard evidence.
//!
//! These functions validate decoded XDR. Generated x23 clients currently map
//! response decode failures to the same `SystemError` as remote failures, so
//! adapters cannot classify those failures as invalid data. Preserving that
//! distinction requires a generated-client change; do not infer it from text.

mod simulation;
mod state;
pub use simulation::*;
pub use state::*;

use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::{BlockHeight, error::native::BackendDataInvalidReason};

fn invalid(reason: BackendDataInvalidReason, message: &str) -> RpcServerError {
    RpcServerError::invalid_data(reason, message)
}

fn height(value: &BlockHeight) -> Result<(), RpcServerError> {
    // hogd's initial DbTip uses -1 until genesis commits.
    if **value == -1 {
        return Err(RpcServerError::BackendUnavailable(
            "no committed snapshot".into(),
        ));
    }
    if **value < 0 {
        return Err(invalid(
            BackendDataInvalidReason::InconsistentResult,
            "negative committed snapshot height",
        ));
    }
    Ok(())
}
