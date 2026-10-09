mod heap_profiling;

use clap::Parser;
use std::sync::Arc;
use thruput_faucet::{FaucetConfig, airdrop::RpcAirdropSubmitter, config::FaucetConfigFile, serve};
use tracing_subscriber::EnvFilter;

const BUILD_VERSION: thruput_build_info::BuildVersion = thruput_build_info::build_version!();

#[derive(Debug, Parser)]
#[command(
    name = "thruput-faucet",
    version = BUILD_VERSION.to_string(),
    about = "Thruput Devnet faucet service"
)]
struct Cli {
    /// Path to faucet TOML config.
    #[arg(long)]
    config: std::path::PathBuf,

    /// HTTP listen address.
    #[arg(long)]
    listen: Option<String>,

    /// Private heap profile listen address (Linux; requires `GROUNDHOG_HEAP_PROFILING=1`).
    #[arg(long)]
    pprof_listen: Option<String>,

    /// Serve heap profiles on a non-loopback `--pprof-listen` address (unauthenticated).
    #[arg(long)]
    pprof_allow_remote: bool,
}

/// Log filter from `RUST_LOG`, defaulting to `info` when unset so operators
/// get logs without extra environment setup. A malformed `RUST_LOG` is a
/// startup error rather than a silent fallback.
fn env_filter() -> Result<EnvFilter, std::io::Error> {
    match std::env::var("RUST_LOG") {
        Ok(directives) => EnvFilter::try_new(&directives).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid RUST_LOG `{directives}`: {error}"),
            )
        }),
        Err(std::env::VarError::NotPresent) => Ok(EnvFilter::new("info")),
        Err(error) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid RUST_LOG: {error}"),
        )),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter()?)
        .init();

    let cli = Cli::parse();
    let heap_dump = heap_profiling::initialize().await?;
    let config_toml = tokio::fs::read_to_string(&cli.config)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed to read faucet config `{}`: {error}",
                    cli.config.display()
                ),
            )
        })?;
    let file_config = toml::from_str::<FaucetConfigFile>(&config_toml)?;
    let parts = file_config.into_parts().await?;
    let airdrop_submitter = RpcAirdropSubmitter::new(
        &parts.network_descriptor,
        &parts.airdrop,
        parts.network_hash.clone(),
    )?;
    let mut config = FaucetConfig::new(parts.network_descriptor, Arc::new(airdrop_submitter))?;
    if let Some(listen) = parts.listen_address {
        config.listen_address = listen;
    }
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }
    config.cors_allowed_origins = parts.cors_allowed_origins;
    config.policy_config = parts.policy;

    let allow_remote = cli.pprof_allow_remote || heap_profiling::allow_remote_env();
    let listener = pprof_listener(cli.pprof_listen.as_deref(), allow_remote).await?;
    if let Some(listener) = listener {
        tracing::info!(listen = %listener.local_addr()?, "faucet heap profile server listening");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let profiles = axum::serve(listener, pprof_router(heap_dump, allow_remote))
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
        let profiles = tokio::spawn(async move { profiles.await });
        let result = serve(config).await;
        let _ = shutdown_tx.send(());
        profiles.await??;
        result?;
    } else {
        serve(config).await?;
    }
    Ok(())
}

fn pprof_router(dump: Option<heap_profiling::Dump>, allow_remote: bool) -> axum::Router {
    heap_profiling::routes(dump, allow_remote)
}

