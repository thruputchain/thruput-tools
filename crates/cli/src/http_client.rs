//! Shared construction for HTTP(S) clients owned by the Thruput CLI.
//!
//! Both blocking requests and JSON-RPC use the same snapshot of the standard proxy environment.
//! Keep future CLI-managed downloads on these constructors instead of adding command-specific
//! proxy parsing.

use std::ffi::OsString;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http_body_util::BodyExt as _;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::http_helpers::HttpError;
use jsonrpsee::http_client::transport;
use jsonrpsee::http_client::{HttpClientBuilder, HttpRequest, HttpResponse};
use tower::{Layer, Service, ServiceBuilder};

use crate::CliError;

const HTTP_PROXY_NAMES: [&str; 2] = ["HTTP_PROXY", "http_proxy"];
const HTTPS_PROXY_NAMES: [&str; 2] = ["HTTPS_PROXY", "https_proxy"];
const ALL_PROXY_NAMES: [&str; 2] = ["ALL_PROXY", "all_proxy"];

pub(crate) fn build_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, CliError> {
    // Keep reqwest's default TLS backend so the operating-system trust store remains authoritative.
    validate_proxy_environment()?;
    builder.build().map_err(|_| CliError::HttpClientInit)
}

pub(crate) fn rpc_client(rpc_url: &str) -> Result<impl ClientT + Clone + use<>, CliError> {
    // Preserve jsonrpsee's no-redirect behavior so JSON-RPC POSTs are never replayed elsewhere.
    let reqwest =
        build_client(reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()))?;
    // jsonrpsee 0.25 has no proxy-aware connector, but its Tower hook can replace the HTTP
    // service while preserving its JSON-RPC serialization, response limits, and timeouts.
    HttpClientBuilder::default()
        .set_http_middleware(ServiceBuilder::new().layer(ReqwestLayer { client: reqwest }))
        .build(rpc_url)
        .map_err(|error| CliError::RpcConnect(format!("{rpc_url}: {error}")))
}

#[derive(Clone)]
struct ProxyValue {
    variable: &'static str,
    value: String,
}

fn validate_proxy_environment() -> Result<(), CliError> {
    // Reqwest owns proxy precedence, NO_PROXY matching, and OS proxy discovery. Validate the two
    // effective destination settings first so a malformed value cannot be silently bypassed.
    let all = first_env(&ALL_PROXY_NAMES)?.filter(nonempty_proxy);
    let http = first_env(&HTTP_PROXY_NAMES)?
        .filter(nonempty_proxy)
        .or_else(|| all.clone());
    let https = first_env(&HTTPS_PROXY_NAMES)?
        .filter(nonempty_proxy)
        .or(all);

    if let Some(value) = &http {
        validate_proxy(value)?;
    }
    if let Some(value) = &https {
        validate_proxy(value)?;
    }

    Ok(())
}

fn nonempty_proxy(proxy: &ProxyValue) -> bool {
    !proxy.value.is_empty()
}

fn first_env(names: &'static [&'static str]) -> Result<Option<ProxyValue>, CliError> {
    for name in names {
        let Some(value) = std::env::var_os(name) else {
            continue;
        };
        let value = os_string(value, name)?;
        return Ok(Some(ProxyValue {
            variable: name,
            value,
        }));
    }
    Ok(None)
}

fn os_string(value: OsString, variable: &'static str) -> Result<String, CliError> {
    value
        .into_string()
        .map_err(|_| CliError::ProxyConfiguration {
            variable,
            message: "value must be valid Unicode",
        })
}

fn validate_proxy(proxy: &ProxyValue) -> Result<(), CliError> {
    // Reqwest follows curl-style proxy conventions and treats a scheme-less host as HTTP.
    let value = if proxy.value.contains("://") {
        std::borrow::Cow::Borrowed(proxy.value.as_str())
    } else {
        std::borrow::Cow::Owned(format!("http://{}", proxy.value))
    };
    let valid = reqwest::Url::parse(&value)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
    if valid {
        Ok(())
    } else {
        Err(CliError::ProxyConfiguration {
            variable: proxy.variable,
            message: "expected an HTTP(S) proxy URL or host:port",
        })
    }
}

#[derive(Clone, Debug)]
struct ReqwestLayer {
    client: reqwest::Client,
}

impl<S> Layer<S> for ReqwestLayer {
    type Service = ReqwestService;

