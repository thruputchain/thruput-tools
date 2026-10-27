//! Opt-in service heap profiles. Dump requests are serialized; concurrent requests get 429.
//! DNS-name Hosts are refused because they can rebind to the listener. Remote opt-in
//! permits direct IP-literal access only.

mod convert;
pub use convert::{Frame, Symbolize, jemalloc_to_pprof};

/// Capture a symbolized heap profile from the current process.
#[cfg(all(target_os = "linux", feature = "jemalloc"))]
pub fn dump_pprof() -> io::Result<Vec<u8>> {
    linux::dump()
}

use axum::Router;
use std::{io, net::SocketAddr};

/// Capture a heap profile for an HTTP request.
pub type Dump = fn() -> io::Result<Vec<u8>>;

/// Environment switch permitting unauthenticated profiles beyond loopback.
pub const ALLOW_REMOTE_ENV: &str = "GROUNDHOG_PPROF_ALLOW_REMOTE";

/// Permit remote profiles only for the exact environment value `1`.
pub fn allow_remote_env() -> bool {
    std::env::var_os(ALLOW_REMOTE_ENV).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// Classify loopback IPs, including IPv4-mapped IPv6 addresses.
pub fn is_loopback(addr: SocketAddr) -> bool {
    addr.ip().to_canonical().is_loopback()
}

/// Refuse a dedicated non-loopback listener unless remote profiles are permitted.
pub fn check_dedicated_listener(local: SocketAddr, allow_remote: bool) -> io::Result<()> {
    if !is_loopback(local) {
        if !allow_remote {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "heap profile listener {local} is not loopback; pass --pprof-allow-remote or set GROUNDHOG_PPROF_ALLOW_REMOTE=1 to serve unauthenticated heap profiles beyond loopback"
                ),
            ));
        }
        tracing::warn!(%local, "unauthenticated heap profiles are served beyond loopback");
    }
    Ok(())
}

fn serve_route(local: SocketAddr, allow_remote: bool) -> bool {
    is_loopback(local) || allow_remote
}

/// Expose profiles on a shared listener only when enabled and permitted for its address.
pub fn routes_for_listener<S: Clone + Send + Sync + 'static>(
    dump: Option<Dump>,
    local: SocketAddr,
    allow_remote: bool,
) -> Router<S> {
    if dump.is_some() && !is_loopback(local) {
        if allow_remote {
            tracing::warn!(%local, "unauthenticated heap profiles are served beyond loopback");
        } else {
            tracing::warn!(%local, "heap profile route is disabled on a non-loopback listener; pass --pprof-allow-remote or set GROUNDHOG_PPROF_ALLOW_REMOTE=1 to serve unauthenticated heap profiles beyond loopback");
        }
    }
    routes(
        dump.filter(|_| serve_route(local, allow_remote)),
        allow_remote,
    )
}

/// Activate profiling only for the exact environment value `1`.
#[allow(clippy::unused_async)]
pub async fn initialize() -> io::Result<Option<Dump>> {
    if std::env::var_os("GROUNDHOG_HEAP_PROFILING").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Ok(None);
    }
    #[cfg(all(target_os = "linux", feature = "jemalloc"))]
    {
        linux::activate()?;
        Ok(Some(linux::dump))
    }
    #[cfg(not(all(target_os = "linux", feature = "jemalloc")))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "GROUNDHOG_HEAP_PROFILING needs a Linux build with the heap-profiling feature",
    ))
}

/// Expose the endpoint only when initialization enabled profiling.
pub fn routes<S: Clone + Send + Sync + 'static>(
    dump: Option<Dump>,
    allow_remote: bool,
) -> Router<S> {
    if let Some(dump) = dump {
        return Router::new()
            .route(
                "/debug/pprof/heap",
                axum::routing::any(move || async move {
                    static BUSY: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
                        std::sync::OnceLock::new();
                    let gate =
                        BUSY.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
                    run_dump(gate.clone(), dump).await
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(allow_remote, guard));
    }
    Router::new()
}

fn allowed_host(raw: &str, allow_remote: bool) -> bool {
    fn port(value: &str) -> bool {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
    }
    let host = if let Some(bracketed) = raw.strip_prefix('[') {
        let Some((host, rest)) = bracketed.split_once(']') else {
            return false;
        };
        if !rest.is_empty() && !rest.strip_prefix(':').is_some_and(port) {
            return false;
        }
        return host
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|ip| allow_remote || ip.to_canonical().is_loopback());
    } else if raw.bytes().filter(|byte| *byte == b':').count() == 1 {
        let Some((host, value)) = raw.split_once(':') else {
            return false;
        };
        if !port(value) {
            return false;
        }
        host
    } else {
        raw
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| allow_remote || ip.to_canonical().is_loopback())
}

fn same_host_origin(origin: &str, host: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    let mut bytes = scheme.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        return false;
    }
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    !authority.is_empty()
        && !authority.contains(['/', '?', '#', '@'])
        && authority.eq_ignore_ascii_case(host)
}

