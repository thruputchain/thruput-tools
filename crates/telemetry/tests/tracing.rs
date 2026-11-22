use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;

const SERVICE: thruput_telemetry::ServiceTelemetry = thruput_telemetry::ServiceTelemetry {
    crate_target: env!("CARGO_CRATE_NAME"),
    max_links_per_span: None,
    warn: |message, error| tracing::warn!(error = error.map(tracing::field::display), "{message}"),
};

async fn shutdown(provider: Option<SdkTracerProvider>) {
    if let Some(provider) = provider {
        tokio::task::spawn_blocking(move || provider.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

fn assert_disabled_tracing() {
    assert!(tracing::info_span!("ordinary").is_disabled());
    assert!(!tracing::enabled!(tracing::Level::DEBUG));
    assert!(!tracing::enabled!(tracing::Level::TRACE));
    assert!(tracing::info_span!(target: "groundhog_otel", "disabled").is_disabled());
    assert!(tracing::enabled!(tracing::Level::WARN));
}

#[tokio::test]
async fn ratio_argument_warns_once() {
    if std::env::var_os("GROUNDHOG_TEST_RATIO_WARNING").is_some() {
        let provider = SERVICE.init(
            "test",
            EnvFilter::new("warn"),
            tracing_subscriber::fmt::format::FmtSpan::NONE,
        );
        assert!(provider.is_some());
        shutdown(provider).await;
        return;
    }
    for sampler in ["traceidratio", "parentbased_traceidratio"] {
        for arg in [None, Some("abc"), Some("1.5")] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "ratio_argument_warns_once", "--nocapture"])
                .env("GROUNDHOG_TEST_RATIO_WARNING", "1")
                .env_remove("OTEL_SDK_DISABLED")
                .env("OTEL_TRACES_SAMPLER", sampler)
                .env_remove("OTEL_TRACES_SAMPLER_ARG")
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317");
            if let Some(arg) = arg {
                child.env("OTEL_TRACES_SAMPLER_ARG", arg);
            }
            let output = child.output().unwrap();
            let logs = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "{logs}");
            assert_eq!(
                logs.matches("invalid OTEL_TRACES_SAMPLER_ARG").count(),
                1,
                "{logs}"
            );
            assert!(
                logs.contains("using parentbased_traceidratio with ratio 0.05"),
                "{logs}"
            );
            assert!(logs.contains(&format!("{arg:?}")), "{logs}");
            assert!(!logs.contains("InvalidSamplerArgument"), "{logs}");
        }
    }
}

#[tokio::test]
async fn closed_collector_warns_once() {
    if std::env::var_os("GROUNDHOG_TEST_CLOSED_COLLECTOR").is_some() {
        let provider = SERVICE
            .init(
                "test",
                EnvFilter::new("info"),
                tracing_subscriber::fmt::format::FmtSpan::NONE,
            )
            .unwrap();
        for _ in 0..3 {
            drop(tracing::info_span!("collector_batch"));
            let flush_provider = provider.clone();
            tokio::task::spawn_blocking(move || flush_provider.force_flush())
                .await
                .unwrap()
                .unwrap();
        }
        shutdown(Some(provider)).await;
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "closed_collector_warns_once", "--nocapture"])
        .env("GROUNDHOG_TEST_CLOSED_COLLECTOR", "1")
        .env_remove("OTEL_SDK_DISABLED")
        .env("OTEL_TRACES_SAMPLER", "always_on")
        .env(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://127.0.0.1:{port}"),
        )
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{logs}");
    assert_eq!(
        logs.matches("OTLP collector unavailable").count(),
        1,
        "{logs}"
    );
    assert!(!logs.contains("BatchSpanProcessor.ExportError"), "{logs}");
    assert!(!logs.contains(" ERROR "), "{logs}");
}

// A child process, like the other subscriber tests: tracing caches each
// callsite's interest process-wide, so a subscriber another test installs
// concurrently can leave `ordinary` enabled here.
#[tokio::test]
async fn invalid_sampler_warns_once() {
    if std::env::var_os("GROUNDHOG_TEST_INVALID").is_some() {
        let provider = SERVICE.init(
            "test",
            EnvFilter::new("warn"),
            tracing_subscriber::fmt::format::FmtSpan::NONE,
        );
        assert!(provider.is_some());
        shutdown(provider).await;
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "invalid_sampler_warns_once", "--nocapture"])
        .env("GROUNDHOG_TEST_INVALID", "1")
        .env_remove("OTEL_SDK_DISABLED")
        .env("OTEL_TRACES_SAMPLER", "invalid")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317")
        .output()
        .unwrap();
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{logs}");
    assert_eq!(
        logs.matches("invalid OTEL_TRACES_SAMPLER").count(),
        1,
        "{logs}"
    );
    assert!(
        !logs.contains("TracerProvider.Config.InvalidSamplerType"),
        "{logs}"
    );
}
#[test]
fn disabled_without_endpoint_builds_no_exporter() {
    if std::env::var_os("GROUNDHOG_TEST_NO_ENDPOINT").is_some() {
        assert!(
            SERVICE
                .init(
                    "test",
                    EnvFilter::new("warn"),
                    tracing_subscriber::fmt::format::FmtSpan::NONE,
                )
                .is_none()
        );
        assert_disabled_tracing();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "disabled_without_endpoint_builds_no_exporter"])
        .env("GROUNDHOG_TEST_NO_ENDPOINT", "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .env_remove("OTEL_SDK_DISABLED")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