    fn layer(&self, _inner: S) -> Self::Service {
        ReqwestService {
            client: self.client.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct ReqwestService {
    client: reqwest::Client,
}

impl Service<HttpRequest> for ReqwestService {
    type Response = HttpResponse<reqwest::Body>;
    type Error = transport::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        let client = self.client.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = body
                .collect()
                .await
                .map_err(|_| transport_error("failed to prepare outbound HTTP request"))?
                .to_bytes();
            let response = client
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(|error| transport_error(request_error_message(&error)))?;
            Ok(response.into())
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct SanitizedTransportError(&'static str);

fn transport_error(message: &'static str) -> transport::Error {
    transport::Error::Http(HttpError::Stream(Box::new(SanitizedTransportError(
        message,
    ))))
}

fn request_error_message(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "outbound HTTP request timed out"
    } else if error.is_connect() {
        "outbound HTTP connection failed"
    } else if error.is_request() {
        "outbound HTTP request failed"
    } else {
        "outbound HTTP response failed"
    }
}

/// Proxy variables for tests that send requests through the CLI's clients.
#[cfg(test)]
pub(crate) mod proxy_env {
    use std::ffi::OsString;

    const PROXY_ENV: [&str; 8] = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];

    pub(crate) struct ProxyEnvGuard(Vec<(&'static str, Option<OsString>)>);

    impl ProxyEnvGuard {
        pub(crate) fn clear() -> Self {
            let saved = PROXY_ENV
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            for name in PROXY_ENV {
                // SAFETY: these tests are serialized with every other test that changes proxy
                // variables, and restore the process environment before returning.
                unsafe { std::env::remove_var(name) };
            }
            Self(saved)
        }

        pub(crate) fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) {
            // SAFETY: see `clear`; the guard owns this test's serialized environment changes.
            unsafe { std::env::set_var(name, value) };
        }
    }

    impl Drop for ProxyEnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                // SAFETY: see `clear`; restoring the saved values ends the serialized mutation.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    use jsonrpsee::core::client::ClientT as _;
    use jsonrpsee::rpc_params;
    use serial_test::serial;

    use super::proxy_env::ProxyEnvGuard;
    use super::*;

    #[test]
    #[serial]
    fn schemeless_http_proxy_routes_json_rpc_through_forwarding_proxy() {
        let _env = ProxyEnvGuard::clear();
        let (origin_url, origin_request) = spawn_json_rpc_origin();
        let (proxy_url, proxy_request) = spawn_forwarding_proxy();
        ProxyEnvGuard::set("HTTP_PROXY", proxy_url.trim_start_matches("http://"));

        let result = request_ok(&origin_url).unwrap();

        assert_eq!(result, "ok");
        assert!(origin_request.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(
            proxy_request
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .starts_with("POST http://")
        );
    }

    #[test]
    #[serial]
    fn no_proxy_bypasses_configured_proxy() {
        let _env = ProxyEnvGuard::clear();
        let (origin_url, origin_request) = spawn_json_rpc_origin();
        let (proxy_url, proxy_request) = spawn_forwarding_proxy();
        ProxyEnvGuard::set("http_proxy", &proxy_url);
        ProxyEnvGuard::set("no_proxy", "127.0.0.1");

        let result = request_ok(&origin_url).unwrap();

        assert_eq!(result, "ok");
        assert!(!origin_request.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(
            proxy_request
                .recv_timeout(Duration::from_millis(200))
                .is_err()
        );
    }

    #[test]
    #[serial]
    fn json_rpc_redirect_is_not_followed() {
        let _env = ProxyEnvGuard::clear();
        ProxyEnvGuard::set("NO_PROXY", "*");
        let (origin_url, redirected_request) = spawn_redirecting_origin();

        let error = request_ok(&origin_url).unwrap_err();

        assert!(error.to_string().contains("307"));
        assert!(
            !redirected_request
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
        );
    }

    #[test]
    #[serial]
    fn https_proxy_uses_connect_and_errors_hide_credentials() {
        let _env = ProxyEnvGuard::clear();
        let (target_url, tunneled_bytes) = spawn_tls_sink();
        let (proxy_url, proxy_request) = spawn_connect_proxy();
        let authenticated_proxy =
            proxy_url.replacen("http://", "http://proxy-user:supersecret@", 1);
        ProxyEnvGuard::set("HTTPS_PROXY", authenticated_proxy);

        let error = request_ok(&target_url).unwrap_err();
        let rendered = error.to_string();

        let target = target_url.trim_start_matches("https://");
        assert_eq!(
            proxy_request.recv_timeout(Duration::from_secs(2)).unwrap(),
            format!("CONNECT {target} HTTP/1.1")
        );
        assert!(tunneled_bytes.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(rendered.contains("outbound HTTP"));
        assert!(!rendered.contains("supersecret"));
        assert!(!rendered.contains("proxy-user"));
        assert!(!rendered.contains(&proxy_url));
    }

    #[test]
    #[serial]
    fn invalid_proxy_error_names_variable_without_echoing_value() {
        let _env = ProxyEnvGuard::clear();
        ProxyEnvGuard::set("HTTP_PROXY", "http://proxy-user:supersecret@");

        let rendered = match rpc_client("http://127.0.0.1:1") {
            Ok(_) => panic!("invalid proxy should fail client construction"),
            Err(error) => error.to_string(),
        };

        assert!(rendered.contains("HTTP_PROXY"));
        assert!(!rendered.contains("supersecret"));
        assert!(!rendered.contains("proxy-user"));
    }

    fn request_ok(url: &str) -> Result<String, Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let client = rpc_client(url)?;
            client
                .request::<String, _>("test_ok", rpc_params![])
                .await
                .map_err(Into::into)
        })
    }

    fn spawn_json_rpc_origin() -> (String, Receiver<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            sender
                .send(
                    request
                        .windows(b"x-test-proxy: routed".len())
                        .any(|window| window.eq_ignore_ascii_case(b"x-test-proxy: routed")),
                )
                .unwrap();
            write_response(
                &mut stream,
                "200 OK",
                br#"{"jsonrpc":"2.0","result":"ok","id":0}"#,
            );
        });
        (format!("http://{address}"), receiver)
    }

    fn spawn_redirecting_origin() -> (String, Receiver<bool>) {
        let redirected_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        redirected_listener.set_nonblocking(true).unwrap();
        let redirected_address = redirected_listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            sender
                .send(accept_before(&redirected_listener, Duration::from_millis(500)).is_some())
                .unwrap();
        });

        let redirect_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_address = redirect_listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = redirect_listener.accept().unwrap();
            read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{redirected_address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });

