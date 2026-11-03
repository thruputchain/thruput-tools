//! Permits belong to backend tasks, independently of their callers.
use crate::{
    config::AdmissionConfig,
    metrics::{GatewayMetrics, lanes::LaneStateMetrics},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct Lane {
    pub name: &'static str,
    pub size: u32,
    pub permits: Arc<Semaphore>,
    pub wait: Duration,
    pub metrics: LaneStateMetrics,
}

pub(crate) struct BackendLanes {
    pub indexer_point: Lane,
    pub indexer_scan: Lane,
    pub view: Lane,
}

impl BackendLanes {
    pub fn new(config: &AdmissionConfig, metrics: &GatewayMetrics) -> Self {
        let lane = |name, size| Lane {
            name,
            size,
            permits: Arc::new(Semaphore::new(size as usize)),
            wait: Duration::from_millis(u64::from(config.lane_wait_ms.get())),
            metrics: metrics.lanes().lane(name, size),
        };
        Self {
            indexer_point: lane("indexer_point", config.indexer_point_lane.get()),
            indexer_scan: lane("indexer_scan", config.indexer_scan_lane.get()),
            view: lane("view", config.view_lane.get()),
        }
    }
}

const RUNNING: u8 = 0;
const CALLER_GONE: u8 = 1;
const DONE: u8 = 2;

struct CallState {
    state: AtomicU8,
    transition: Mutex<()>,
    metrics: LaneStateMetrics,
    count: f64,
}

pub(crate) struct CallerGuard(Arc<CallState>);

pub(crate) struct TaskGuard {
    state: Arc<CallState>,
    _permit: OwnedSemaphorePermit,
}

impl CallerGuard {
    pub fn new(lane: &Lane, permit: OwnedSemaphorePermit, count: u32) -> (Self, TaskGuard) {
        lane.metrics.held.add(f64::from(count));
        let state = Arc::new(CallState {
            state: AtomicU8::new(RUNNING),
            transition: Mutex::new(()),
            metrics: lane.metrics.clone(),
            count: f64::from(count),
        });
        (
            Self(state.clone()),
            TaskGuard {
                state,
                _permit: permit,
            },
        )
    }
}

impl Drop for CallerGuard {
    fn drop(&mut self) {
        // Serialize the metric update with completion so scrapes cannot see a
        // decrement before the matching increment in the drop/finish race.
        let state = &self.0;
        let _transition = state
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .state
            .compare_exchange(RUNNING, CALLER_GONE, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            state.metrics.abandoned.add(state.count);
        }
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let _transition = self
            .state
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.state.state.swap(DONE, Ordering::SeqCst) == CALLER_GONE {
            self.state.metrics.abandoned.sub(self.state.count);
        }
        self.state.metrics.held.sub(self.state.count);
    }
}

#[cfg(test)]
// These gauges contain only exactly representable integer permit counts.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abandonment_transitions_balance_in_both_orders() {
        for caller_first in [true, false] {
            let metrics = GatewayMetrics::for_tests();
            let lanes = BackendLanes::new(&AdmissionConfig::default(), &metrics);
            let lane = &lanes.view;
            let permit = lane.permits.clone().acquire_many_owned(2).await.unwrap();
            let (caller, task) = CallerGuard::new(lane, permit, 2);
            if caller_first {
                drop(caller);
                assert_eq!(lane.metrics.abandoned.get(), 2.0);
                drop(task);
            } else {
                drop(task);
                assert_eq!(lane.metrics.abandoned.get(), 0.0);
                drop(caller);
            }
            assert_eq!(lane.metrics.abandoned.get(), 0.0);
            assert_eq!(lane.metrics.held.get(), 0.0);
            assert_eq!(lane.permits.available_permits(), lane.size as usize);
        }
    }

    #[test]
    fn metric_registry_exposes_all_lanes_and_bounded_states() {
        let metrics = GatewayMetrics::for_tests();
        let _lanes = BackendLanes::new(&AdmissionConfig::default(), &metrics);
        let snapshot = crate::health::GatewayHealthSnapshot::for_test(
            crate::health::HealthStatus::Starting,
            None,
            &[],
        );
        let body = metrics.gather(&snapshot).unwrap();
        for (lane, size) in [("indexer_point", 64), ("indexer_scan", 8), ("view", 16)] {
            assert!(
                body.contains(&format!(
                    "groundhog_gateway_lane_size{{lane=\"{lane}\"}} {size}"
                )),
                "{body}"
            );
            for state in ["held", "abandoned"] {
                assert!(
                    body.contains(&format!(
                        "groundhog_gateway_lane_permits{{lane=\"{lane}\",state=\"{state}\"}} 0"
                    )),
                    "{body}"
                );
            }
            assert!(body.contains(&format!(
                "groundhog_gateway_lane_wait_duration_seconds_count{{lane=\"{lane}\"}} 0"
            )));
            assert!(body.contains(&format!(
                "groundhog_gateway_lane_rejected_total{{lane=\"{lane}\"}} 0"
            )));
        }
    }
}
