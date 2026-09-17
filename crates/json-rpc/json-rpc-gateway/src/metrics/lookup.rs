//! Local scheduler-lookup admission and waiting, separate from x23 call duration.
use std::time::Instant;

use prometheus::{
    Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts,
    Registry,
};

use super::BACKEND_REQUEST_DURATION_BUCKETS;

#[derive(Clone, Copy)]
pub(crate) enum LookupAdmission {
    Accepted,
    Full,
    Closed,
}

impl LookupAdmission {
    fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Full => "full",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum LookupWaitStage {
    ExecutionPermit,
    SchedulerClient,
}

impl LookupWaitStage {
    fn label(self) -> &'static str {
        match self {
            Self::ExecutionPermit => "execution_permit",
            Self::SchedulerClient => "scheduler_client",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum LookupWaitOutcome {
    Ready,
    Timeout,
    Closed,
    Cancelled,
}

impl LookupWaitOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Timeout => "timeout",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }
}

pub(crate) struct LookupMetrics {
    admission: [IntCounter; 3],
    queued: Gauge,
    executing: Gauge,
    waits: [[Histogram; 4]; 2],
}

impl LookupMetrics {
    pub(crate) fn new(registry: &Registry) -> prometheus::Result<Self> {
        let admission = IntCounterVec::new(
            Opts::new(
                "groundhog_gateway_scheduler_lookup_admission_total",
                "Scheduler lookup admission decisions before any execution wait or backend RPC.",
            ),
            &["outcome"],
        )?;
        let in_flight = GaugeVec::new(
            Opts::new(
                "groundhog_gateway_scheduler_lookup_in_flight",
                concat!(
                    "Local scheduler lookups waiting for an execution permit (queued) or holding ",
                    "one (executing, including client-lock and backend waits).",
                ),
            ),
            &["stage"],
        )?;
        let queued = in_flight.with_label_values(&["queued"]);
        let executing = in_flight.with_label_values(&["executing"]);
        let waits = HistogramVec::new(
            HistogramOpts::new(
                "groundhog_gateway_scheduler_lookup_wait_duration_seconds",
                concat!(
                    "Local scheduler lookup wait duration, including waits ended by timeout, ",
                    "closure, or cancellation. Ready means the permit or client lock was ",
                    "acquired, not that the backend is healthy.",
                ),
            )
            .buckets(BACKEND_REQUEST_DURATION_BUCKETS.to_vec()),
            &["stage", "outcome"],
        )?;
        registry.register(Box::new(admission.clone()))?;
        let admission = [
            LookupAdmission::Accepted,
            LookupAdmission::Full,
            LookupAdmission::Closed,
        ]
        .map(|outcome| admission.with_label_values(&[outcome.label()]));
        registry.register(Box::new(in_flight))?;
        registry.register(Box::new(waits.clone()))?;
        let waits = [
            LookupWaitStage::ExecutionPermit,
            LookupWaitStage::SchedulerClient,
        ]
        .map(|stage| {
            [
                LookupWaitOutcome::Ready,
                LookupWaitOutcome::Timeout,
                LookupWaitOutcome::Closed,
                LookupWaitOutcome::Cancelled,
            ]
            .map(|outcome| waits.with_label_values(&[stage.label(), outcome.label()]))
        });
        Ok(Self {
            admission,
            queued,
            executing,
            waits,
        })
    }

    pub(crate) fn admit(&self, outcome: LookupAdmission) {
        self.admission[match outcome {
            LookupAdmission::Accepted => 0,
            LookupAdmission::Full => 1,
            LookupAdmission::Closed => 2,
        }]
        .inc();
    }

    pub(crate) fn wait(&self, stage: LookupWaitStage) -> LookupWaitGuard<'_> {
        let queued = match stage {
            LookupWaitStage::ExecutionPermit => {
                self.queued.inc();
                Some(self.queued.clone())
            }
            LookupWaitStage::SchedulerClient => None,
        };
        let histograms = &self.waits[match stage {
            LookupWaitStage::ExecutionPermit => 0,
            LookupWaitStage::SchedulerClient => 1,
        }];
        LookupWaitGuard {
            started: Instant::now(),
            queued,
            histograms,
            outcome: LookupWaitOutcome::Cancelled,
        }
    }

    pub(crate) fn executing(&self) -> LookupExecutionGuard {
        self.executing.inc();
        LookupExecutionGuard(self.executing.clone())
    }
}

pub(crate) struct LookupWaitGuard<'a> {
    started: Instant,
    queued: Option<Gauge>,
    histograms: &'a [Histogram; 4],
    outcome: LookupWaitOutcome,
}

impl LookupWaitGuard<'_> {
    pub(crate) fn finish(mut self, outcome: LookupWaitOutcome) {
        self.outcome = outcome;
    }
}

impl Drop for LookupWaitGuard<'_> {
    fn drop(&mut self) {
        if let Some(queued) = &self.queued {
            queued.dec();
        }
        let index = match self.outcome {
            LookupWaitOutcome::Ready => 0,
            LookupWaitOutcome::Timeout => 1,
            LookupWaitOutcome::Closed => 2,
            LookupWaitOutcome::Cancelled => 3,
        };
        self.histograms[index].observe(self.started.elapsed().as_secs_f64());
    }
}

pub(crate) struct LookupExecutionGuard(Gauge);
impl Drop for LookupExecutionGuard {
    fn drop(&mut self) {
        self.0.dec();
    }
}