        (format!("http://{redirect_address}"), receiver)
    }

    fn spawn_forwarding_proxy() -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let Some((mut inbound, _)) = accept_before(&listener, Duration::from_secs(2)) else {
                return;
            };
            let request = read_request(&mut inbound);
            let header_end = find_header_end(&request).unwrap();
            let first_line_end = request
                .windows(2)
                .position(|window| window == b"\r\n")
                .unwrap();
            let first_line = String::from_utf8(request[..first_line_end].to_vec()).unwrap();
            sender.send(first_line.clone()).unwrap();
            let mut fields = first_line.split_whitespace();
            let method = fields.next().unwrap();
            let url = reqwest::Url::parse(fields.next().unwrap()).unwrap();
            let version = fields.next().unwrap();
            let target = format!(
                "{}:{}",
                url.host_str().unwrap(),
                url.port_or_known_default().unwrap()
            );
            let mut outbound_request =
                format!("{method} {} {version}\r\n", origin_form(&url)).into_bytes();
            outbound_request.extend_from_slice(&request[first_line_end + 2..header_end]);
            outbound_request.extend_from_slice(b"X-Test-Proxy: routed\r\n\r\n");
            outbound_request.extend_from_slice(&request[header_end + 4..]);

            let mut outbound = TcpStream::connect(target).unwrap();
            outbound.write_all(&outbound_request).unwrap();
            let mut response = Vec::new();
            outbound.read_to_end(&mut response).unwrap();
            inbound.write_all(&response).unwrap();
        });
        (format!("http://{address}"), receiver)
    }

    fn spawn_tls_sink() -> (String, Receiver<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0_u8; 1024];
            sender.send(stream.read(&mut bytes).unwrap() > 0).unwrap();
        });
        (format!("https://{address}"), receiver)
    }

    fn spawn_connect_proxy() -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let first_line_end = request
                .windows(2)
                .position(|window| window == b"\r\n")
                .unwrap();
            sender
                .send(String::from_utf8(request[..first_line_end].to_vec()).unwrap())
                .unwrap();
            let target = String::from_utf8_lossy(&request[..first_line_end])
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_owned();
            let mut outbound = TcpStream::connect(target).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut bytes = [0_u8; 4096];
            let read = stream.read(&mut bytes).unwrap();
            outbound.write_all(&bytes[..read]).unwrap();
        });
        (format!("http://{address}"), receiver)
    }

    fn accept_before(
        listener: &TcpListener,
        timeout: Duration,
    ) -> Option<(TcpStream, std::net::SocketAddr)> {
        let deadline = Instant::now() + timeout;
        loop {
            match listener.accept() {
                Ok(connection) => return Some(connection),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("proxy accept failed: {error}"),
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            assert_ne!(read, 0, "connection closed before request completed");
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = find_header_end(&request) else {
                continue;
            };
            let content_length = content_length(&request[..header_end]).unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                return request;
            }
        }
    }

    fn find_header_end(request: &[u8]) -> Option<usize> {
        request.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn content_length(headers: &[u8]) -> Option<usize> {
        String::from_utf8_lossy(headers).lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
    }

    fn origin_form(url: &reqwest::Url) -> String {
        let mut value = url.path().to_owned();
        if let Some(query) = url.query() {
            value.push('?');
            value.push_str(query);
        }
        value
    }

    fn write_response(stream: &mut TcpStream, status: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }
}
