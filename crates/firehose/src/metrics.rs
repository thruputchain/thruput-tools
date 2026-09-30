//! Private Prometheus registry and health state for this process.
use prometheus::{
    Encoder, Gauge, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Process metrics shared with the HTTP server.
#[derive(Clone, Debug)]
pub struct Metrics {
    registry: Registry,
    /// Blocks durably added during this process lifetime.
    pub blocks: IntCounter,
    /// Full serialized block bytes added during this process lifetime.
    pub bytes: IntCounter,
    /// Events in each newly ingested block.
    pub events: Histogram,
    /// Highest durable contiguous height.
    pub height: IntGauge,
    /// Lag from an independently sampled scheduler tip; NaN when unknown.
    pub lag: Gauge,
    /// Age of serve's latest record; NaN before the first record.
    pub tip_age: Gauge,
    health: IntGauge,
    /// End-to-end pull duration, including parked time.
    pub pull: Histogram,
    /// Accumulated parked seconds including the currently outstanding pull.
    pub parked: Gauge,
    /// Logical durable archive bytes.
    pub store: IntGauge,
    /// Filesystem allocation occupied by archive records (Unix block accounting).
    pub allocated: IntGauge,
    /// Every returned protocol outcome, including errors.
    pub outcomes: IntCounterVec,
    /// Whether ingestion has a healthy backend connection.
    healthy: Arc<AtomicBool>,
}

impl Metrics {
    /// Construct an independent registry without global registration.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let blocks = IntCounter::new("firehose_blocks_ingested_total", "Durably ingested blocks")?;
        let bytes = IntCounter::new("firehose_bytes_ingested_total", "XDR bytes ingested")?;
        let events = Histogram::with_opts(
            HistogramOpts::new("firehose_events_per_block", "Events per committed block")
                .buckets(vec![0., 1., 10., 100., 1000., 10_000., 100_000.]),
        )?;
        let height = IntGauge::new("firehose_current_height", "Durable contiguous height")?;
        let lag = Gauge::new(
            "firehose_tip_lag_blocks",
            "Sampled scheduler tip minus durable height; NaN if unavailable",
        )?;
        lag.set(f64::NAN);
        let tip_age = Gauge::new(
            "firehose_tip_age_seconds",
            "Age of the latest archive record",
        )?;
        tip_age.set(f64::NAN);
        let health = IntGauge::new("firehose_healthy", "Whether this process is healthy")?;
        let pull = Histogram::with_opts(
            HistogramOpts::new("firehose_pull_seconds", "Pull latency including parking")
                .buckets(vec![0.001, 0.01, 0.1, 1., 5., 30., 60., 300.]),
        )?;
        let parked = Gauge::new(
            "firehose_parked_seconds_total",
            "Time in explicitly parked pulls",
        )?;
        let store = IntGauge::new(
            "firehose_store_bytes",
            "Logical bytes in durable archive records",
        )?;
        let allocated = IntGauge::new(
            "firehose_store_allocated_bytes",
            "Allocated record bytes, including filesystem block rounding on Unix",
        )?;
        let outcomes = IntCounterVec::new(
            Opts::new("firehose_pull_outcomes_total", "Pull outcomes"),
            &["outcome"],
        )?;
        for label in [
            "block",
            "replay",
            "caught_up",
            "not_ready",
            "mismatched_protocol",
            "progress_conflict",
            "internal_failure",
            "unknown",
            "transport_error",
        ] {
            outcomes.with_label_values(&[label]);
        }
        for metric in [
            Box::new(blocks.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(bytes.clone()),
            Box::new(events.clone()),
            Box::new(height.clone()),
            Box::new(lag.clone()),
            Box::new(tip_age.clone()),
            Box::new(health.clone()),
            Box::new(pull.clone()),
            Box::new(parked.clone()),
            Box::new(store.clone()),
            Box::new(allocated.clone()),
            Box::new(outcomes.clone()),
        ] {
            registry.register(metric)?;
        }
        Ok(Self {
            registry,
            blocks,
            bytes,
            events,
            height,
            lag,
            tip_age,
            health,
            pull,
            parked,
            store,
            allocated,
            outcomes,
            healthy: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Render Prometheus text exposition.
    pub fn render(&self) -> Result<Vec<u8>, prometheus::Error> {
        let mut output = Vec::new();
        TextEncoder::new().encode(&self.registry.gather(), &mut output)?;
        Ok(output)
    }

    /// Mark the ingest connection healthy or degraded.
    pub fn set_healthy(&self, healthy: bool) {
        self.health.set(i64::from(healthy));
        self.healthy.store(healthy, Ordering::Relaxed);
    }

    /// Whether ingestion currently has a working backend connection.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }
}
