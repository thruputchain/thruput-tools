//! Temporary startup cancellation without changing later Cargo signal behavior.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::CliError;

static SIGNAL: AtomicI32 = AtomicI32::new(0);
static CAPTURE: Mutex<()> = Mutex::new(());

extern "C" fn record_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::Relaxed);
}

pub(super) struct StartupSignals {
    previous: Vec<(libc::c_int, libc::sigaction)>,
    _capture: MutexGuard<'static, ()>,
}

impl StartupSignals {
    pub(super) fn capture() -> Result<Self, CliError> {
        let capture = CAPTURE.try_lock().map_err(|_| {
            CliError::Other("integration startup signal capture is already active".into())
        })?;
        SIGNAL.store(0, Ordering::Relaxed);
        let mut guard = Self {
            previous: Vec::with_capacity(2),
            _capture: capture,
        };
        for signal in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: zero initialization is valid for sigaction, sigemptyset
            // initializes its mask, and the handler only stores a lock-free atomic.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = record_signal as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            let mut previous = std::mem::MaybeUninit::uninit();
            // SAFETY: both action pointers are valid; successful sigaction writes
            // the previous action. The guard restores actions even on partial failure.
            let result = unsafe {
                libc::sigemptyset(&raw mut action.sa_mask);
                libc::sigaction(signal, &raw const action, previous.as_mut_ptr())
            };
            if result == -1 {
                return Err(CliError::Other(format!(
                    "cannot capture integration startup signals: {}",
                    std::io::Error::last_os_error()
                )));
            }
            // SAFETY: successful sigaction initialized previous.
            guard
                .previous
                .push((signal, unsafe { previous.assume_init() }));
        }
        Ok(guard)
    }

    pub(super) fn finish(mut self) -> bool {
        self.restore();
        self.interrupted()
    }

    fn restore(&mut self) {
        for (signal, previous) in self.previous.iter().rev() {
            // SAFETY: restore the exact action saved for this signal. The static
            // capture mutex prevents overlapping integration startup guards.
            unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
        self.previous.clear();
    }

    #[expect(
        clippy::unused_self,
        reason = "signal state belongs to this guard's exclusive capture"
    )]
    fn interrupted(&self) -> bool {
        SIGNAL.load(Ordering::Relaxed) != 0
    }

    pub(super) async fn cancelled(&self) {
        while !self.interrupted() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for StartupSignals {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn restores_signal_actions_before_later_cargo_work() {
        const CHILD_PHASE: &str = "GROUNDHOG_STARTUP_SIGNAL_TEST_PHASE";
        if let Ok(phase) = std::env::var(CHILD_PHASE) {
            let signals = StartupSignals::capture().unwrap();
            match phase.as_str() {
                "cancel" => {
                    // SAFETY: this test helper deliberately signals only itself.
                    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
                    // No timer was polled: finish must still observe this signal.
                    assert!(signals.finish());
                }
                "success" => assert!(!signals.finish()),
                "drop" => drop(signals),
                _ => panic!("unexpected helper phase"),
            }
            // Like a signal during the subsequent synchronous Cargo stage, this
            // must retain the process's original default termination behavior.
            // SAFETY: signal only this isolated helper process.
            unsafe { libc::raise(libc::SIGTERM) };
            panic!("startup signal handler remained installed");
        }
        for phase in ["cancel", "success", "drop"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "test_cmd::startup_signals::tests::restores_signal_actions_before_later_cargo_work", "--nocapture"])
                .env(CHILD_PHASE, phase)
                .status().unwrap();
            assert_eq!(status.signal(), Some(libc::SIGTERM), "{phase}: {status}");
        }
    }
}
