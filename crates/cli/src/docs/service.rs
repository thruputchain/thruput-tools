//! Per-user background docs process with authenticated loopback lifecycle control.

use super::{DocsError, prepare};
use crate::{
    CliError, Print,
    docs_json::DocsReport,
    result::{CommandError, CommandResult},
};
use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use fs2::FileExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

const HEALTH: &str = "__thruput_docs/health";
const STOP: &str = "__thruput_docs/stop";
const HANDOFF: &str = "ready\n";

/// A capability used only by the managed process and its parent controller.
/// The token is intentionally excluded from Debug output and command arguments.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Control {
    token: String,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Control { .. }")
    }
}

pub(crate) fn routes(control: Option<Control>, shutdown: Arc<tokio::sync::Notify>) -> Router {
    let Some(control) = control else {
        return Router::new();
    };
    let expected = format!("Bearer {}", control.token);
    let identity = hex::encode(Sha256::digest(control.token.as_bytes()));
    let health_token = expected.clone();
    Router::new()
        .route(
            &format!("/{HEALTH}"),
            get(move |headers: HeaderMap| async move {
                if authorized(&headers, &health_token) {
                    (
                        StatusCode::NO_CONTENT,
                        [("x-thruput-docs-instance", identity)],
                    )
                } else {
                    (
                        StatusCode::FORBIDDEN,
                        [("x-thruput-docs-instance", String::new())],
                    )
                }
            }),
        )
        .route(
            &format!("/{STOP}"),
            post(move |headers: HeaderMap| async move {
                if !authorized(&headers, &expected) {
                    return StatusCode::FORBIDDEN;
                }
                // Give the HTTP response a chance to flush before dropping active connections.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    shutdown.notify_one();
                });
                StatusCode::ACCEPTED
            }),
        )
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(expected)
        && !headers.contains_key("origin")
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    schema_version: u32,
    control: Control,
    report: DocsReport,
    requested_port: u16,
    executable: PathBuf,
    log_path: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct Startup {
    root: PathBuf,
    port: u16,
    control: Control,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ServiceReport {
    schema_version: u32,
    running: bool,
    reused: bool,
    server: Option<DocsReport>,
    log_path: Option<PathBuf>,
}

/// Supported management operations for the current user's background server.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// Start or reuse a matching managed server.
    Start,
    /// Probe the recorded instance without starting it.
    Status,
    /// Stop the recorded instance without signaling unrelated processes.
    Stop,
}

/// Manage one background instance per Thruput cache directory.
pub fn run(
    action: Action,
    dir: Option<&Path>,
    port: u16,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    match manage(action, dir, port) {
        Ok(report) => {
            if let Some(server) = &report.server {
                print.success(format_args!(
                    "Documentation ready at {}{}",
                    server.url,
                    if report.reused {
                        " (already running)"
                    } else {
                        ""
                    }
                ));
                if let Some(index) = &server.markdown_index {
                    print.detail("Markdown index", index.display());
                }
            } else {
                print.status("No background documentation server is running.");
            }
            if let Some(path) = &report.log_path {
                print.detail("log", path.display());
            }
            print.json(&CommandResult::new(cmd, report))
        }
        Err(error) if print.is_json() => {
            // An error is not evidence that a running server has stopped.
            #[derive(Serialize, JsonSchema)]
            #[serde(rename_all = "camelCase")]
            struct Failure {
                schema_version: u32,
            }
            print.json(&CommandResult::failure_with_error(
                cmd,
                Failure { schema_version: 1 },
                CommandError::from_cli_error(&error),
            ))?;
            Err(CliError::CommandReportedFailure)
        }
        Err(error) => Err(error),
    }
}

fn service_error(message: impl Into<String>) -> CliError {
    DocsError::Service(message.into()).into()
}

fn read_state_file(path: &Path) -> Result<Option<State>, CliError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let state: State = serde_json::from_slice(&bytes).map_err(|_| {
        service_error("invalid state file; restore state.json or state.backup.json from a trusted copy; the server may still be running")
    })?;
    let url =
        reqwest::Url::parse(&state.report.url).map_err(|_| service_error("invalid service URL"))?;
    if state.schema_version != 1
        || url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.path() != "/"
        || url.port_or_known_default().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || state.control.token.len() != 64
        || !state.control.token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(service_error("invalid service identity"));
    }
    Ok(Some(state))
}