#[tracing::instrument(skip_all)]
async fn guard(
    axum::extract::State(allow_remote): axum::extract::State<bool>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::{
        http::{Method, StatusCode, header},
        response::IntoResponse,
    };
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
        )
            .into_response();
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let host = hosts.next().and_then(|value| value.to_str().ok());
    let Some(host) = host.filter(|host| hosts.next().is_none() && allowed_host(host, allow_remote))
    else {
        return (
            StatusCode::FORBIDDEN,
            "heap profile requires a loopback or IP-literal Host header",
        )
            .into_response();
    };
    if request
        .uri()
        .authority()
        .is_some_and(|authority| !authority.as_str().eq_ignore_ascii_case(host))
    {
        return (
            StatusCode::FORBIDDEN,
            "heap profile requires a loopback or IP-literal Host header",
        )
            .into_response();
    }
    let mut origins = request.headers().get_all(header::ORIGIN).iter();
    if let Some(origin) = origins.next()
        && (origins.next().is_some()
            || !origin
                .to_str()
                .is_ok_and(|origin| same_host_origin(origin, host)))
    {
        return (
            StatusCode::FORBIDDEN,
            "heap profile requires a same-host Origin header",
        )
            .into_response();
    }
    next.run(request).await
}

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

    #[test]
    fn host_policy() {
        for (host, local, remote) in [
            ("127.0.0.1", true, true),
            ("127.0.0.1:6060", true, true),
            ("::1", true, true),
            ("[::1]", true, true),
            ("[::1]:6060", true, true),
            ("localhost:6060", true, true),
            ("LOCALHOST", true, true),
            ("[localhost]", false, false),
            ("[localhost]:6060", false, false),
            ("[127.0.0.1]", false, false),
            ("localhost.", false, false),
            ("evil.example", false, false),
            ("evil.example:9646", false, false),
            ("10.0.0.5:9646", false, true),
            ("10.0.0.5", false, true),
            ("[2001:db8::1]:6060", false, true),
            ("2001:db8::1", false, true),
            ("[::ffff:127.0.0.1]", true, true),
            ("[::ffff:127.0.0.1]:6060", true, true),
            ("", false, false),
            ("127.0.0.1:abc", false, false),
            ("127.0.0.1:", false, false),
            ("[::1]:abc", false, false),
            ("[::1]junk", false, false),
            ("[::1", false, false),
            (" localhost", false, false),
        ] {
            assert_eq!(allowed_host(host, false), local, "{host}");
            assert_eq!(allowed_host(host, true), remote, "{host}");
        }
    }

    #[test]
    fn origin_policy() {
        for (origin, expected) in [
            ("http://localhost:6060", true),
            ("https://LOCALHOST:6060/", true),
            ("http://evil.example", false),
            ("null", false),
            ("http://localhost", false),
            ("http://localhost:6060/path", false),
            ("http://localhost:6060//", false),
            ("http://localhost:6060?x", false),
            ("http://localhost:6060#x", false),
            ("://localhost:6060", false),
            ("1http://localhost:6060", false),
        ] {
            assert_eq!(
                same_host_origin(origin, "localhost:6060"),
                expected,
                "{origin}"
            );
        }
        assert!(same_host_origin("http://[::1]:6060", "[::1]:6060"));
    }

    #[test]
    fn loopback_classification() {
        for (ip, expected) in [
            ("127.0.0.1", true),
            ("127.0.0.2", true),
            ("::1", true),
            ("::ffff:127.0.0.1", true),
            ("0.0.0.0", false),
            ("::", false),
            ("10.0.0.5", false),
            ("192.168.1.1", false),
            ("::ffff:10.0.0.5", false),
        ] {
            let addr = SocketAddr::new(ip.parse().unwrap(), 0);
            assert_eq!(is_loopback(addr), expected, "{addr}");
        }
    }

    #[test]
    fn dedicated_listener_policy() {
        let remote = "0.0.0.0:1234".parse().unwrap();
        let error = check_dedicated_listener(remote, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            error.to_string(),
            "heap profile listener 0.0.0.0:1234 is not loopback; pass --pprof-allow-remote or set GROUNDHOG_PPROF_ALLOW_REMOTE=1 to serve unauthenticated heap profiles beyond loopback"
        );
        assert!(check_dedicated_listener(remote, true).is_ok());
        assert!(check_dedicated_listener("127.0.0.1:0".parse().unwrap(), false).is_ok());
    }

    #[test]
    fn shared_listener_route_policy() {
        let loopback: SocketAddr = "127.0.0.1:9646".parse().unwrap();
        let remote: SocketAddr = "10.0.0.5:9646".parse().unwrap();
        for (local, allow_remote, expected) in [
            (loopback, false, true),
            (remote, false, false),
            (remote, true, true),
        ] {
            assert_eq!(
                serve_route(local, allow_remote),
                expected,
                "{local} {allow_remote}"
            );
        }
    }

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

