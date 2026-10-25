//! Process-environment changes scoped to one test.

use std::ffi::{OsStr, OsString};

/// Restores a variable's previous value, or its absence, when dropped, so a
/// failing assertion cannot leak the change into later tests. Tests that use
/// it still run `#[serial]`: the environment is process-wide.
#[must_use = "the variable is restored as soon as the guard drops"]
pub struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: callers serialize environment access with `#[serial]`.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    #[allow(dead_code)]
    pub fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: callers serialize environment access with `#[serial]`.
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: callers serialize environment access with `#[serial]`.
        match &self.previous {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
