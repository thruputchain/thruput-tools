use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use thruput_json_rpc_types::error::native::AttemptAdmission;

const QUEUED: u8 = 0;
const CANCELLED: u8 = 1;
const DISPATCHED: u8 = 2;
const REJECTED: u8 = 3;

/// Attempt-local evidence shared with the batch worker and every deadline layer.
/// A cancellation wins only before the worker atomically takes dispatch ownership.
#[derive(Debug, Clone, Default)]
pub struct SubmissionAttempt(Arc<AtomicU8>);
impl SubmissionAttempt {
    /// Claim this item before moving its bytes into a batch. A false return means
    /// the item must not be sent, even if its reply channel still appears open.
    pub fn begin_dispatch(&self) -> bool {
        self.0
            .compare_exchange(QUEUED, DISPATCHED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    /// Record an authoritative scheduler refusal, before delivering its reply.
    pub fn rejected(&self) {
        let _ = self
            .0
            .compare_exchange(DISPATCHED, REJECTED, Ordering::AcqRel, Ordering::Acquire);
    }
    /// Packing failed before the sink was invoked. No transport may use this item.
    pub fn packing_failed(&self) {
        let _ = self
            .0
            .compare_exchange(DISPATCHED, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
    }
    /// Close the queued dispatch gate and return the strongest available evidence.
    /// Never infer non-dispatch merely from a timeout or a closed reply channel.
    pub fn cancel(&self) -> AttemptAdmission {
        let _ = self
            .0
            .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
        match self.0.load(Ordering::Acquire) {
            QUEUED | CANCELLED => AttemptAdmission::NotAttempted,
            REJECTED => AttemptAdmission::Rejected,
            _ => AttemptAdmission::Unknown,
        }
    }
    /// Close the gate if the caller drops its future, including outer cancellation.
    pub fn guard(&self) -> SubmissionGuard {
        SubmissionGuard(self.clone())
    }
}

#[derive(Debug)]
pub struct SubmissionGuard(SubmissionAttempt);
impl Drop for SubmissionGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_and_dispatch_are_mutually_exclusive() {
        for _ in 0..128 {
            let attempt = SubmissionAttempt::default();
            let worker = attempt.clone();
            let thread = std::thread::spawn(move || worker.begin_dispatch());
            let evidence = attempt.cancel();
            let sent = thread.join().unwrap();
            if matches!(evidence, AttemptAdmission::NotAttempted) {
                assert!(!sent);
            }
        }
    }
    #[test]
    fn cancellation_preserves_dispatch_and_refusal() {
        let attempt = SubmissionAttempt::default();
        assert!(attempt.begin_dispatch());
        assert!(matches!(attempt.cancel(), AttemptAdmission::Unknown));
        attempt.rejected();
        assert!(matches!(attempt.cancel(), AttemptAdmission::Rejected));
        let queued = SubmissionAttempt::default();
        drop(queued.guard());
        assert!(!queued.begin_dispatch());
    }
    #[test]
    fn failed_packing_retains_proof_that_no_sink_was_called() {
        let attempt = SubmissionAttempt::default();
        assert!(attempt.begin_dispatch());
        attempt.packing_failed();
        assert!(matches!(attempt.cancel(), AttemptAdmission::NotAttempted));
        assert!(!attempt.begin_dispatch());
    }
}