#[cfg(all(target_os = "linux", feature = "jemalloc"))]
mod linux {
    use super::{Frame, Symbolize, io, jemalloc_to_pprof};
    use std::{
        collections::BTreeMap,
        ffi::{CStr, CString},
        fs::{File, OpenOptions},
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        path::PathBuf,
        ptr,
    };

    #[derive(Default)]
    struct LinuxSymbolizer {
        build_ids: BTreeMap<String, Option<String>>,
    }

    impl Symbolize for LinuxSymbolizer {
        fn frames(&mut self, pc: u64) -> Vec<Frame> {
            let mut frames = Vec::new();
            backtrace::resolve(pc as *mut std::ffi::c_void, |symbol| {
                if let Some(name) = symbol.name() {
                    frames.push(Frame {
                        name: format!("{name:#}"),
                        system_name: String::from_utf8_lossy(name.as_bytes()).into_owned(),
                        file: symbol.filename().map(|path| path.display().to_string()),
                        line: symbol.lineno(),
                    });
                }
            });
            frames
        }

        fn build_id(&mut self, path: &str) -> Option<String> {
            self.build_ids
                .entry(path.to_owned())
                .or_insert_with(|| read_build_id(path))
                .clone()
        }
    }

    fn read_build_id(path: &str) -> Option<String> {
        use object::Object;
        use std::fmt::Write;
        let file = std::fs::File::open(path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        // Read only the headers and note, not the whole file.
        let data = object::ReadCache::new(file);
        let elf = object::read::elf::ElfFile64::<object::Endianness, _>::parse(&data).ok()?;
        let bytes = elf.build_id().ok()??;
        let mut id = String::new();
        for byte in bytes {
            write!(&mut id, "{byte:02x}").ok()?;
        }
        Some(id)
    }

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
                let path = std::env::temp_dir().join(format!("thruput-heap-{suffix:032x}"));
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
        let result = jemalloc_to_pprof(&dump, &maps, &mut LinuxSymbolizer::default());
        // Drop parsed debuginfo so it neither stays resident nor appears in later profiles.
        backtrace::clear_symbol_cache();
        result
    }

    #[cfg(test)]
    mod tests {
        use super::read_build_id;
        use object::Object;
        use std::fmt::Write;

        #[test]
        fn lazy_build_id_matches_full_file_read_and_rejects_directory() {
            let path = std::env::current_exe().unwrap();
            let lazy_id = read_build_id(path.to_str().unwrap());
            let bytes = std::fs::read(&path).unwrap();
            let elf = object::read::elf::ElfFile64::<object::Endianness>::parse(&*bytes).unwrap();
            let full_id = elf.build_id().unwrap().map(|bytes| {
                let mut id = String::new();
                for byte in bytes {
                    write!(&mut id, "{byte:02x}").unwrap();
                }
                id
            });
            assert!(lazy_id.is_some());
            assert!(full_id.is_some());
            assert_eq!(lazy_id, full_id);
            assert_eq!(read_build_id("/proc/self"), None);
        }
    }
}
