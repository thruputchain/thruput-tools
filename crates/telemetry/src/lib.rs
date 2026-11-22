//! Optional OTLP tracing; exporter failures never affect service availability.
use opentelemetry::{propagation::Extractor, trace::TracerProvider};
use opentelemetry_otlp::{
    WithExportConfig, WithTonicConfig, tonic_types::transport::ClientTlsConfig,
};
use opentelemetry_sdk::{
    Resource,
    trace::{Sampler, SdkTracerProvider, SpanData, SpanExporter},
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tracing_subscriber::filter::FilterExt;
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Service-specific tracing policy. Diagnostics are emitted at the service's
/// own callsite so existing log target filters keep working.
#[derive(Debug)]
pub struct ServiceTelemetry {
    pub crate_target: &'static str,
    pub max_links_per_span: Option<u32>,
    pub warn: fn(&str, Option<&dyn std::fmt::Display>),
}

#[derive(Debug)]
struct BestEffortExporter {
    inner: opentelemetry_otlp::SpanExporter,
    warned: AtomicBool,
    service: &'static ServiceTelemetry,
}

impl SpanExporter for BestEffortExporter {
    async fn export(&self, batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
        if let Err(error) = self.inner.export(batch).await
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            (self.service.warn)(
                "OTLP collector unavailable; continuing without exported spans",
                Some(&error),
            );
        }
        Ok(())
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }
}

fn provider(
    service: &'static ServiceTelemetry,
    default_service: &'static str,
    endpoint: Option<std::ffi::OsString>,
) -> Result<Option<SdkTracerProvider>, opentelemetry_otlp::ExporterBuildError> {
    if sdk_disabled(std::env::var("OTEL_SDK_DISABLED").ok().as_deref()) {
        return Ok(None);
    }
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let endpoint = endpoint.to_string_lossy().into_owned();
    let use_tls = endpoint
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| {
            uri.scheme_str()
                .map(|scheme| scheme.eq_ignore_ascii_case("https"))
        })
        .unwrap_or(false);
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint);
    let exporter = if use_tls {
        exporter.with_tls_config(ClientTlsConfig::new().with_enabled_roots())
    } else {
        exporter
    }
    .build()?;
    Ok(Some(
        service
            .provider_builder()
            .with_resource(resource(default_service))
            .with_batch_exporter(BestEffortExporter {
                inner: exporter,
                warned: AtomicBool::new(false),
                service,
            })
            .build(),
    ))
}

/// The trace-specific endpoint wins over the generic one, as in the OTLP
/// exporter spec; an empty value counts as unset.
fn collector_endpoint(
    traces: Option<std::ffi::OsString>,
    generic: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    traces
        .filter(|value| !value.is_empty())
        .or(generic.filter(|value| !value.is_empty()))
}

fn resource(default_service: &'static str) -> Resource {
    // The default resource builder includes the SDK's environment detectors.
    let resource = Resource::builder()
        .with_service_name(default_service)
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_detector(Box::new(
            opentelemetry_sdk::resource::EnvResourceDetector::new(),
        ));
    match std::env::var("OTEL_SERVICE_NAME") {
        Ok(service) => resource.with_service_name(service),
        Err(_) => resource,
    }
    .build()
}

fn sampler_configuration(
    name: Option<&str>,
    arg: Option<&str>,
) -> (Option<Sampler>, Option<String>) {
    let default = || Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(0.05)));
    if matches!(name, Some("traceidratio" | "parentbased_traceidratio")) {
        if let Some(ratio) = arg
            .and_then(|arg| arg.trim().parse::<f64>().ok())
            .filter(|ratio| ratio.is_finite() && (0.0..=1.0).contains(ratio))
        {
            let sampler = Sampler::TraceIdRatioBased(ratio);
            return (
                Some(if name == Some("parentbased_traceidratio") {
                    Sampler::ParentBased(Box::new(sampler))
                } else {
                    sampler
                }),
                None,
            );
        }
        // Rust and Go SDKs use 1.0 for parentbased_traceidratio without _ARG; we use 0.05.
        return (
            Some(default()),
            Some(format!(
                "invalid OTEL_TRACES_SAMPLER_ARG {arg:?}; using parentbased_traceidratio with ratio 0.05"
            )),
        );
    }
    if invalid_sampler(name) {
        return (
            Some(default()),
            Some(
                "invalid OTEL_TRACES_SAMPLER; using parentbased_traceidratio with ratio 0.05"
                    .into(),
            ),
        );
    }
    (name.is_none().then(default), None)
}

fn sdk_disabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn invalid_sampler(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !matches!(
            value,
            "always_on"
                | "always_off"
                | "traceidratio"
                | "parentbased_always_on"
                | "parentbased_always_off"
                | "parentbased_traceidratio"
        )
    })
}

