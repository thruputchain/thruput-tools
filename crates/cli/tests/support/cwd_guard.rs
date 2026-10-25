//! Working-directory changes scoped to one test.

use std::path::{Path, PathBuf};

/// Restores the previous working directory when dropped, so a failing
/// assertion cannot leak the change into later tests. Tests that use it still
/// run `#[serial]`: the working directory is process-wide.
#[derive(Debug)]
#[must_use = "the working directory is restored as soon as the guard drops"]
pub struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    pub fn enter(path: &Path) -> Self {
        let previous = std::env::current_dir().expect("bug: current dir should be readable");
        std::env::set_current_dir(path).expect("bug: current dir should be set");
        Self { previous }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).expect("bug: current dir should be restored");
    }
}
