use std::{net::SocketAddr, path::PathBuf, time::Duration};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use clap::Parser;
use groundhog_firehose::{
    metrics::Metrics,
    subscription::{self, Hub},
    tailer::Tailer,
};

#[derive(Debug, Parser)]
#[command(
    name = "hog-firehose-serve",
    version,
    about = "Read-only WebSocket firehose"
)]
struct Config {
    /// Directory holding the immutable archive.
    #[arg(long)]
    store: PathBuf,
    /// Health and Prometheus listener.
    #[arg(long, default_value = "127.0.0.1:9552")]
    listen: SocketAddr,
    /// Interval between archive polls in milliseconds.
    #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u64).range(1..))]
    poll_interval_ms: u64,
    /// Maximum healthy tip age in seconds.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    tip_stale_seconds: u64,
    #[command(flatten)]
    subscriptions: subscription::Config,
}

async fn health(State((metrics, _)): State<(Metrics, Hub)>) -> (StatusCode, &'static str) {
    if metrics.is_healthy() {
        (StatusCode::OK, "ok\n")
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "archive tip unavailable or stale\n",
        )
    }
}

async fn exposition(
    State((metrics, hub)): State<(Metrics, Hub)>,
) -> Result<([(&'static str, &'static str); 1], Vec<u8>), StatusCode> {
    let mut bytes = metrics
        .render()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    bytes.extend_from_slice(hub.render_metrics().as_bytes());
    Ok(([("content-type", "text/plain; version=0.0.4")], bytes))
}

async fn shutdown() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            Err(error) => tracing::error!(%error, "cannot install SIGTERM handler"),
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    config.subscriptions.validate(config.listen)?;
    let tailer = Tailer::new(config.store.clone())?;
    let metrics = Metrics::new()?;
    let stale = Duration::from_secs(config.tip_stale_seconds);
    tailer.update_health(&metrics, stale);
    let hub = Hub::new(config.store, tailer.tip(), config.subscriptions);
    let (address, ws) = hub.serve().await?;
    tracing::info!(%address, tip = tailer.tip(), "subscription listener started");
    let router = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(exposition))
        .with_state((metrics.clone(), hub.clone()));
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let http = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = stopped.changed().await;
    });
    let http = std::future::IntoFuture::into_future(http);
    tokio::pin!(http);
    let tail = tailer.run(
        hub,
        metrics.clone(),
        Duration::from_millis(config.poll_interval_ms),
        stale,
    );
    tokio::pin!(tail);
    let (result, http_done) = tokio::select! {
        result = &mut http => (result.map_err(Into::into), true),
        () = &mut tail => (Err("archive tail stopped unexpectedly".into()), false),
        () = ws.clone().stopped() => (Err("WebSocket server stopped unexpectedly".into()), false),
        () = shutdown() => (Ok(()), false),
    };
    metrics.set_healthy(false);
    let _ = ws.stop();
    ws.stopped().await;
    let _ = stop.send(true);
    if !http_done {
        http.await?;
    }
    result
}
