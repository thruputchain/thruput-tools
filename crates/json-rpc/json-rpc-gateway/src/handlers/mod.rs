pub mod blocks;
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
/// metric. Decoding and validation happen after the backend task completes.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn timed_backend_call<T: Send + 'static, E: std::fmt::Display + Send + 'static>(
    metrics: &GatewayMetrics,
    lane: &crate::lanes::Lane,
    permits: u32,
    backend: HealthBackend,
    op: &'static str,
    service: &str,
    deadline: Instant,
    call: impl Future<Output = Result<T, E>> + Send + 'static,
    is_ok: impl Fn(&T) -> bool + Send + 'static,
) -> Result<T, RpcServerError> {
    let permits = permits.clamp(1, lane.size);
    let waiting = lane.metrics.wait.start_timer();
    let permit = tokio::time::timeout_at(
        (Instant::now() + lane.wait).min(deadline).into(),
        lane.permits.clone().acquire_many_owned(permits),
    )
    .await;
    drop(waiting);
    let permit = match permit {
        Ok(Ok(permit)) => permit,
        Err(_) => {
            lane.metrics.rejected.inc();
            return Err(RpcServerError::Overloaded(format!(
                "{} lane wait expired",
                lane.name
            )));
        }
        Ok(Err(_)) => {
            return Err(RpcServerError::BackendUnavailable(format!(
                "{} lane closed",
                lane.name
            )));
        }
    };
    let (caller, task) = crate::lanes::CallerGuard::new(lane, permit, permits);
    let (send, receive) = tokio::sync::oneshot::channel();
    let metrics = metrics.clone();
    let service = service.to_owned();
    // Spawned tasks do not inherit the current span; create the backend span
    // here so it stays a child of the request span.
    let span = crate::telemetry::backend_span(&service, op);
    tokio::spawn(async move {
        let task = task;
        let started_at = Instant::now();
        let result = tokio::time::timeout_at(deadline.into(), call)
            .instrument(span)
            .await;
        let outcome = match &result {
            Err(_) => BackendRequestOutcome::Timeout,
            Ok(Ok(value)) if is_ok(value) => BackendRequestOutcome::Ok,
            Ok(Ok(_) | Err(_)) => BackendRequestOutcome::Error,
        };
        metrics.observe_backend_request(backend, op, outcome, started_at.elapsed());
        let result = match result {
            Err(_) => Err(RpcServerError::BackendTimeout),
            Ok(result) => result
                .map_err(|error| RpcServerError::BackendUnavailable(format!("{service}: {error}"))),
        };
        drop(task);
        let _ = send.send(result);
    });
    let result = receive.await.map_err(|_| {
        RpcServerError::BackendUnavailable("backend task ended without a result".into())
    });
    drop(caller);
    result?
}

