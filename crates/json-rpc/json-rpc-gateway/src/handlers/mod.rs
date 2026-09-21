pub mod blocks;
pub mod events;
pub mod receipt;
pub mod transactions;
pub mod view;

use std::time::{Duration, Instant};

use groundhog_json_rpc_server::RpcRequestContext;

use crate::error::GatewayError;

/// How the gateway names hogd's services in diagnostics.
pub(crate) const INDEXER: &str = "indexer";
pub(crate) const DB_GATEWAY: &str = "db_gateway";
pub(crate) const SCHEDULER: &str = "scheduler";
pub(crate) const VIEW_GATEWAY: &str = "view_gateway";

fn backend_deadline(
    context: RpcRequestContext,
    timeout: Duration,
) -> Result<Instant, GatewayError> {
    let deadline = context
        .capped_from_start(timeout)
        .deadline()
        .expect("bug: backend timeout should establish a deadline");
    if deadline <= Instant::now() {
        Err(GatewayError::BackendTimeout)
    } else {
        Ok(deadline)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use groundhog_json_rpc_server::RpcRequestContext;

    use super::backend_deadline;

    #[test]
    fn backend_deadline_is_measured_from_request_start() {
        let context = RpcRequestContext::new(Some(Duration::from_secs(10)));
        let expected = context
            .started_at()
            .checked_add(Duration::from_secs(5))
            .expect("test deadline should fit");

        assert_eq!(
            backend_deadline(context, Duration::from_secs(5)).unwrap(),
            expected
        );
    }
}
