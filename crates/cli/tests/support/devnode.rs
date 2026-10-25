use std::path::PathBuf;

#[allow(dead_code)]
#[path = "../../../node-supervisor/src/test_support.rs"]
mod supervisor_fixtures;

/// Build the supervisor fixture with the current protocol dependencies.
pub fn binary() -> PathBuf {
    supervisor_fixtures::fake_hogd_dev_binary()
}
