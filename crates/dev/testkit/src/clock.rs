//! Virtual clock owned by the harness.
//!
//! Agents read time and sleep only through this clock, never through
//! `std::time`. Phase 0 exposes a manually advanced logical clock; a later
//! phase drives it from block production so a whole scenario advances in
//! virtual time.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

/// A logical clock measured in whole milliseconds of virtual time.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    inner: Arc<Mutex<ClockState>>,
}

#[derive(Debug, Default)]
struct ClockState {
    now_ms: u64,
    waiters: Vec<Waiter>,
}

#[derive(Debug)]
struct Waiter {
    target_ms: u64,
    wake: oneshot::Sender<()>,
}

impl Clock {
    /// Create a clock at virtual time zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current virtual time, in milliseconds since the clock started.
    pub fn now_ms(&self) -> u64 {
        self.lock().now_ms
    }

    /// Advance virtual time by `delta_ms`, waking every sleeper whose deadline
    /// has now passed.
    pub fn advance_ms(&self, delta_ms: u64) {
        let mut state = self.lock();
        state.now_ms = state.now_ms.saturating_add(delta_ms);
        let now = state.now_ms;
        let mut i = 0;
        while i < state.waiters.len() {
            if state.waiters[i].target_ms <= now {
                let waiter = state.waiters.swap_remove(i);
                // A dropped receiver just means that sleeper went away.
                let _ = waiter.wake.send(());
            } else {
                i += 1;
            }
        }
    }

    /// Wait until virtual time has advanced by at least `dur_ms`.
    ///
    /// Returns immediately when `dur_ms` is zero. Otherwise the future resolves
    /// only when another caller advances the clock past the deadline, so a
    /// scenario that sleeps must arrange for time to advance.
    pub async fn sleep_ms(&self, dur_ms: u64) {
        let rx = {
            let mut state = self.lock();
            let target_ms = state.now_ms.saturating_add(dur_ms);
            if state.now_ms >= target_ms {
                return;
            }
            let (tx, rx) = oneshot::channel();
            state.waiters.push(Waiter {
                target_ms,
                wake: tx,
            });
            rx
        };
        // A send error means the clock was torn down; either way, stop waiting.
        let _ = rx.await;
    }

    fn lock(&self) -> MutexGuard<'_, ClockState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Clock;

    #[tokio::test]
    async fn sleep_waits_until_the_clock_advances() {
        let clock = Clock::new();
        let sleeper_clock = clock.clone();
        let sleeper = tokio::spawn(async move {
            sleeper_clock.sleep_ms(100).await;
            sleeper_clock.now_ms()
        });

        // Let the sleeper register its deadline, then confirm it is still
        // pending because virtual time has not moved.
        tokio::task::yield_now().await;
        assert!(
            !sleeper.is_finished(),
            "sleeper resolved before the clock advanced"
        );

        clock.advance_ms(100);

        let woke_at = tokio::time::timeout(Duration::from_millis(500), sleeper)
            .await
            .expect("sleeper resolves once the clock passes its deadline")
            .expect("sleeper task did not panic");
        assert_eq!(woke_at, 100);
    }

    #[tokio::test]
    async fn zero_duration_sleep_returns_immediately() {
        let clock = Clock::new();
        clock.sleep_ms(0).await;
        assert_eq!(clock.now_ms(), 0);
    }
}
