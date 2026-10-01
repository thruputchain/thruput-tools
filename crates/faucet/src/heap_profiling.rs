//! Opt-in heap profiling for the service binary only.

use axum::Router;

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// The allocator reads this NUL-terminated configuration before Rust startup.
#[cfg(target_os = "linux")]
#[unsafe(export_name = "malloc_conf")]
pub static MALLOC_CONF: &[u8] = b"prof:true,prof_active:false,lg_prof_sample:19\0";

fn requested(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

/// Activate only for the exact opt-in value; unsupported hosts fail explicitly.
#[cfg_attr(not(target_os = "linux"), allow(clippy::unused_async))]
pub async fn initialize() -> Result<bool, std::io::Error> {
    if !requested(std::env::var_os("GROUNDHOG_HEAP_PROFILING").as_deref()) {
        return Ok(false);
    }
    #[cfg(target_os = "linux")]
    {
        let ctl = jemalloc_pprof::PROF_CTL.as_ref().ok_or_else(|| {
            std::io::Error::other("jemalloc profiling unavailable; check MALLOC_CONF")
        })?;
        ctl.lock().await.activate().map_err(std::io::Error::other)?;
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "GROUNDHOG_HEAP_PROFILING requires Linux",
    ))
}

/// An empty router when disabled, preserving the host's existing 404 fallback.
pub fn routes<S: Clone + Send + Sync + 'static>(enabled: bool) -> Router<S> {
    #[cfg(target_os = "linux")]
    if enabled {
        return Router::new().route("/debug/pprof/heap", axum::routing::get(heap));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = enabled;
    Router::new()
}

#[cfg(target_os = "linux")]
#[tracing::instrument(skip_all)]
async fn heap() -> axum::response::Response {
    use axum::{
        http::{StatusCode, header},
        response::IntoResponse,
    };

    let Some(ctl) = jemalloc_pprof::PROF_CTL.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut ctl = ctl.lock().await;
    if !ctl.activated() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match ctl.dump_pprof() {
        Ok(body) => ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to dump heap profile");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[test]
    fn only_exact_one_enables_profiling() {
        assert!(!requested(None));
        for value in ["", "0", "true", "01"] {
            assert!(!requested(Some(std::ffi::OsStr::new(value))));
        }
        assert!(requested(Some(std::ffi::OsStr::new("1"))));
    }

    #[tokio::test]
    async fn disabled_route_is_not_exposed() {
        for method in ["GET", "POST"] {
            let response = routes::<()>(false)
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/debug/pprof/heap")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn enabled_route_serves_profile() {
        let ctl = jemalloc_pprof::PROF_CTL.as_ref().unwrap();
        let mut guard = ctl.lock().await;
        assert!(!guard.activated());
        guard.activate().unwrap();
        drop(guard);
        let response = routes::<()>(true)
            .oneshot(
                Request::builder()
                    .uri("/debug/pprof/heap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.starts_with(&[0x1f, 0x8b]));
        ctl.lock().await.deactivate().unwrap();
    }
}
