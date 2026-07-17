//! Optional stderr tracing for forwarded `hog-devnode` logs.
//!
//! The supervisor forwards `hog-devnode`'s logs through the `tracing` framework
//! (under the `hogd_dev` target, with its own diagnostics under
//! `groundhog_node_supervisor`). These helpers install a stderr subscriber so
//! those events actually print, mirroring what `groundhog node` does.

use groundhog_node_supervisor::LogLevel;

/// Install a stderr `tracing` subscriber that prints forwarded `hog-devnode`
/// logs and supervisor diagnostics.
///
/// Idempotent and non-clobbering: it uses `try_init`, so it never replaces a
/// subscriber a test already installed, and it honors `RUST_LOG` when set. A
/// `local` target with a configured `log_level` installs this automatically;
/// call it directly to print logs without setting a specific devnode level.
pub fn init_tracing() {
    install("hogd_dev=info,groundhog_node_supervisor=info");
}

/// Install a subscriber whose default filter matches the requested devnode
/// `level`, so logs emitted at that verbosity print.
pub(crate) fn install_for_level(level: LogLevel) {
    let level = match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error | LogLevel::Critical => "error",
    };
    install(&format!("hogd_dev={level},groundhog_node_supervisor=info"));
}

fn install(default_filter: &str) {
    use tracing_subscriber::EnvFilter;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}