fn write_state(path: &Path, state: &State) -> Result<(), CliError> {
    let directory = path
        .parent()
        .ok_or_else(|| service_error("missing state directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&serde_json::to_vec(state).map_err(DocsError::Json)?)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn read_state(path: &Path) -> Result<Option<State>, CliError> {
    let primary = read_state_file(path);
    if matches!(primary, Ok(Some(_))) {
        return primary;
    }
    // Preserve the authenticated control identity if one cache copy is damaged.
    // Never discard both copies or report a server stopped merely because parsing failed.
    match read_state_file(&path.with_file_name("state.backup.json")) {
        Ok(Some(state)) => {
            write_state(path, &state)?;
            Ok(Some(state))
        }
        Ok(None) => primary,
        Err(error) => Err(error),
    }
}

fn remove_state(path: &Path) -> Result<(), CliError> {
    for path in [path.to_owned(), path.with_file_name("state.backup.json")] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn client() -> Result<reqwest::blocking::Client, CliError> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| service_error(error.to_string()))
}

/// What a health probe learned about the recorded instance.
enum Health {
    Running,
    /// Nothing listens, or another process answers on the recorded port.
    Gone,
    /// A connection opened but no complete answer arrived, as when the server
    /// drops connections while it exits.
    Unanswered,
}

fn health(state: &State) -> Result<Health, CliError> {
    match client()?
        .get(format!("{}{HEALTH}", state.report.url))
        .bearer_auth(&state.control.token)
        .send()
    {
        Ok(response)
            if response.status() == StatusCode::NO_CONTENT
                && response
                    .headers()
                    .get("x-thruput-docs-instance")
                    .and_then(|value| value.to_str().ok())
                    == Some(
                        hex::encode(Sha256::digest(state.control.token.as_bytes())).as_str(),
                    ) =>
        {
            Ok(Health::Running)
        }
        Ok(_) => Ok(Health::Gone), // Another process may have reused the port; never signal its PID.
        Err(error) if error.is_connect() => Ok(Health::Gone),
        Err(_) => Ok(Health::Unanswered),
    }
}

fn running(state: &State) -> Result<bool, CliError> {
    match health(state)? {
        Health::Running => Ok(true),
        Health::Gone => Ok(false),
        Health::Unanswered => Err(service_error(
            "server did not answer; inspect its log and retry",
        )),
    }
}

/// Wait for an instance that accepted a stop request to go away. An exiting
/// server can accept a probe and then drop it, so an unanswered probe means
/// it is still stopping, not that the stop failed.
fn await_stop(state: &State, timeout: Duration) -> Result<(), CliError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Health::Gone = health(state)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(service_error("server has not stopped; inspect its log"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn manage(action: Action, dir: Option<&Path>, port: u16) -> Result<ServiceReport, CliError> {
    let cache = crate::config::cache_dir()?.join("docs-service");
    fs::create_dir_all(&cache)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("control.lock"))?;
    lock.lock_exclusive()?;
    let state_path = cache.join("state.json");
    let state = match read_state(&state_path)? {
        Some(state) if running(&state)? => Some(state),
        Some(_) => {
            remove_state(&state_path)?;
            None
        }
        None => None,
    };
    let empty = || ServiceReport {
        schema_version: 1,
        running: false,
        reused: false,
        server: None,
        log_path: None,
    };
    match action {
        Action::Status => Ok(state.map_or_else(empty, |state| report(state, false))),
        Action::Stop => {
            if let Some(state) = state {
                let response = client()?
                    .post(format!("{}{STOP}", state.report.url))
                    .bearer_auth(&state.control.token)
                    .send()
                    .map_err(|_| {
                        service_error("could not stop server; inspect its log and retry")
                    })?;
                if response.status() != StatusCode::ACCEPTED {
                    return Err(service_error("server rejected stop request"));
                }
                await_stop(&state, Duration::from_secs(5))?;
                remove_state(&state_path)?;
            }
            Ok(empty())
        }
        Action::Start => {
            let (root, snapshot) = prepare(dir)?;
            let executable = std::env::current_exe()?.canonicalize()?;
            if let Some(state) = state {
                if state.report.path != root
                    || state.requested_port != port
                    || state.executable != executable
                    || state.report.snapshot != snapshot
                    || state.report.cli_version != env!("CARGO_PKG_VERSION")
                {
                    return Err(service_error(
                        "a different docs configuration is running; run `thruput docs stop` first",
                    ));
                }
                return Ok(report(state, true));
            }
            let (state, mut child) = spawn(&cache, root, port, &executable)?;
            let saved = (|| -> Result<(), CliError> {
                write_state(&state_path, &state)?;
                write_state(&state_path.with_file_name("state.backup.json"), &state)?;
                // The child remains tied to this pipe until its controller state
                // is durable. Parent interruption before this point closes stdin
                // and stops the otherwise untracked background process.
                let mut pipe = child
                    .stdin
                    .take()
                    .ok_or_else(|| service_error("missing handoff pipe"))?;
                pipe.write_all(HANDOFF.as_bytes())?;
                Ok(())
            })();
            if let Err(error) = saved {
                let _ = child.kill();
                let _ = child.wait();
                let _ = remove_state(&state_path);
                return Err(error);
            }
            Ok(report(state, false))
        }
    }
}

fn report(state: State, reused: bool) -> ServiceReport {
    ServiceReport {
        schema_version: 1,
        running: true,
        reused,
        server: Some(state.report),
        log_path: Some(state.log_path),
    }
}

#[cfg(unix)]
fn spawn(
    cache: &Path,
    root: PathBuf,
    port: u16,
    executable: &Path,
) -> Result<(State, Child), CliError> {
    use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token)
        .map_err(|_| service_error("could not generate service identity"))?;
    let control = Control {
        token: hex::encode(token),
    };
    let startup = Startup {
        root,
        port,
        control: control.clone(),
    };
    let ready = tempfile::NamedTempFile::new_in(cache)?;
    let log_path = cache.join("server.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut command = Command::new(executable);
    command
        .args(["docs", "--managed", "--non-interactive", "--json"])
        .current_dir(cache)
        .stdin(Stdio::piped())
        .stdout(ready.as_file().try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe; no allocations or locks occur after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn()?;
    let result = (|| {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| service_error("missing startup pipe"))?;
        stdin.write_all(&serde_json::to_vec(&startup).map_err(DocsError::Json)?)?;
        stdin.write_all(b"\n")?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.try_wait()?.is_some() {
                return Err(service_error(format!(
                    "startup failed; see {}",
                    log_path.display()
                )));
            }
            let text = fs::read(ready.path())?;
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&text)
                && value["ok"] == true
                && let Ok(report) = serde_json::from_value::<DocsReport>(value["data"].clone())
            {
                let state = State {
                    schema_version: 1,
                    control: control.clone(),
                    report,
                    requested_port: port,
                    executable: executable.to_owned(),
                    log_path: log_path.clone(),
                };
                if running(&state)? {
                    return Ok(state);
                }
            }
            if Instant::now() >= deadline {
                return Err(service_error(format!(
                    "startup timed out; see {}",
                    log_path.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    match result {
        Ok(state) => Ok((state, child)),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn spawn(_: &Path, _: PathBuf, _: u16, _: &Path) -> Result<(State, Child), CliError> {
    Err(service_error(
        "background mode requires Unix; use foreground mode on this platform",
    ))
}

/// Internal child entry point. Startup configuration is received over a private pipe.
#[doc(hidden)]
pub fn managed(print: &Print) -> Result<(), CliError> {
    let mut pipe = BufReader::new(std::io::stdin());
    let mut input = String::new();
    pipe.by_ref().take(64 * 1024).read_line(&mut input)?;
    let startup: Startup =
        serde_json::from_str(&input).map_err(|_| service_error("invalid startup configuration"))?;
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let abandoned = shutdown.clone();
    std::thread::spawn(move || {
        let mut handoff = String::new();
        if pipe.take(32).read_line(&mut handoff).is_err() || handoff != HANDOFF {
            abandoned.notify_one();
        }
    });
    super::serve(
        Some(&startup.root),
        startup.port,
        "docs",
        print,
        Some(startup.control),
        &shutdown,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs_json::Snapshot;
    use std::net::TcpListener;

    fn state_at(url: String) -> State {
        State {
            schema_version: 1,
            control: Control {
                token: "0".repeat(64),
            },
            report: DocsReport {
                schema_version: 1,
                url,
                path: PathBuf::new(),
                snapshot: Snapshot {
                    schema_version: 1,
                    source_revision: String::new(),
                    source_dirty: false,
                    agent_guide_cli_version: String::new(),
                },
                cli_version: String::new(),
                guide_matches_cli: true,
                markdown_index: None,
            },
            requested_port: 0,
            executable: PathBuf::new(),
            log_path: PathBuf::new(),
        }
    }

    // An exiting server accepts a probe, drops it unanswered, then stops
    // listening. The stop must wait that out instead of failing.
    #[test]
    fn stop_waits_out_a_probe_dropped_during_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let state = state_at(format!("http://{}/", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut request = BufReader::new(socket);
            let mut line = String::new();
            while request.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
        });
        await_stop(&state, Duration::from_secs(5)).unwrap();
        server.join().unwrap();
    }
}
