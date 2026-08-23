use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use prometheus::{
    Encoder, Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounterVec, Opts,
    Registry, TextEncoder,
};

use groundhog_build_info::BuildVersion;
use groundhog_json_rpc_api::WIRE_FORMAT_VERSION;

use crate::{
    config::HealthBackend,
    health::{GatewayHealthSnapshot, HealthStatus},
};

const HEALTH_STATES: [HealthStatus; 4] = [
    HealthStatus::Starting,
    HealthStatus::Ok,
    HealthStatus::Degraded,
    HealthStatus::Error,
];

const BACKEND_REQUEST_DURATION_BUCKETS: [f64; 15] = [
    0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

const RPC_REQUEST_DURATION_BUCKETS: [f64; 15] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

const HTTP_REQUEST_DURATION_BUCKETS: [f64; 15] = RPC_REQUEST_DURATION_BUCKETS;

const RPC_BATCH_SIZE_BUCKETS: [f64; 10] =
    [1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0];

const INTERNAL_BATCH_SIZE_BUCKETS: [f64; 10] = RPC_BATCH_SIZE_BUCKETS;
const COALESCED_WAITER_BUCKETS: [f64; 10] = RPC_BATCH_SIZE_BUCKETS;

pub(crate) const REASON_CLOSED: &str = "closed";
pub(crate) const REASON_INGRESS_FULL: &str = "ingress_full";
pub(crate) const REASON_PARSE_FAILED: &str = "parse_failed";

#[cfg(test)]
pub(crate) const INTERNAL_BATCH_REJECTION_REASONS: [&str; 2] = [REASON_CLOSED, REASON_INGRESS_FULL];
#[cfg(test)]
pub(crate) const MALFORMED_EVENT_DROP_REASONS: [&str; 1] = [REASON_PARSE_FAILED];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendRequestOutcome {
    Ok,
    Error,
    Timeout,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendReconnectOutcome {
    Success,
    Error,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AirdropRequestOutcome {
    Pending,
    Succeeded,
    Error,
    Timeout,
}

impl AirdropRequestOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }

    const fn result(self) -> &'static str {
        match self {
            Self::Pending | Self::Succeeded => "ok",
            Self::Error | Self::Timeout => "error",
        }
    }
}

impl BackendReconnectOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }
}

impl BackendRequestOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Rejected => "rejected",
        }
    }

    const fn result(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error | Self::Timeout | Self::Rejected => "error",
        }
    }
}

/// Gateway-owned Prometheus registry and core availability metrics.
#[derive(Clone)]
pub struct GatewayMetrics {
    registry: Registry,
    gather_lock: Arc<Mutex<()>>,
    health_status: GaugeVec,
    backend_up: GaugeVec,
    chain_tip_height: Gauge,
    chain_tip_age_seconds: Gauge,
    backend_requests_total: IntCounterVec,
    backend_request_duration_seconds: HistogramVec,
    backend_reconnects_total: IntCounterVec,
    airdrop_requests_total: IntCounterVec,
    airdrop_request_duration_seconds: HistogramVec,
    rpc_requests_total: IntCounterVec,
    rpc_request_duration_seconds: HistogramVec,
    rpc_in_flight: GaugeVec,
    rpc_batch_size: Histogram,
    rpc_connections_limit: Gauge,
    http_requests_total: IntCounterVec,
    http_request_duration_seconds: HistogramVec,
    internal_batch_size: HistogramVec,
    internal_batch_rejected_total: IntCounterVec,
    internal_batch_in_flight: GaugeVec,
    coalesced_waiters: HistogramVec,
    submitted_transactions_total: IntCounterVec,
    view_timeouts_total: prometheus::IntCounter,
    malformed_events_dropped_total: IntCounterVec,
}