#[cfg(test)]
// These gauges contain only exactly representable integer permit counts.
#[allow(clippy::float_cmp)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use thruput_json_rpc_server::RpcRequestContext;

    use super::backend_deadline;
    use super::*;
    use crate::{config::AdmissionConfig, lanes::BackendLanes};
    use tokio::sync::Notify;

    fn lanes(size: u32) -> (Arc<BackendLanes>, GatewayMetrics) {
        let config = AdmissionConfig {
            lane_wait_ms: 20.try_into().unwrap(),
            view_lane: size.try_into().unwrap(),
            ..AdmissionConfig::default()
        };
        let metrics = GatewayMetrics::for_tests();
        (Arc::new(BackendLanes::new(&config, &metrics)), metrics)
    }

    #[allow(clippy::too_many_arguments)]
    fn gated_call(
        lanes: Arc<BackendLanes>,
        metrics: GatewayMetrics,
        count: u32,
        gate: Arc<Notify>,
        started: Arc<Notify>,
        seen: Arc<AtomicUsize>,
        timeout: Duration,
    ) -> tokio::task::JoinHandle<Result<(), RpcServerError>> {
        tokio::spawn(async move {
            timed_backend_call(
                &metrics,
                &lanes.view,
                count,
                HealthBackend::ViewGateway,
                "viewgateway_view_batch",
                VIEW_GATEWAY,
                Instant::now() + timeout,
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    gate.notified().await;
                    Ok::<(), String>(())
                },
                |()| true,
            )
            .await
        })
    }

    #[tokio::test]
    async fn saturated_lane_rejects_without_polling_backend() {
        let (lanes, metrics) = lanes(2);
        let gate = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());
        let seen = Arc::new(AtomicUsize::new(0));
        let first = gated_call(
            lanes.clone(),
            metrics.clone(),
            1,
            gate.clone(),
            started.clone(),
            seen.clone(),
            Duration::from_secs(5),
        );
        started.notified().await;
        let second = gated_call(
            lanes.clone(),
            metrics.clone(),
            1,
            gate.clone(),
            started.clone(),
            seen.clone(),
            Duration::from_secs(5),
        );
        started.notified().await;
        let third = gated_call(
            lanes.clone(),
            metrics.clone(),
            1,
            gate.clone(),
            started,
            seen.clone(),
            Duration::from_secs(5),
        );
        assert!(matches!(
            third.await.unwrap(),
            Err(RpcServerError::Overloaded(_))
        ));
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(lanes.view.metrics.held.get(), 2.0);
        assert_eq!(lanes.view.metrics.rejected.get(), 1);
        let snapshot = crate::health::GatewayHealthSnapshot::for_test(
            crate::health::HealthStatus::Starting,
            None,
            &[],
        );
        assert!(
            !metrics
                .gather(&snapshot)
                .unwrap()
                .contains("groundhog_gateway_backend_requests_total{backend=\"view_gateway\"")
        );
        gate.notify_waiters();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(lanes.view.metrics.held.get(), 0.0);
    }

    #[tokio::test]
    async fn abandoned_caller_keeps_permit_until_backend_finishes() {
        let (lanes, metrics) = lanes(1);
        let gate = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());
        let seen = Arc::new(AtomicUsize::new(0));
        let caller = gated_call(
            lanes.clone(),
            metrics.clone(),
            1,
            gate.clone(),
            started.clone(),
            seen.clone(),
            Duration::from_secs(5),
        );
        started.notified().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(lanes.view.permits.available_permits(), 0);
        assert_eq!(lanes.view.metrics.held.get(), 1.0);
        assert_eq!(lanes.view.metrics.abandoned.get(), 1.0);
        gate.notify_one();
        let permit = tokio::time::timeout(
            Duration::from_secs(1),
            lanes.view.permits.clone().acquire_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(lanes.view.metrics.held.get(), 0.0);
        assert_eq!(lanes.view.metrics.abandoned.get(), 0.0);
        drop(permit);
        let next = gated_call(
            lanes.clone(),
            metrics,
            1,
            gate.clone(),
            started.clone(),
            seen,
            Duration::from_secs(5),
        );
        started.notified().await;
        gate.notify_one();
        next.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn running_deadline_releases_permit() {
        let (lanes, metrics) = lanes(1);
        let caller = gated_call(
            lanes.clone(),
            metrics,
            1,
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            Arc::new(AtomicUsize::new(0)),
            Duration::from_millis(30),
        );
        assert!(matches!(
            caller.await.unwrap(),
            Err(RpcServerError::BackendTimeout)
        ));
        assert_eq!(lanes.view.permits.available_permits(), 1);
        assert_eq!(lanes.view.metrics.held.get(), 0.0);
        assert_eq!(lanes.view.metrics.abandoned.get(), 0.0);
    }

    #[tokio::test]
    async fn queued_deadline_and_cancellation_do_not_leak_permits() {
        let (mut lanes, metrics) = lanes(1);
        // The request deadline must cap a much longer lane wait.
        Arc::get_mut(&mut lanes).unwrap().view.wait = Duration::from_secs(5);
        let held = lanes.view.permits.clone().acquire_owned().await.unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let caller = gated_call(
            lanes.clone(),
            metrics.clone(),
            1,
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            seen.clone(),
            Duration::from_millis(20),
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), caller)
                .await
                .unwrap()
                .unwrap(),
            Err(RpcServerError::Overloaded(_))
        ));
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        assert_eq!(lanes.view.metrics.rejected.get(), 1);
        let caller = gated_call(
            lanes.clone(),
            metrics,
            1,
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            seen.clone(),
            Duration::from_secs(5),
        );
        tokio::task::yield_now().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(held);
        assert_eq!(lanes.view.permits.available_permits(), 1);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        assert_eq!(lanes.view.metrics.held.get(), 0.0);
        assert_eq!(lanes.view.metrics.abandoned.get(), 0.0);
    }

    #[tokio::test]
    async fn batch_weight_is_clamped_to_lane_capacity() {
        for (requested, held) in [(2, 2_u32), (9, 3), (0, 1)] {
            let (lanes, metrics) = lanes(3);
            let gate = Arc::new(Notify::new());
            let started = Arc::new(Notify::new());
            let caller = gated_call(
                lanes.clone(),
                metrics,
                requested,
                gate.clone(),
                started.clone(),
                Arc::new(AtomicUsize::new(0)),
                Duration::from_secs(5),
            );
            started.notified().await;
            assert_eq!(lanes.view.metrics.held.get(), f64::from(held));
            assert_eq!(lanes.view.permits.available_permits(), (3 - held) as usize);
            gate.notify_one();
            caller.await.unwrap().unwrap();
            assert_eq!(lanes.view.metrics.held.get(), 0.0);
        }
    }

    #[tokio::test]
    async fn backend_panic_releases_permit_and_reports_unavailable() {
        let (lanes, metrics) = lanes(1);
        let result = timed_backend_call(
            &metrics,
            &lanes.view,
            1,
            HealthBackend::ViewGateway,
            "viewgateway_view",
            VIEW_GATEWAY,
            Instant::now() + Duration::from_secs(1),
            async {
                panic!("test backend panic");
                #[allow(unreachable_code)]
                Ok::<(), String>(())
            },
            |()| true,
        )
        .await;
        assert!(matches!(result, Err(RpcServerError::BackendUnavailable(_))));
        assert_eq!(lanes.view.permits.available_permits(), 1);
        assert_eq!(lanes.view.metrics.held.get(), 0.0);
        assert_eq!(lanes.view.metrics.abandoned.get(), 0.0);
    }

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