impl ServiceTelemetry {
    /// Configure the service sampler and span-link limit.
    pub fn provider_builder(&self) -> opentelemetry_sdk::trace::TracerProviderBuilder {
        let (sampler, warning) = sampler_configuration(
            std::env::var("OTEL_TRACES_SAMPLER").ok().as_deref(),
            std::env::var("OTEL_TRACES_SAMPLER_ARG").ok().as_deref(),
        );
        // 0.33 reads sampler env vars in the builder's default Config. Suppress its
        // always-on fallback warning: init emits our corrected fallback warning.
        let builder = if warning.is_some() {
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                SdkTracerProvider::builder()
            })
        } else {
            SdkTracerProvider::builder()
        };
        let builder = match self.max_links_per_span {
            Some(limit) => builder.with_max_links_per_span(limit),
            None => builder,
        };
        match sampler {
            Some(sampler) => builder.with_sampler(sampler),
            None => builder,
        }
    }

    /// Install the existing log layers and optional batch OTLP exporter.
    pub fn init(
        &'static self,
        default_service: &'static str,
        filter: EnvFilter,
        events: tracing_subscriber::fmt::format::FmtSpan,
    ) -> Option<SdkTracerProvider> {
        let result = provider(
            self,
            default_service,
            collector_endpoint(
                std::env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
                std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT"),
            ),
        );
        let (provider, error) = match result {
            Ok(provider) => (provider, None),
            Err(error) => (None, Some(error)),
        };
        subscriber(
            self.crate_target,
            default_service,
            filter,
            events,
            provider.as_ref(),
        )
        .init();
        if provider.is_some()
            && let (_, Some(warning)) = sampler_configuration(
                std::env::var("OTEL_TRACES_SAMPLER").ok().as_deref(),
                std::env::var("OTEL_TRACES_SAMPLER_ARG").ok().as_deref(),
            )
        {
            (self.warn)(&warning, None);
        }
        if let Some(error) = error {
            (self.warn)("cannot configure OTLP tracing; continuing", Some(&error));
        }
        provider
    }
}

fn subscriber(
    crate_target: &'static str,
    default_service: &'static str,
    filter: EnvFilter,
    events: tracing_subscriber::fmt::format::FmtSpan,
    provider: Option<&SdkTracerProvider>,
) -> Box<dyn tracing::Subscriber + Send + Sync> {
    // A plain level avoids EnvFilter's per-span filter hooks. Keep EnvFilter
    // when target/span directives require that filtering.
    if let Ok(level) = filter
        .to_string()
        .parse::<tracing_subscriber::filter::LevelFilter>()
    {
        subscriber_with_filter(crate_target, default_service, level, events, provider)
    } else {
        subscriber_with_filter(crate_target, default_service, filter, events, provider)
    }
}

fn subscriber_with_filter<F>(
    crate_target: &'static str,
    default_service: &'static str,
    filter: F,
    events: tracing_subscriber::fmt::format::FmtSpan,
    provider: Option<&SdkTracerProvider>,
) -> Box<dyn tracing::Subscriber + Send + Sync>
where
    F: tracing_subscriber::layer::Filter<tracing_subscriber::Registry> + Send + Sync + 'static,
{
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_span_events(events)
            // Keep OTLP-only spans out of log context and close events.
            .with_filter(
                filter.and(tracing_subscriber::filter::filter_fn(|metadata| {
                    metadata.target() != "groundhog_otel"
                })),
            ),
    );
    match provider {
        Some(provider) => Box::new(
            subscriber.with(
                tracing_opentelemetry::layer()
                    .with_tracer(provider.tracer(default_service))
                    .with_filter(otel_filter(crate_target)),
            ),
        ),
        None => Box::new(subscriber),
    }
}

/// Export the OTLP-only spans and this crate's INFO-and-above spans. The fmt
/// layer's `RUST_LOG` filter does not apply to this sibling layer, so without
/// its own filter it would record every dependency span (h2 polls, frames).
fn otel_filter(
    crate_target: &'static str,
) -> tracing_subscriber::filter::FilterFn<impl Fn(&tracing::Metadata<'_>) -> bool> {
    tracing_subscriber::filter::filter_fn(move |metadata| {
        metadata.target() == "groundhog_otel"
            || (metadata.target().starts_with(crate_target)
                && *metadata.level() <= tracing::Level::INFO)
    })
}

struct Headers<'a>(&'a http::HeaderMap);
impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.to_str().ok()
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

/// Read the W3C trace context from incoming HTTP headers.
pub fn extract(headers: &http::HeaderMap) -> opentelemetry::Context {
    use opentelemetry::propagation::TextMapPropagator;
    opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(&Headers(headers))
}

#[cfg(test)]
mod tests;
