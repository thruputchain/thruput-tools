use super::*;

const SERVICE: ServiceTelemetry = ServiceTelemetry {
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

fn log_selection() -> Vec<bool> {
    vec![
        tracing::enabled!(tracing::Level::ERROR),
        tracing::enabled!(tracing::Level::WARN),
        tracing::enabled!(tracing::Level::INFO),
        tracing::enabled!(tracing::Level::DEBUG),
        tracing::enabled!(tracing::Level::TRACE),
        !tracing::error_span!("error_scope").is_disabled(),
        !tracing::warn_span!("warn_scope").is_disabled(),
        !tracing::info_span!("info_scope").is_disabled(),
        !tracing::debug_span!("debug_scope").is_disabled(),
        !tracing::trace_span!("trace_scope").is_disabled(),
        tracing::enabled!(target: "special", tracing::Level::DEBUG),
        !tracing::info_span!(target: "groundhog_otel", "otel_only").is_disabled(),
    ]
}

#[test]
fn static_filter_preserves_log_selection() {
    if let Ok(directive) = std::env::var("GROUNDHOG_TEST_LOG_FILTER") {
        let filter = EnvFilter::new(directive);
        let events = tracing_subscriber::fmt::format::FmtSpan::NONE;
        let subscriber = if std::env::var_os("GROUNDHOG_TEST_LOG_REFERENCE").is_some() {
            subscriber_with_filter(SERVICE.crate_target, "test", filter, events, None)
        } else {
            subscriber(SERVICE.crate_target, "test", filter, events, None)
        };
        let selection = tracing::subscriber::with_default(subscriber, log_selection);
        println!(
            "LOG_SELECTION={}",
            serde_json::to_string(&selection).unwrap()
        );
        return;
    }
    // Callsite interest is shared across concurrently executing tests.
    // Isolate each subscriber so another test cannot enable its callsites.
    for directive in [
        "off",
        "error",
        "warn",
        "info",
        "debug",
        "trace",
        "info,special=debug",
    ] {
        let selections: Vec<Vec<bool>> = [true, false]
            .into_iter()
            .map(|reference| {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "tests::static_filter_preserves_log_selection",
                        "--nocapture",
                    ])
                    .env("GROUNDHOG_TEST_LOG_FILTER", directive)
                    .env_remove("GROUNDHOG_TEST_LOG_REFERENCE");
                if reference {
                    command.env("GROUNDHOG_TEST_LOG_REFERENCE", "1");
                }
                let output = command.output().unwrap();
                assert!(output.status.success(), "{output:?}");
                let stdout = String::from_utf8(output.stdout).unwrap();
                let selected = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("LOG_SELECTION="))
                    .unwrap();
                serde_json::from_str(selected).unwrap()
            })
            .collect();
        assert_eq!(selections[0], selections[1], "filter {directive}");
        assert!(!selections[1][11], "OTLP-only spans must stay out of logs");
    }
}

#[test]
fn span_field_filter_keeps_dynamic_scope() {
    if std::env::var_os("GROUNDHOG_TEST_DYNAMIC_LOG_FILTER").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::span_field_filter_keeps_dynamic_scope",
                "--nocapture",
            ])
            .env("GROUNDHOG_TEST_DYNAMIC_LOG_FILTER", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let logs = String::from_utf8(output.stdout).unwrap();
        assert!(logs.contains("dynamic_inside"), "{logs}");
        for suppressed in ["dynamic_before", "dynamic_after", "dynamic_debug"] {
            assert!(!logs.contains(suppressed), "{logs}");
        }
        return;
    }
    tracing::subscriber::with_default(
        subscriber(
            SERVICE.crate_target,
            "test",
            EnvFilter::new("off,[selected{enabled=true}]=info"),
            tracing_subscriber::fmt::format::FmtSpan::NONE,
            None,
        ),
        || {
            tracing::info!("dynamic_before");
            let selected = tracing::info_span!("selected", enabled = true);
            {
                let _entered = selected.enter();
                tracing::info!("dynamic_inside");
                tracing::debug!("dynamic_debug");
            }
            tracing::info!("dynamic_after");
        },
    );
}

#[tokio::test]
async fn collector_endpoint_builds_provider() {
    if let Ok(endpoint) = std::env::var("GROUNDHOG_TEST_COLLECTOR_ENDPOINT") {
        let provider = provider(&SERVICE, "test", Some(endpoint.into())).unwrap();
        assert!(provider.is_some());
        shutdown(provider).await;
        return;
    }
    for endpoint in [
        "http://collector.example.com:4317",
        "https://collector.example.com:4317",
        "HTTPS://collector.example.com:4317",
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::collector_endpoint_builds_provider",
                "--nocapture",
            ])
            .env("GROUNDHOG_TEST_COLLECTOR_ENDPOINT", endpoint)
            .env_remove("OTEL_SDK_DISABLED")
            .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .output()
            .unwrap();
        assert!(output.status.success(), "{endpoint}: {output:?}");
    }
}

