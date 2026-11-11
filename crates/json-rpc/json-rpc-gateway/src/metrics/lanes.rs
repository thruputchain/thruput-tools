//! Bounded labels and registry ownership for direct backend lanes.
use prometheus::{
    Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts,
    Registry,
};

pub(crate) struct LaneMetrics {
    size: GaugeVec,
    permits: GaugeVec,
    wait: HistogramVec,
    rejected: IntCounterVec,
}

#[derive(Clone)]
pub(crate) struct LaneStateMetrics {
    pub held: Gauge,
    pub abandoned: Gauge,
    pub wait: Histogram,
    pub rejected: IntCounter,
}

impl LaneMetrics {
    pub fn new(registry: &Registry) -> prometheus::Result<Self> {
        let size = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_lane_size",
                "Configured backend lane capacity.",
            ),
            &["lane"],
        )?;
        let permits = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_lane_permits",
                "Backend lane permits held and abandoned by callers.",
            ),
            &["lane", "state"],
        )?;
        let wait = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_lane_wait_duration_seconds",
                "Time spent acquiring backend lane permits.",
            )
            .buckets(super::BACKEND_REQUEST_DURATION_BUCKETS.to_vec()),
            &["lane"],
        )?;
        let rejected = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_lane_rejected_total",
                "Backend lane acquisition wait expiries.",
            ),
            &["lane"],
        )?;
        registry.register(Box::new(size.clone()))?;
        registry.register(Box::new(permits.clone()))?;
        registry.register(Box::new(wait.clone()))?;
        registry.register(Box::new(rejected.clone()))?;
        Ok(Self {
            size,
            permits,
            wait,
            rejected,
        })
    }

    pub fn lane(&self, name: &'static str, size: u32) -> LaneStateMetrics {
        self.size.with_label_values(&[name]).set(f64::from(size));
        LaneStateMetrics {
            held: self.permits.with_label_values(&[name, "held"]),
            abandoned: self.permits.with_label_values(&[name, "abandoned"]),
            wait: self.wait.with_label_values(&[name]),
            rejected: self.rejected.with_label_values(&[name]),
        }
    }
}