impl GatewayMetrics {
    /// Create a gateway-owned Prometheus registry with all v1 collectors.
    ///
    /// The registry is intentionally private to the gateway process so metric
    /// names and labels can be bounded independently of other crates.
    #[allow(clippy::too_many_lines)]
    pub fn new(
        build_version: BuildVersion,
        commit_hash: Option<&'static str>,
    ) -> prometheus::Result<Self> {
        let registry = Registry::new();
        let health_status = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_health_status",
                "Gateway readiness status as enum gauges: exactly one status is 1.",
            ),
            &["status"],
        )?;
        let backend_up = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_backend_up",
                "Whether each gateway backend is configured and currently healthy.",
            ),
            &["backend"],
        )?;
        let chain_tip_height = Gauge::with_opts(Opts::new(
            "groundhog_gateway_chain_tip_height",
            "Last successfully observed scheduler chain-tip height.",
        ))?;
        let chain_tip_age_seconds = Gauge::with_opts(Opts::new(
            "groundhog_gateway_chain_tip_age_seconds",
            "Age in seconds of the last successfully observed scheduler chain tip.",
        ))?;
        let build_info = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_build_info",
                "Gateway build metadata. Constant 1 labeled by crate version, platform version, commit hash, and wire format version.",
            ),
            &[
                "version",
                "platform_version",
                "commit_hash",
                "wire_format_version",
            ],
        )?;
        let backend_requests_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_backend_requests_total",
                "Total gateway x23 backend requests by bounded backend, operation, and outcome labels.",
            ),
            &["backend", "operation", "outcome"],
        )?;
        let backend_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_backend_request_duration_seconds",
                "Gateway x23 backend request duration in seconds by bounded backend, operation, and result labels.",
            )
            .buckets(BACKEND_REQUEST_DURATION_BUCKETS.to_vec()),
            &["backend", "operation", "result"],
        )?;
        let backend_reconnects_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_backend_reconnects_total",
                "Total gateway backend reconnect attempts by bounded backend and outcome labels.",
            ),
            &["backend", "outcome"],
        )?;
        let airdrop_requests_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_airdrop_requests_total",
                "Total airdrop backend requests by bounded backend and outcome labels.",
            ),
            &["backend", "outcome"],
        )?;
        let airdrop_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_airdrop_request_duration_seconds",
                "Airdrop backend request duration in seconds by bounded backend and result labels.",
            )
            .buckets(BACKEND_REQUEST_DURATION_BUCKETS.to_vec()),
            &["backend", "result"],
        )?;
        let rpc_requests_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_rpc_requests_total",
                "Total gateway JSON-RPC method requests by bounded method and outcome labels.",
            ),
            &["method", "outcome"],
        )?;
        let rpc_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_rpc_request_duration_seconds",
                "Gateway JSON-RPC method duration in seconds by bounded method and result labels.",
            )
            .buckets(RPC_REQUEST_DURATION_BUCKETS.to_vec()),
            &["method", "result"],
        )?;
        let rpc_in_flight = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_rpc_in_flight",
                "Current in-flight gateway JSON-RPC method requests by bounded method label.",
            ),
            &["method"],
        )?;
        let rpc_batch_size = Histogram::with_opts(
            HistogramOpts::new(
                "groundhog_gateway_rpc_batch_size",
                "Inbound JSON-RPC request batch size.",
            )
            .buckets(RPC_BATCH_SIZE_BUCKETS.to_vec()),
        )?;
        let rpc_connections_limit = Gauge::with_opts(Opts::new(
            "groundhog_gateway_rpc_connections_limit",
            "Configured maximum accepted gateway JSON-RPC transport connections.",
        ))?;
        let http_requests_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_http_requests_total",
                "Total gateway HTTP transport requests by bounded endpoint and status class labels.",
            ),
            &["endpoint", "status_class"],
        )?;
        let http_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_http_request_duration_seconds",
                "Gateway HTTP transport request duration in seconds by bounded endpoint and status class labels.",
            )
            .buckets(HTTP_REQUEST_DURATION_BUCKETS.to_vec()),
            &["endpoint", "status_class"],
        )?;
        let internal_batch_size = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_internal_batch_size",
                "Gateway internal batch size by bounded component label.",
            )
            .buckets(INTERNAL_BATCH_SIZE_BUCKETS.to_vec()),
            &["component"],
        )?;
        let internal_batch_rejected_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_internal_batch_rejected_total",
                "Total gateway internal batch rejections by bounded component and reason labels.",
            ),
            &["component", "reason"],
        )?;
        let internal_batch_in_flight = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_internal_batch_in_flight",
                "Current in-flight gateway internal batches by bounded component label.",
            ),
            &["component"],
        )?;
        let coalesced_waiters = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_coalesced_waiters",
                "Number of waiters coalesced into one backend request by bounded component label.",
            )
            .buckets(COALESCED_WAITER_BUCKETS.to_vec()),
            &["component"],
        )?;
        let submitted_transactions_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_submitted_transactions_total",
                "Total gateway transaction submissions by bounded outcome label.",
            ),
            &["outcome"],
        )?;
        let view_timeouts_total = prometheus::IntCounter::with_opts(Opts::new(
            "groundhog_gateway_view_timeouts_total",
            "Total gateway view RPCs that timed out waiting for ViewGateway.",
        ))?;
        let malformed_events_dropped_total = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_malformed_events_dropped_total",
                "Total malformed events dropped by bounded reason label.",
            ),
            &["reason"],
        )?;

        registry.register(Box::new(health_status.clone()))?;
        registry.register(Box::new(backend_up.clone()))?;
        registry.register(Box::new(chain_tip_height.clone()))?;
        registry.register(Box::new(chain_tip_age_seconds.clone()))?;
        registry.register(Box::new(build_info.clone()))?;
        registry.register(Box::new(backend_requests_total.clone()))?;
        registry.register(Box::new(backend_request_duration_seconds.clone()))?;
        registry.register(Box::new(backend_reconnects_total.clone()))?;
        registry.register(Box::new(airdrop_requests_total.clone()))?;
        registry.register(Box::new(airdrop_request_duration_seconds.clone()))?;
        registry.register(Box::new(rpc_requests_total.clone()))?;
        registry.register(Box::new(rpc_request_duration_seconds.clone()))?;
        registry.register(Box::new(rpc_in_flight.clone()))?;
        registry.register(Box::new(rpc_batch_size.clone()))?;
        registry.register(Box::new(rpc_connections_limit.clone()))?;
        registry.register(Box::new(http_requests_total.clone()))?;
        registry.register(Box::new(http_request_duration_seconds.clone()))?;
        registry.register(Box::new(internal_batch_size.clone()))?;
        registry.register(Box::new(internal_batch_rejected_total.clone()))?;
        registry.register(Box::new(internal_batch_in_flight.clone()))?;
        registry.register(Box::new(coalesced_waiters.clone()))?;
        registry.register(Box::new(submitted_transactions_total.clone()))?;
        registry.register(Box::new(view_timeouts_total.clone()))?;
        registry.register(Box::new(malformed_events_dropped_total.clone()))?;

        build_info
            .with_label_values(&[
                build_version.crate_version(),
                build_version.platform_version().unwrap_or("unknown"),
                commit_hash.unwrap_or("unknown"),
                WIRE_FORMAT_VERSION,
            ])
            .set(1.0);

        Ok(Self {
            registry,
            gather_lock: Arc::new(Mutex::new(())),
            health_status,
            backend_up,
            chain_tip_height,
            chain_tip_age_seconds,
            backend_requests_total,
            backend_request_duration_seconds,
            backend_reconnects_total,
            airdrop_requests_total,
            airdrop_request_duration_seconds,
            rpc_requests_total,
            rpc_request_duration_seconds,
            rpc_in_flight,
            rpc_batch_size,
            rpc_connections_limit,
            http_requests_total,
            http_request_duration_seconds,
            internal_batch_size,
            internal_batch_rejected_total,
            internal_batch_in_flight,
            coalesced_waiters,
            submitted_transactions_total,
            view_timeouts_total,
            malformed_events_dropped_total,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new(BuildVersion::new("test", None), None).expect("bug: test metrics should register")
    }

    pub(crate) fn observe_backend_request(
        &self,
        backend: HealthBackend,
        operation: &'static str,
        outcome: BackendRequestOutcome,
        duration: Duration,
    ) {
        self.backend_requests_total
            .with_label_values(&[backend.as_str(), operation, outcome.as_str()])
            .inc();
        self.backend_request_duration_seconds
            .with_label_values(&[backend.as_str(), operation, outcome.result()])
            .observe(duration.as_secs_f64());
    }

    pub(crate) fn observe_backend_reconnect(
        &self,
        backend: HealthBackend,
        outcome: BackendReconnectOutcome,
    ) {
        self.backend_reconnects_total
            .with_label_values(&[backend.as_str(), outcome.as_str()])
            .inc();
    }

    pub(crate) fn observe_airdrop_request(
        &self,
        backend: &'static str,
        outcome: AirdropRequestOutcome,
        duration: Duration,
    ) {
        self.airdrop_requests_total
            .with_label_values(&[backend, outcome.as_str()])
            .inc();
        self.airdrop_request_duration_seconds
            .with_label_values(&[backend, outcome.result()])
            .observe(duration.as_secs_f64());
    }

    pub(crate) fn bound_rpc_method(method: &str) -> &'static str {
        match method {
            "groundhog_sendRawTransaction" => "groundhog_sendRawTransaction",
            "groundhog_getTransactionReceipt" => "groundhog_getTransactionReceipt",
            "groundhog_getTransactionStatus" => "groundhog_getTransactionStatus",
            "groundhog_chainTip" => "groundhog_chainTip",
            "groundhog_getBlockByNumber" => "groundhog_getBlockByNumber",
            "groundhog_pendingTransactionCount" => "groundhog_pendingTransactionCount",
            "groundhog_getEvents" => "groundhog_getEvents",
            "groundhog_getTransactionsByAddress" => "groundhog_getTransactionsByAddress",
            "groundhog_getStorageAt" => "groundhog_getStorageAt",
            "groundhog_getBalance" => "groundhog_getBalance",
            "groundhog_view" => "groundhog_view",
            "groundhog_networkInfo" => "groundhog_networkInfo",
            "groundhog_requestAirdrop" => "groundhog_requestAirdrop",
            "groundhog_version" => "groundhog_version",
            _ => "unknown",
        }
    }

    pub(crate) fn observe_rpc_request(
        &self,
        method: &'static str,
        outcome: &'static str,
        duration: Duration,
    ) {
        self.rpc_requests_total
            .with_label_values(&[method, outcome])
            .inc();
        self.rpc_request_duration_seconds
            .with_label_values(&[method, result_for_outcome(outcome)])
            .observe(duration.as_secs_f64());
    }

    pub(crate) fn rpc_in_flight_guard(&self, method: &'static str) -> RpcInFlightGuard {
        let gauge = self.rpc_in_flight.with_label_values(&[method]);
        gauge.inc();
        RpcInFlightGuard { gauge }
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn observe_rpc_batch_size(&self, size: usize) {
        self.rpc_batch_size.observe(size as f64);
    }

    pub(crate) fn set_rpc_connections_limit(&self, limit: u32) {
        self.rpc_connections_limit.set(f64::from(limit));
    }

    pub(crate) fn observe_http_request(
        &self,
        endpoint: &'static str,
        status_class: &'static str,
        duration: Duration,
    ) {
        self.http_requests_total
            .with_label_values(&[endpoint, status_class])
            .inc();
        self.http_request_duration_seconds
            .with_label_values(&[endpoint, status_class])
            .observe(duration.as_secs_f64());
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn observe_internal_batch_size(&self, component: &'static str, size: usize) {
        self.internal_batch_size
            .with_label_values(&[component])
            .observe(size as f64);
    }

    pub(crate) fn observe_internal_batch_rejected(
        &self,
        component: &'static str,
        reason: &'static str,
    ) {
        self.internal_batch_rejected_total
            .with_label_values(&[component, reason])
            .inc();
    }

    pub(crate) fn internal_batch_in_flight_guard(
        &self,
        component: &'static str,
    ) -> InternalBatchInFlightGuard {
        let gauge = self
            .internal_batch_in_flight
            .with_label_values(&[component]);
        gauge.inc();
        InternalBatchInFlightGuard { gauge }
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn observe_coalesced_waiters(&self, component: &'static str, waiters: usize) {
        self.coalesced_waiters
            .with_label_values(&[component])
            .observe(waiters as f64);
    }

    pub(crate) fn observe_submitted_transaction(&self, outcome: &'static str) {
        self.submitted_transactions_total
            .with_label_values(&[outcome])
            .inc();
    }

    pub(crate) fn observe_view_timeout(&self) {
        self.view_timeouts_total.inc();
    }

    pub(crate) fn observe_malformed_event_dropped(&self, reason: &'static str) {
        self.malformed_events_dropped_total
            .with_label_values(&[reason])
            .inc();
    }

    /// Render Prometheus text exposition after refreshing snapshot-backed gauges.
    pub fn gather(&self, snapshot: &GatewayHealthSnapshot) -> prometheus::Result<String> {
        let _guard = self
            .gather_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.update_health(snapshot);
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        TextEncoder::new().encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8_lossy(&buffer).into_owned())
    }

    fn update_health(&self, snapshot: &GatewayHealthSnapshot) {
        for status in HEALTH_STATES {
            let value = if status == snapshot.status() {
                1.0
            } else {
                0.0
            };
            self.health_status
                .with_label_values(&[status.as_str()])
                .set(value);
        }

        for backend in HealthBackend::ALL {
            let value = if snapshot.backend_up(backend) {
                1.0
            } else {
                0.0
            };
            self.backend_up
                .with_label_values(&[backend.as_str()])
                .set(value);
        }

        self.chain_tip_height
            .set(snapshot.chain_tip_height().unwrap_or(f64::NAN));
        self.chain_tip_age_seconds
            .set(snapshot.chain_tip_age_seconds().unwrap_or(f64::NAN));
    }
}

pub(crate) struct RpcInFlightGuard {
    gauge: Gauge,
}

impl Drop for RpcInFlightGuard {
    fn drop(&mut self) {
        self.gauge.dec();
    }
}

pub(crate) struct InternalBatchInFlightGuard {
    gauge: Gauge,
}

impl Drop for InternalBatchInFlightGuard {
    fn drop(&mut self) {
        self.gauge.dec();
    }
}

pub(crate) fn result_for_outcome(outcome: &str) -> &'static str {
    match outcome {
        "ok" => "ok",
        _ => "error",
    }
}

