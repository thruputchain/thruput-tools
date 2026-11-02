pub mod blocks;
pub mod contracts;
pub mod native_history;
pub mod receipt;
pub mod view;

use std::time::{Duration, Instant};

use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};

use crate::config::HealthBackend;
use crate::metrics::{BackendRequestOutcome, GatewayMetrics};
use tracing::Instrument;

/// How the gateway names hogd's services in diagnostics.
pub(crate) const INDEXER: &str = "indexer";
pub(crate) const DB_GATEWAY: &str = "db_gateway";
pub(crate) const SCHEDULER: &str = "scheduler";
pub(crate) const VIEW_GATEWAY: &str = "view_gateway";

fn backend_deadline(
    context: RpcRequestContext,
    timeout: Duration,
) -> Result<Instant, RpcServerError> {
    let deadline = context
        .capped_from_start(timeout)
        .deadline()
        .expect("bug: backend timeout should establish a deadline");
    if deadline <= Instant::now() {
        Err(RpcServerError::BackendTimeout)
    } else {
        Ok(deadline)
    }
}

/// Run one backend RPC against `deadline` and record it as `op` on
/// `backend`.
///
/// A timeout becomes `BackendTimeout` and a transport failure
/// `BackendUnavailable`, prefixed with `service`. `is_ok` says whether a
/// value the backend did return counts as a successful request for the
/// metric; an outcome the caller's decoder will reject is still reported
/// as an error here.
pub(crate) async fn timed_backend_call<T, E: std::fmt::Display>(
    metrics: &GatewayMetrics,
    backend: HealthBackend,
    op: &'static str,
    service: &str,
    deadline: Instant,
    call: impl Future<Output = Result<T, E>>,
    is_ok: impl Fn(&T) -> bool,
) -> Result<T, RpcServerError> {
    let started_at = Instant::now();
    let result = tokio::time::timeout_at(deadline.into(), call)
        .instrument(crate::telemetry::backend_span(service, op))
        .await;
    let outcome = match &result {
        Err(_) => BackendRequestOutcome::Timeout,
        Ok(Ok(value)) if is_ok(value) => BackendRequestOutcome::Ok,
        Ok(Ok(_) | Err(_)) => BackendRequestOutcome::Error,
    };
    metrics.observe_backend_request(backend, op, outcome, started_at.elapsed());
    result
        .map_err(|_| RpcServerError::BackendTimeout)?
        .map_err(|error| RpcServerError::BackendUnavailable(format!("{service}: {error}")))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use thruput_json_rpc_server::RpcRequestContext;

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
