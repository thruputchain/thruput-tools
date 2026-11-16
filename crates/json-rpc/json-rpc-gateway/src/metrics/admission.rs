use prometheus::{Gauge, IntCounterVec, Opts, Registry};

pub(crate) struct AdmissionMetrics {
    rejected: IntCounterVec,
    pub(crate) in_flight: Gauge,
    pub(crate) tracked_clients: Gauge,
    pub(crate) connections_open: Gauge,
}

impl AdmissionMetrics {
    pub(crate) fn new(registry: &Registry) -> prometheus::Result<Self> {
        let rejected = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_admission_rejected_total",
                "RPC calls rejected before handler execution.",
            ),
            &["reason"],
        )?;
        for reason in ["global", "connection", "rate_limited"] {
            rejected.with_label_values(&[reason]);
        }
        let in_flight = Gauge::new(
            "groundhog_gateway_admission_in_flight",
            "Global RPC admission permits held.",
        )?;
        let tracked_clients = Gauge::new(
            "groundhog_gateway_tracked_clients",
            "Individually tracked client buckets.",
        )?;
        let connections_open = Gauge::new(
            "groundhog_gateway_connections_open",
            "Accepted TCP sockets currently open.",
        )?;
        registry.register(Box::new(rejected.clone()))?;
        for gauge in [&in_flight, &tracked_clients, &connections_open] {
            registry.register(Box::new(gauge.clone()))?;
        }
        Ok(Self {
            rejected,
            in_flight,
            tracked_clients,
            connections_open,
        })
    }

    pub(crate) fn reject(&self, reason: &'static str) {
        self.rejected.with_label_values(&[reason]).inc();
    }
}

pub(crate) struct GaugeGuard(pub(crate) Gauge);

impl GaugeGuard {
    pub(crate) fn new(gauge: &Gauge) -> Self {
        gauge.inc();
        Self(gauge.clone())
    }
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        self.0.dec();
    }
}