pub(crate) const fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{GatewayConfig, HealthBackend},
        health::{GatewayHealth, GatewayHealthSnapshot},
    };

    #[tokio::test]
    async fn core_metrics_render_health_status_backend_and_build_info() {
        let config = GatewayConfig::default();
        let build_version = BuildVersion::new("test-version", Some("test-platform"));
        let health = GatewayHealth::new(&config, build_version, Some("test-commit"));
        let snapshot = health.snapshot().await;
        let metrics = GatewayMetrics::new(build_version, Some("test-commit"))
            .expect("metrics should register");

        let body = metrics.gather(&snapshot).expect("metrics should encode");

        assert!(body.contains("# TYPE groundhog_gateway_health_status gauge"));
        assert!(body.contains("groundhog_gateway_health_status{status=\"starting\"} 1"));
        assert!(body.contains("groundhog_gateway_health_status{status=\"ok\"} 0"));
        assert!(body.contains("groundhog_gateway_backend_up{backend=\"scheduler\"} 0"));
        assert!(body.contains("groundhog_gateway_chain_tip_height NaN"));
        assert!(body.contains("groundhog_gateway_chain_tip_age_seconds NaN"));
        assert!(body.contains(
            "groundhog_gateway_build_info{commit_hash=\"test-commit\",platform_version=\"test-platform\",version=\"test-version\""
        ));
        assert!(body.contains(&format!("wire_format_version=\"{WIRE_FORMAT_VERSION}\"")));
    }

    #[test]
    fn core_metrics_render_ok_snapshot_tip_and_backend_up() {
        let snapshot = GatewayHealthSnapshot::for_test(
            HealthStatus::Ok,
            Some((42, 1500)),
            &[
                (HealthBackend::Scheduler, true),
                (HealthBackend::Indexer, true),
                (HealthBackend::DbGateway, true),
                (HealthBackend::ViewGateway, true),
            ],
        );
        let metrics = GatewayMetrics::for_tests();

        let body = metrics.gather(&snapshot).expect("metrics should encode");

        assert!(body.contains("groundhog_gateway_health_status{status=\"starting\"} 0"));
        assert!(body.contains("groundhog_gateway_health_status{status=\"ok\"} 1"));
        assert!(body.contains("groundhog_gateway_health_status{status=\"degraded\"} 0"));
        assert!(body.contains("groundhog_gateway_health_status{status=\"error\"} 0"));
        assert!(body.contains("groundhog_gateway_backend_up{backend=\"scheduler\"} 1"));
        assert!(body.contains("groundhog_gateway_backend_up{backend=\"view_gateway\"} 1"));
        assert!(body.contains("groundhog_gateway_chain_tip_height 42"));
        assert!(body.contains("groundhog_gateway_chain_tip_age_seconds 1.5"));
    }

    #[test]
    fn backend_metrics_render_counter_and_pinned_histogram() {
        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let metrics = GatewayMetrics::for_tests();

        for outcome in [
            BackendRequestOutcome::Ok,
            BackendRequestOutcome::Error,
            BackendRequestOutcome::Timeout,
            BackendRequestOutcome::Rejected,
        ] {
            metrics.observe_backend_request(
                HealthBackend::Scheduler,
                "schedcoord_get_tip",
                outcome,
                Duration::from_millis(3),
            );
        }
        for outcome in [
            BackendReconnectOutcome::Success,
            BackendReconnectOutcome::Error,
            BackendReconnectOutcome::Timeout,
        ] {
            metrics.observe_backend_reconnect(HealthBackend::Scheduler, outcome);
        }

        let body = metrics.gather(&snapshot).expect("metrics should encode");

        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedcoord_get_tip\",outcome=\"ok\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedcoord_get_tip\",outcome=\"error\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedcoord_get_tip\",outcome=\"timeout\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_reconnects_total{backend=\"scheduler\",outcome=\"success\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_reconnects_total{backend=\"scheduler\",outcome=\"error\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_reconnects_total{backend=\"scheduler\",outcome=\"timeout\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedcoord_get_tip\",outcome=\"rejected\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_request_duration_seconds_bucket{backend=\"scheduler\",operation=\"schedcoord_get_tip\",result=\"ok\",le=\"0.005\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_request_duration_seconds_count{backend=\"scheduler\",operation=\"schedcoord_get_tip\",result=\"error\"} 3"
        ));
        let expected_buckets = [
            0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
            5.0, 10.0,
        ];
        for bucket in expected_buckets {
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_backend_request_duration_seconds_bucket{{backend=\"scheduler\",operation=\"schedcoord_get_tip\",result=\"ok\",le=\"{bucket}\"}}"
                )),
                "missing backend duration bucket {bucket}:\n{body}"
            );
        }
    }

    #[test]
    fn rpc_and_http_metrics_render_bounded_labels() {
        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let metrics = GatewayMetrics::for_tests();

        for method in [
            "groundhog_sendRawTransaction",
            "groundhog_getTransactionReceipt",
            "groundhog_getTransactionStatus",
            "groundhog_chainTip",
            "groundhog_getBlockByNumber",
            "groundhog_pendingTransactionCount",
            "groundhog_getEvents",
            "groundhog_getTransactionsByAddress",
            "groundhog_getStorageAt",
            "groundhog_getBalance",
            "groundhog_view",
            "groundhog_networkInfo",
            "groundhog_version",
        ] {
            assert_eq!(GatewayMetrics::bound_rpc_method(method), method);
        }
        assert_eq!(GatewayMetrics::bound_rpc_method("evil_custom"), "unknown");

        metrics.observe_rpc_request("groundhog_chainTip", "ok", Duration::from_millis(2));
        metrics.observe_rpc_request("unknown", "not_implemented", Duration::from_millis(3));
        metrics.observe_rpc_batch_size(5);
        metrics.set_rpc_connections_limit(2000);
        metrics.observe_http_request("jsonrpc", "2xx", Duration::from_millis(4));
        metrics.observe_http_request("readyz", "5xx", Duration::from_millis(1));

        let guard = metrics.rpc_in_flight_guard("groundhog_chainTip");
        let body = metrics.gather(&snapshot).expect("metrics should encode");
        drop(guard);

        assert!(body.contains(
            "groundhog_gateway_rpc_requests_total{method=\"groundhog_chainTip\",outcome=\"ok\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_rpc_requests_total{method=\"unknown\",outcome=\"not_implemented\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_rpc_request_duration_seconds_count{method=\"unknown\",result=\"error\"} 1"
        ));
        assert!(body.contains("groundhog_gateway_rpc_in_flight{method=\"groundhog_chainTip\"} 1"));
        assert!(body.contains("groundhog_gateway_rpc_batch_size_bucket{le=\"5\"} 1"));
        assert!(body.contains("groundhog_gateway_rpc_connections_limit 2000"));
        assert!(body.contains(
            "groundhog_gateway_http_requests_total{endpoint=\"jsonrpc\",status_class=\"2xx\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_http_requests_total{endpoint=\"readyz\",status_class=\"5xx\"} 1"
        ));

        let expected_duration_buckets = [
            0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
            60.0,
        ];
        for bucket in expected_duration_buckets {
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_rpc_request_duration_seconds_bucket{{method=\"groundhog_chainTip\",result=\"ok\",le=\"{bucket}\"}}"
                )),
                "missing RPC duration bucket {bucket}:\n{body}"
            );
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_http_request_duration_seconds_bucket{{endpoint=\"jsonrpc\",status_class=\"2xx\",le=\"{bucket}\"}}"
                )),
                "missing HTTP duration bucket {bucket}:\n{body}"
            );
        }
        for bucket in [1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0] {
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_rpc_batch_size_bucket{{le=\"{bucket}\"}}"
                )),
                "missing RPC batch size bucket {bucket}:\n{body}"
            );
        }

        let body = metrics.gather(&snapshot).expect("metrics should encode");
        assert!(body.contains("groundhog_gateway_rpc_in_flight{method=\"groundhog_chainTip\"} 0"));
    }

    #[test]
    fn internal_and_domain_metrics_render_bounded_labels() {
        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let metrics = GatewayMetrics::for_tests();

        metrics.observe_internal_batch_size("submit_batcher", 5);
        metrics.observe_internal_batch_rejected("tip_coalescer", REASON_INGRESS_FULL);
        metrics.observe_coalesced_waiters("tip_coalescer", 10);
        metrics.observe_submitted_transaction("backend_unavailable");
        metrics.observe_airdrop_request(
            "hosted",
            AirdropRequestOutcome::Pending,
            Duration::from_millis(25),
        );
        metrics.observe_view_timeout();
        metrics.observe_malformed_event_dropped(REASON_PARSE_FAILED);
        let guard = metrics.internal_batch_in_flight_guard("storage_batcher");

        let body = metrics.gather(&snapshot).expect("metrics should encode");
        drop(guard);

        assert!(body.contains(
            "groundhog_gateway_internal_batch_size_bucket{component=\"submit_batcher\",le=\"5\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_internal_batch_rejected_total{component=\"tip_coalescer\",reason=\"ingress_full\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_internal_batch_in_flight{component=\"storage_batcher\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_coalesced_waiters_bucket{component=\"tip_coalescer\",le=\"10\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_submitted_transactions_total{outcome=\"backend_unavailable\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_airdrop_requests_total{backend=\"hosted\",outcome=\"pending\"} 1"
        ));
        assert!(body.contains(
            "groundhog_gateway_airdrop_request_duration_seconds_count{backend=\"hosted\",result=\"ok\"} 1"
        ));
        assert!(body.contains("groundhog_gateway_view_timeouts_total 1"));
        assert!(body.contains(
            "groundhog_gateway_malformed_events_dropped_total{reason=\"parse_failed\"} 1"
        ));

        for bucket in [1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0] {
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_internal_batch_size_bucket{{component=\"submit_batcher\",le=\"{bucket}\"}}"
                )),
                "missing internal batch size bucket {bucket}:\n{body}"
            );
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_coalesced_waiters_bucket{{component=\"tip_coalescer\",le=\"{bucket}\"}}"
                )),
                "missing coalesced waiter bucket {bucket}:\n{body}"
            );
        }

        let body = metrics.gather(&snapshot).expect("metrics should encode");
        assert!(body.contains(
            "groundhog_gateway_internal_batch_in_flight{component=\"storage_batcher\"} 0"
        ));
    }

    #[test]
    fn reason_label_domains_are_pinned() {
        assert_eq!(
            INTERNAL_BATCH_REJECTION_REASONS,
            [REASON_CLOSED, REASON_INGRESS_FULL]
        );
        assert_eq!(MALFORMED_EVENT_DROP_REASONS, [REASON_PARSE_FAILED]);
    }
}
