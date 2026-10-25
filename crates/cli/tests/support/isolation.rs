//! Developer environment that must not reach a CLI invocation under test.

use std::process::Command;

/// Variables that change what the CLI does regardless of the test's setup: a
/// relocated cargo target directory moves where project commands look for
/// build artifacts, and a configured proxy would intercept loopback mocks.
const DEVELOPER_ENV: [&str; 8] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// Clear [`DEVELOPER_ENV`] and bypass any system proxy. Tests that exercise
/// one of these variables set it on the command afterwards.
pub fn isolate_from_developer_env(command: &mut Command) -> &mut Command {
    for name in DEVELOPER_ENV {
        command.env_remove(name);
    }
    command.env("NO_PROXY", "*").env("no_proxy", "*")
}