async fn pprof_listener(
    address: Option<&str>,
    allow_remote: bool,
) -> std::io::Result<Option<tokio::net::TcpListener>> {
    match address {
        Some(address) => {
            let listener = tokio::net::TcpListener::bind(address).await?;
            heap_profiling::check_dedicated_listener(listener.local_addr()?, allow_remote)?;
            Ok(Some(listener))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serial_test::serial;

    use super::env_filter;

    #[tokio::test]
    async fn pprof_router_guards_requests() {
        use axum::{
            body::{Body, to_bytes},
            http::Request,
        };
        use tower::ServiceExt;
        for (host, origin, method, remote, enabled, status) in [
            (Some("evil.example"), None, "GET", false, true, 403),
            (Some("127.0.0.1"), None, "GET", false, true, 200),
            (Some("localhost"), None, "GET", false, true, 200),
            (Some("[::1]"), None, "GET", false, true, 200),
            (Some("127.0.0.1:9647"), None, "GET", false, true, 200),
            (Some("10.0.0.5:9646"), None, "GET", true, true, 200),
            (Some("evil.example"), None, "GET", true, true, 403),
            (Some("10.0.0.5:9646"), None, "GET", false, true, 403),
            (
                Some("localhost:9647"),
                Some("http://evil.example"),
                "GET",
                false,
                true,
                403,
            ),
            (
                Some("localhost:9647"),
                Some("http://localhost:9647"),
                "GET",
                false,
                true,
                200,
            ),
            (None, None, "GET", false, true, 403),
            (Some("localhost"), None, "POST", false, true, 405),
            (Some("localhost"), None, "GET", false, false, 404),
            (Some("localhost"), None, "HEAD", false, true, 200),
            (Some("localhost"), Some("null"), "GET", false, true, 403),
        ] {
            let dump: Option<super::heap_profiling::Dump> =
                enabled.then_some(|| Ok(b"profile".to_vec()));
            let mut request = Request::builder().method(method).uri("/debug/pprof/heap");
            if let Some(host) = host {
                request = request.header("host", host);
            }
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = super::pprof_router(dump, remote)
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status().as_u16(),
                status,
                "{host:?} {origin:?} {method}"
            );
            if status == 405 {
                assert_eq!(response.headers()["allow"], "GET, HEAD");
            }
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            if status == 200 {
                assert_eq!(
                    &body[..],
                    if method == "HEAD" {
                        &b""[..]
                    } else {
                        &b"profile"[..]
                    }
                );
            }
        }
        for header in ["host", "origin"] {
            for value in [
                axum::http::HeaderValue::from_static("http://localhost"),
                axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
            ] {
                let mut request = Request::builder()
                    .uri("/debug/pprof/heap")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap();
                if value.to_str().is_err() {
                    request.headers_mut().insert(header, value);
                } else {
                    if header == "origin" {
                        request.headers_mut().insert(
                            "origin",
                            axum::http::HeaderValue::from_static("http://localhost"),
                        );
                    }
                    request.headers_mut().append(header, value);
                }
                let response = super::pprof_router(Some(|| Ok(b"profile".to_vec())), false)
                    .oneshot(request)
                    .await
                    .unwrap();
                assert_eq!(response.status(), 403);
            }
        }
    }

    #[tokio::test]
    async fn pprof_listener_requires_explicit_address() {
        let cli =
            super::Cli::try_parse_from(["groundhog-faucet", "--config", "unused.toml"]).unwrap();
        assert!(cli.pprof_listen.is_none());
        assert!(
            super::pprof_listener(cli.pprof_listen.as_deref(), false)
                .await
                .unwrap()
                .is_none()
        );
        let cli = super::Cli::try_parse_from([
            "groundhog-faucet",
            "--config",
            "unused.toml",
            "--pprof-listen",
            "127.0.0.1:0",
        ])
        .unwrap();
        let listener = super::pprof_listener(cli.pprof_listen.as_deref(), false)
            .await
            .unwrap()
            .unwrap();
        assert!(listener.local_addr().unwrap().port() > 0);
    }

    #[tokio::test]
    async fn pprof_listener_requires_remote_opt_in() {
        let error = super::pprof_listener(Some("0.0.0.0:0"), false)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        for (address, allow_remote) in [
            ("0.0.0.0:0", true),
            ("127.0.0.1:0", false),
            ("localhost:0", false),
        ] {
            assert!(
                super::pprof_listener(Some(address), allow_remote)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    fn pprof_allow_remote_flag_parses() {
        let cli = super::Cli::try_parse_from([
            "groundhog-faucet",
            "--config",
            "unused.toml",
            "--pprof-allow-remote",
        ])
        .unwrap();
        assert!(cli.pprof_allow_remote);
    }

    #[test]
    #[serial]
    fn env_filter_defaults_to_info_when_rust_log_unset() {
        unsafe { std::env::remove_var("RUST_LOG") };

        let filter = env_filter().expect("bug: unset RUST_LOG should default");

        assert_eq!(filter.to_string(), "info");
    }

    #[test]
    #[serial]
    fn env_filter_honors_rust_log_when_set() {
        unsafe { std::env::set_var("RUST_LOG", "debug") };

        let filter = env_filter();

        unsafe { std::env::remove_var("RUST_LOG") };
        assert_eq!(
            filter
                .expect("bug: valid RUST_LOG should parse")
                .to_string(),
            "debug"
        );
    }

    #[test]
    #[serial]
    fn env_filter_rejects_malformed_rust_log() {
        unsafe { std::env::set_var("RUST_LOG", "info,foo=notalevel") };

        let result = env_filter();

        unsafe { std::env::remove_var("RUST_LOG") };
        let error = result.expect_err("bug: malformed RUST_LOG should fail");
        assert!(error.to_string().contains("RUST_LOG"), "{error}");
    }
}