#[test]
fn otel_layer_ignores_dependency_and_verbose_spans() {
    if std::env::var_os("GROUNDHOG_TEST_OTEL_FILTER").is_some() {
        let provider = SdkTracerProvider::builder().build();
        let subscriber = subscriber(
            SERVICE.crate_target,
            "test",
            EnvFilter::new("off"),
            tracing_subscriber::fmt::format::FmtSpan::NONE,
            Some(&provider),
        );
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::trace_span!(target: "h2::proto::streams", "poll").is_disabled());
            assert!(tracing::info_span!(target: "hyper::client", "connect").is_disabled());
            assert!(tracing::debug_span!("verbose").is_disabled());
            assert!(!tracing::info_span!("request").is_disabled());
            assert!(!tracing::debug_span!(target: "groundhog_otel", "rpc").is_disabled());
        });
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::otel_layer_ignores_dependency_and_verbose_spans",
            "--nocapture",
        ])
        .env("GROUNDHOG_TEST_OTEL_FILTER", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn trace_endpoint_takes_precedence() {
    let os = |value: &str| Some(std::ffi::OsString::from(value));
    assert_eq!(
        collector_endpoint(os("http://traces:4317"), os("http://generic:4317")),
        os("http://traces:4317")
    );
    assert_eq!(
        collector_endpoint(os("http://traces:4317"), None),
        os("http://traces:4317")
    );
    assert_eq!(
        collector_endpoint(None, os("http://generic:4317")),
        os("http://generic:4317")
    );
    assert_eq!(
        collector_endpoint(os(""), os("http://generic:4317")),
        os("http://generic:4317")
    );
    assert_eq!(collector_endpoint(None, os("")), None);
    assert_eq!(collector_endpoint(None, None), None);
}

#[test]
fn service_resource() {
    if std::env::var_os("GROUNDHOG_TEST_RESOURCE").is_some() {
        let resource = resource("test");
        assert_eq!(
            resource.get(&opentelemetry::Key::new("service.version")),
            Some(env!("CARGO_PKG_VERSION").into())
        );
        assert_eq!(
            resource.get(&opentelemetry::Key::new("service.name")),
            Some("test".into())
        );
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::service_resource", "--nocapture"])
        .env("GROUNDHOG_TEST_RESOURCE", "1")
        .env_remove("OTEL_SERVICE_NAME")
        .env_remove("OTEL_RESOURCE_ATTRIBUTES")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn disabled_subscriber_respects_filter() {
    if std::env::var_os("GROUNDHOG_TEST_FILTER").is_some() {
        tracing::subscriber::with_default(
            subscriber(
                SERVICE.crate_target,
                "test",
                EnvFilter::new("warn"),
                tracing_subscriber::fmt::format::FmtSpan::NONE,
                None,
            ),
            assert_disabled_tracing,
        );
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::disabled_subscriber_respects_filter",
            "--nocapture",
        ])
        .env("GROUNDHOG_TEST_FILTER", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

// Child processes isolate SDK env parsing from concurrently running tests.
#[test]
fn sampler_environment() {
    if let Ok(expected) = std::env::var("GROUNDHOG_TEST_SAMPLER") {
        assert!(format!("{:?}", SERVICE.provider_builder()).contains(&expected));
        return;
    }
    let default = "ParentBased(TraceIdRatioBased(0.05))";
    for (sampler, arg, expected) in [
        (None, None, default),
        (Some("always_on"), None, "sampler: AlwaysOn"),
        (Some("always_off"), None, "sampler: AlwaysOff"),
        (
            Some("traceidratio"),
            Some("0.25"),
            "sampler: TraceIdRatioBased(0.25)",
        ),
        (Some("traceidratio"), None, default),
        (Some("traceidratio"), Some("abc"), default),
        (Some("traceidratio"), Some("1.5"), default),
        (Some("traceidratio"), Some("-1"), default),
        (Some("traceidratio"), Some("NaN"), default),
        (
            Some("traceidratio"),
            Some(" 0.25 "),
            "sampler: TraceIdRatioBased(0.25)",
        ),
        (Some("parentbased_always_on"), None, "ParentBased(AlwaysOn)"),
        (
            Some("parentbased_always_off"),
            None,
            "ParentBased(AlwaysOff)",
        ),
        (
            Some("parentbased_traceidratio"),
            Some("0.25"),
            "ParentBased(TraceIdRatioBased(0.25))",
        ),
        (Some("parentbased_traceidratio"), None, default),
        (Some("parentbased_traceidratio"), Some("abc"), default),
        (Some("parentbased_traceidratio"), Some("1.5"), default),
        (Some("invalid"), None, default),
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "tests::sampler_environment", "--nocapture"])
            .env_remove("OTEL_TRACES_SAMPLER")
            .env_remove("OTEL_TRACES_SAMPLER_ARG")
            .env("GROUNDHOG_TEST_SAMPLER", expected);
        if let Some(sampler) = sampler {
            child.env("OTEL_TRACES_SAMPLER", sampler);
        }
        if let Some(arg) = arg {
            child.env("OTEL_TRACES_SAMPLER_ARG", arg);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            sampler,
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn sdk_disabled_with_endpoint() {
    if std::env::var_os("GROUNDHOG_TEST_DISABLED").is_some() {
        assert!(
            provider(&SERVICE, "test", Some("invalid endpoint".into()))
                .unwrap()
                .is_none()
        );
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
    assert!(sdk_disabled(Some("true")));
    assert!(sdk_disabled(Some("TRUE")));
    assert!(!sdk_disabled(Some("false")));
    assert!(!sdk_disabled(None));
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::sdk_disabled_with_endpoint"])
        .env("GROUNDHOG_TEST_DISABLED", "1")
        .env("OTEL_SDK_DISABLED", "true")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "invalid endpoint")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test]
async fn enabled_with_endpoint_builds_provider_without_collector() {
    let provider = provider(&SERVICE, "test", Some("http://127.0.0.1:4317".into())).unwrap();
    assert!(provider.is_some());
    shutdown(provider).await;
}
