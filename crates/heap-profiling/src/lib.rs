//! Opt-in service heap profiles. Dump requests are serialized; concurrent requests get 429.

mod convert;
pub use convert::jemalloc_to_pprof;

use axum::Router;
use std::io;

/// Activate profiling only for the exact environment value `1`.
#[allow(clippy::unused_async)]
pub async fn initialize() -> io::Result<bool> {
    if std::env::var_os("GROUNDHOG_HEAP_PROFILING").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Ok(false);
    }
    #[cfg(target_os = "linux")]
    {
        linux::activate()?;
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "GROUNDHOG_HEAP_PROFILING requires Linux",
    ))
}

/// Expose the endpoint only when initialization enabled profiling.
pub fn routes<S: Clone + Send + Sync + 'static>(enabled: bool) -> Router<S> {
    #[cfg(target_os = "linux")]
    if enabled {
        return Router::new().route("/debug/pprof/heap", axum::routing::get(heap));
    }
    let _ = enabled;
    Router::new()
}

#[cfg(target_os = "linux")]
#[tracing::instrument(skip_all)]
async fn heap() -> axum::response::Response {
    static BUSY: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let gate = BUSY.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
    run_dump(gate.clone(), linux::dump).await
}

#[cfg(any(target_os = "linux", test))]
async fn run_dump<F>(
    gate: std::sync::Arc<tokio::sync::Semaphore>,
    dump: F,
) -> axum::response::Response
where
    F: FnOnce() -> io::Result<Vec<u8>> + Send + 'static,
{
    use axum::{
        http::{StatusCode, header},
        response::IntoResponse,
    };
    let Ok(permit) = gate.try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    // The task owns the permit: cancellation of the HTTP future cannot start a second dump.
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        dump()
    })
    .await
    {
        Ok(Ok(body)) => {
            ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response()
        }
        error => {
            tracing::error!(?error, "failed to dump heap profile");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_dump_does_not_block_runtime_and_cancellation_retains_permit() {
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run_dump(gate.clone(), move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            finished_tx.send(()).unwrap();
            Ok(Vec::new())
        }));
        started_rx.await.unwrap();
        task.abort();
        let response = run_dump(gate.clone(), || panic!("second dump started")).await;
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        release_tx.send(()).unwrap();
        finished_rx.await.unwrap();
        // Wait for the worker's permit drop, without tying completion to the cancelled future.
        let permit = gate.acquire().await.unwrap();
        drop(permit);
        let response = run_dump(gate.clone(), || Err(io::Error::other("test failure"))).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(gate.available_permits(), 1);
        let response = run_dump(gate, || Ok(vec![1, 2, 3])).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{io, jemalloc_to_pprof};
    use std::{
        ffi::{CStr, CString},
        fs::{File, OpenOptions},
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        path::PathBuf,
        ptr,
    };

    fn result(code: i32) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code))
        }
    }

    fn read_bool(name: &CStr) -> io::Result<bool> {
        let mut value = false;
        let mut size = std::mem::size_of::<bool>();
        // SAFETY: oldp points to a bool of the exact size expected by these bool controls.
        result(unsafe {
            tikv_jemalloc_sys::mallctl(
                name.as_ptr(),
                ptr::from_mut(&mut value).cast(),
                &raw mut size,
                ptr::null_mut(),
                0,
            )
        })?;
        Ok(value)
    }

    pub(super) fn activate() -> io::Result<()> {
        if !read_bool(c"config.prof").unwrap_or(false) || !read_bool(c"opt.prof").unwrap_or(false) {
            return Err(io::Error::other(
                "jemalloc profiling unavailable: require config.prof=true and opt.prof=true; check MALLOC_CONF",
            ));
        }
        let mut value = true;
        // SAFETY: prof.active accepts a bool, and the storage remains live throughout the call.
        result(unsafe {
            tikv_jemalloc_sys::mallctl(
                c"prof.active".as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::from_mut(&mut value).cast(),
                std::mem::size_of::<bool>(),
            )
        })
    }

    struct PrivateDump(PathBuf);
    impl Drop for PrivateDump {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    impl PrivateDump {
        fn create() -> io::Result<Self> {
            use std::io::Read;
            let mut random = File::open("/dev/urandom")?;
            for _ in 0..32 {
                let mut bytes = [0; 16];
                random.read_exact(&mut bytes)?;
                let suffix = u128::from_ne_bytes(bytes);
                let path = std::env::temp_dir().join(format!("groundhog-heap-{suffix:032x}"));
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                {
                    Ok(_) => return Ok(Self(path)),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::other("could not create private heap dump"))
        }
    }

    pub(super) fn dump() -> io::Result<Vec<u8>> {
        let file = PrivateDump::create()?;
        let filename = CString::new(file.0.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let mut pointer = filename.as_ptr();
        // SAFETY: prof.dump expects a pointer to a const char*, not the string itself.
        // Both the pointer and its NUL-terminated target remain live until mallctl returns.
        result(unsafe {
            tikv_jemalloc_sys::mallctl(
                c"prof.dump".as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::from_mut(&mut pointer).cast(),
                std::mem::size_of_val(&pointer),
            )
        })?;
        let dump = std::fs::read_to_string(&file.0)?;
        let maps = std::fs::read_to_string("/proc/self/maps")?;
        jemalloc_to_pprof(&dump, &maps)
    }
}
