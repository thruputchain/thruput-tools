//! hogd-dev process adapter.

use std::{
    env,
    ffi::OsString,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{ChildStderr, ChildStdout},
    task::JoinHandle,
    time,
};

use crate::{
    ChildProcessSpec, ChildSupervisor, HogdDevConfig, NodeSupervisorError, SupervisedChild,
};

/// Default hogd-dev binary name.
pub const DEFAULT_HOGD_DEV_BINARY: &str = "hogd-dev";

/// Environment variable used to override the hogd-dev binary path.
pub const HOGD_DEV_BINARY_ENV: &str = "GROUNDHOG_HOGD_DEV_BINARY";

/// Maximum accepted hogd-dev readiness line length.
pub const MAX_READINESS_LINE_BYTES: usize = 64 * 1024;

/// Ready endpoints exposed by hogd-dev.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct HogdDevEndpoints {
    /// Scheduler/submit endpoint.
    pub scheduler: SocketAddr,
    /// Read endpoint. Absent in the v1 reduced contract.
    #[serde(default)]
    pub read: Option<SocketAddr>,
    /// Dev-control endpoint. Absent in the v1 reduced contract.
    #[serde(default)]
    pub control: Option<SocketAddr>,
}

/// hogd-dev readiness payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct HogdDevReadiness {
    /// Readiness status. Must be `ready`.
    pub status: String,
    /// Local chain network identifier.
    pub network_id: String,
    /// x23 endpoints available after startup.
    pub endpoints: HogdDevEndpoints,
}

impl HogdDevReadiness {
    fn ensure_ready(self, program: &str) -> Result<Self, NodeSupervisorError> {
        if self.status == "ready" {
            return Ok(self);
        }
        Err(NodeSupervisorError::InvalidReadinessStatus {
            program: program.to_string(),
            status: self.status,
        })
    }
}

/// Running hogd-dev child process plus its parsed readiness metadata.
#[derive(Debug)]
pub struct HogdDevProcess {
    readiness: HogdDevReadiness,
    child: SupervisedChild,
    stderr_task: Option<JoinHandle<()>>,
    shutdown_grace: Duration,
}

impl HogdDevProcess {
    /// Start hogd-dev using the configured binary lookup.
    #[tracing::instrument(skip_all, fields(listen_host = %config.listen_host))]
    pub async fn start(config: &HogdDevConfig) -> Result<Self, NodeSupervisorError> {
        let binary = resolve_hogd_dev_binary(config)?;
        Self::start_with_binary(config, binary).await
    }

    /// Start hogd-dev from an already-resolved binary path.
    #[tracing::instrument(skip_all, fields(binary = %binary.as_ref().display()))]
    pub async fn start_with_binary(
        config: &HogdDevConfig,
        binary: impl AsRef<Path>,
    ) -> Result<Self, NodeSupervisorError> {
        let spec = build_hogd_dev_spec(binary.as_ref(), config);
        Self::start_with_spec(config, spec).await
    }

    async fn start_with_spec(
        config: &HogdDevConfig,
        spec: ChildProcessSpec,
    ) -> Result<Self, NodeSupervisorError> {
        let mut child = ChildSupervisor::new(spec).spawn()?;
        let program = child.program().to_string();
        let stdout = child
            .take_stdout()
            .ok_or_else(|| NodeSupervisorError::MissingStdout {
                program: program.clone(),
            })?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| NodeSupervisorError::MissingStderr {
                program: program.clone(),
            })?;
        let mut stderr_task = Some(spawn_stderr_forwarder(stderr));

        let readiness_result = read_readiness_line(
            &mut child,
            stdout,
            config.startup_timeout,
            stderr_task.as_ref(),
        )
        .await;
        let readiness = match readiness_result {
            Ok(readiness) => readiness,
            Err(error) => {
                wait_for_stderr_task(stderr_task.take()).await;
                return Err(error);
            }
        };

        Ok(Self {
            readiness,
            child,
            stderr_task,
            shutdown_grace: config.shutdown_grace,
        })
    }

    /// Parsed readiness metadata.
    pub fn readiness(&self) -> &HogdDevReadiness {
        &self.readiness
    }

    /// Operating-system process id for hogd-dev, if available.
    pub fn child_id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wait for hogd-dev to exit.
    #[tracing::instrument(skip_all)]
    pub async fn stopped(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        let status = self.child.wait().await?;
        self.wait_for_stderr_task().await;
        Ok(status)
    }

    /// Gracefully stop hogd-dev, escalating after the configured timeout.
    #[tracing::instrument(skip_all, fields(shutdown_grace = ?self.shutdown_grace))]
    pub async fn stop(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        #[cfg(not(unix))]
        if let Some(status) = self.child.try_wait()? {
            self.wait_for_stderr_task().await;
            return Ok(status);
        }

        #[cfg(unix)]
        self.child.terminate_process_group()?;

        #[cfg(unix)]
        let status = self.stop_process_group().await?;

        #[cfg(not(unix))]
        let status = self.child.kill_and_wait().await?;

        self.wait_for_stderr_task().await;
        Ok(status)
    }

    #[cfg(unix)]
    async fn stop_process_group(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        let deadline = time::Instant::now() + self.shutdown_grace;
        let mut status = None;

        loop {
            if status.is_none() {
                status = self.child.try_wait()?;
            }

            if !self.child.process_group_exists()? {
                break;
            }

            let now = time::Instant::now();
            if now >= deadline {
                self.child.kill_process_group()?;
                break;
            }

            time::sleep((deadline - now).min(Duration::from_millis(10))).await;
        }

        match status {
            Some(status) => Ok(status),
            None => self.child.wait().await,
        }
    }

    async fn wait_for_stderr_task(&mut self) {
        wait_for_stderr_task(self.stderr_task.take()).await;
    }
}

/// Build the hogd-dev child process command specification.
pub fn build_hogd_dev_spec(binary: impl AsRef<Path>, config: &HogdDevConfig) -> ChildProcessSpec {
    let spec = ChildProcessSpec::new(binary.as_ref().as_os_str().to_owned())
        .args([
            OsString::from("--listen-host"),
            OsString::from(config.listen_host.as_str()),
            OsString::from("--scheduler-port"),
            OsString::from("0"),
            OsString::from("--readiness"),
            OsString::from("stdout"),
        ])
        .stdout_piped()
        .stderr_piped();

    #[cfg(unix)]
    let spec = spec.process_group(0);

    spec
}

/// Resolve the hogd-dev binary path using the v1 lookup order.
pub fn resolve_hogd_dev_binary(config: &HogdDevConfig) -> Result<PathBuf, NodeSupervisorError> {
    if let Some(binary_path) = &config.binary_path {
        return validate_configured_binary(binary_path);
    }

    if let Some(env_path) = env::var_os(HOGD_DEV_BINARY_ENV) {
        return validate_configured_binary(Path::new(&env_path));
    }

    let current_exe =
        env::current_exe().map_err(|source| NodeSupervisorError::CurrentExe { source })?;
    if let Some(parent) = current_exe.parent() {
        let sibling = parent.join(DEFAULT_HOGD_DEV_BINARY);
        if is_executable_file(&sibling) {
            return Ok(sibling);
        }
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(DEFAULT_HOGD_DEV_BINARY);
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(NodeSupervisorError::HogdDevNotFound)
}

/// Parse one hogd-dev readiness line.
pub fn parse_readiness_line(
    program: &str,
    line: &str,
) -> Result<HogdDevReadiness, NodeSupervisorError> {
    let readiness = serde_json::from_str::<HogdDevReadiness>(line).map_err(|source| {
        NodeSupervisorError::InvalidReadinessJson {
            program: program.to_string(),
            line: line.to_string(),
            source,
        }
    })?;
    readiness.ensure_ready(program)
}

fn validate_configured_binary(path: &Path) -> Result<PathBuf, NodeSupervisorError> {
    if !path.exists() {
        return Err(NodeSupervisorError::ConfiguredPathMissing {
            path: path.to_path_buf(),
        });
    }

    if !is_executable_file(path) {
        return Err(NodeSupervisorError::ConfiguredPathNotExecutable {
            path: path.to_path_buf(),
        });
    }

    Ok(path.to_path_buf())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = path.metadata() else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

async fn read_readiness_line(
    child: &mut SupervisedChild,
    stdout: ChildStdout,
    timeout: Duration,
    stderr_task: Option<&JoinHandle<()>>,
) -> Result<HogdDevReadiness, NodeSupervisorError> {
    let program = child.program().to_string();
    let read_result =
        time::timeout(timeout, read_capped_line(stdout, MAX_READINESS_LINE_BYTES)).await;
    match read_result {
        Ok(Ok(CappedLine::Eof)) => {
            abort_stderr_task(stderr_task);
            // EOF on stdout means the child has either already exited or is
            // about to. `try_wait()` polls `waitpid`, which races SIGCHLD
            // delivery on Linux — so a tight EOF→try_wait sequence can see
            // `None` and misclassify a clean exit as `ReadinessRead`. Bridge
            // the race with a short `wait()` before giving up.
            if let Some(status) = wait_briefly_for_exit(child).await? {
                return Err(NodeSupervisorError::ExitedBeforeReady { program, status });
            }
            kill_unready_child(child).await;
            Err(NodeSupervisorError::ReadinessRead {
                program,
                source: io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "child closed stdout before readiness",
                ),
            })
        }
        Ok(Ok(CappedLine::Line(line))) => {
            let line = line.trim_end_matches(['\r', '\n']);
            match parse_readiness_line(&program, line) {
                Ok(readiness) => Ok(readiness),
                Err(error) => {
                    kill_unready_child(child).await;
                    abort_stderr_task(stderr_task);
                    Err(error)
                }
            }
        }
        Ok(Ok(CappedLine::TooLarge)) => {
            kill_unready_child(child).await;
            abort_stderr_task(stderr_task);
            Err(NodeSupervisorError::ReadinessTooLarge {
                program,
                max_bytes: MAX_READINESS_LINE_BYTES,
            })
        }
        Ok(Err(source)) => {
            kill_unready_child(child).await;
            abort_stderr_task(stderr_task);
            Err(NodeSupervisorError::ReadinessRead { program, source })
        }
        Err(_) => {
            kill_unready_child(child).await;
            abort_stderr_task(stderr_task);
            Err(NodeSupervisorError::ReadinessTimeout { program, timeout })
        }
    }
}

/// Bridge the EOF→`try_wait` race after the child closes stdout.
///
/// Tries `try_wait()` first. If the child has not been reaped yet, awaits
/// `wait()` with a short cap — long enough to outlast any SIGCHLD delivery
/// jitter, short enough that a genuinely-stuck child still hits the cap and
/// gets force-killed by the caller.
async fn wait_briefly_for_exit(
    child: &mut SupervisedChild,
) -> Result<Option<ExitStatus>, NodeSupervisorError> {
    if let Some(status) = child.try_wait()? {
        return Ok(Some(status));
    }
    match time::timeout(Duration::from_millis(100), child.wait()).await {
        Ok(Ok(status)) => Ok(Some(status)),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(None),
    }
}

enum CappedLine {
    Line(String),
    Eof,
    TooLarge,
}

async fn read_capped_line(
    mut stdout: ChildStdout,
    max_bytes: usize,
) -> Result<CappedLine, io::Error> {
    let mut line = Vec::new();
    let mut chunk = [0_u8; 1024];

    loop {
        let read = stdout.read(&mut chunk).await?;
        if read == 0 {
            if line.is_empty() {
                return Ok(CappedLine::Eof);
            }
            return Ok(CappedLine::Line(
                String::from_utf8_lossy(&line).into_owned(),
            ));
        }

        let bytes = &chunk[..read];
        let line_end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(read, |position| position + 1);

        if line.len().saturating_add(line_end) > max_bytes {
            return Ok(CappedLine::TooLarge);
        }

        line.extend_from_slice(&bytes[..line_end]);
        if line_end < read || line.ends_with(b"\n") {
            return Ok(CappedLine::Line(
                String::from_utf8_lossy(&line).into_owned(),
            ));
        }
    }
}

async fn kill_unready_child(child: &mut SupervisedChild) {
    #[cfg(unix)]
    {
        if let Err(error) = child.kill_process_group() {
            tracing::warn!(
                target: "hogd_dev",
                %error,
                program = child.program(),
                "failed to kill unready hogd-dev process group"
            );
        }
        if let Err(error) = child.wait().await {
            tracing::warn!(
                target: "hogd_dev",
                %error,
                program = child.program(),
                "failed waiting for unready hogd-dev cleanup"
            );
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(error) = child.kill_and_wait().await {
            tracing::warn!(
                target: "hogd_dev",
                %error,
                program = child.program(),
                "failed to kill unready hogd-dev child"
            );
        }
    }
}

fn spawn_stderr_forwarder(stderr: ChildStderr) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr);
        let mut line = String::new();

        loop {
            line.clear();
            match stderr.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => forward_log_line(line.trim_end_matches(['\r', '\n'])),
                Err(error) => {
                    tracing::warn!(target: "hogd_dev", %error, "failed to read hogd-dev stderr");
                    break;
                }
            }
        }
    })
}

#[derive(Debug, Deserialize)]
struct HogdDevLogLine {
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    fields: Option<serde_json::Value>,
}

fn forward_log_line(line: &str) {
    match serde_json::from_str::<HogdDevLogLine>(line) {
        Ok(record) => {
            let level = record.level.as_deref();
            let hogd_target = record.target.as_deref().unwrap_or("hogd_dev");
            let message = record.message.as_deref().unwrap_or("");
            let fields = record
                .fields
                .as_ref()
                .map_or_else(String::new, serde_json::Value::to_string);

            if level.is_some_and(|level| level.eq_ignore_ascii_case("error")) {
                tracing::error!(
                    target: "hogd_dev",
                    %hogd_target,
                    %fields,
                    "{message}"
                );
            } else if level.is_some_and(|level| {
                level.eq_ignore_ascii_case("warn") || level.eq_ignore_ascii_case("warning")
            }) {
                tracing::warn!(
                    target: "hogd_dev",
                    %hogd_target,
                    %fields,
                    "{message}"
                );
            } else if level.is_some_and(|level| level.eq_ignore_ascii_case("debug")) {
                tracing::debug!(
                    target: "hogd_dev",
                    %hogd_target,
                    %fields,
                    "{message}"
                );
            } else if level.is_some_and(|level| level.eq_ignore_ascii_case("trace")) {
                tracing::trace!(
                    target: "hogd_dev",
                    %hogd_target,
                    %fields,
                    "{message}"
                );
            } else if level.is_some_and(|level| level.eq_ignore_ascii_case("info")) {
                tracing::info!(
                    target: "hogd_dev",
                    %hogd_target,
                    %fields,
                    "{message}"
                );
            } else {
                let level = level.unwrap_or("<missing>");
                tracing::warn!(
                    target: "hogd_dev",
                    %hogd_target,
                    %fields,
                    %level,
                    "{message}"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                target: "hogd_dev",
                %error,
                line,
                "invalid hogd-dev stderr log line"
            );
        }
    }
}

async fn wait_for_stderr_task(task: Option<JoinHandle<()>>) {
    if let Some(task) = task {
        let _ = task.await;
    }
}

fn abort_stderr_task(task: Option<&JoinHandle<()>>) {
    if let Some(task) = task {
        task.abort();
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::{
        env,
        ffi::OsString,
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use serial_test::serial;
    use tempfile::TempDir;

    use super::{
        DEFAULT_HOGD_DEV_BINARY, HOGD_DEV_BINARY_ENV, HogdDevProcess, build_hogd_dev_spec,
        parse_readiness_line, resolve_hogd_dev_binary,
    };
    use crate::{
        HogdDevConfig, NodeSupervisorError,
        test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
    };

    const READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","control":"127.0.0.1:12347"}}"#;
    const V1_READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345"}}"#;

    #[test]
    fn command_shape_matches_contract() {
        let config = HogdDevConfig::default().with_listen_host("127.0.0.2");
        let spec = build_hogd_dev_spec("/tmp/hogd-dev", &config);

        assert_eq!(spec.program(), std::ffi::OsStr::new("/tmp/hogd-dev"));
        let args = spec.program_args();
        assert_eq!(
            args,
            &[
                OsString::from("--listen-host"),
                OsString::from("127.0.0.2"),
                OsString::from("--scheduler-port"),
                OsString::from("0"),
                OsString::from("--readiness"),
                OsString::from("stdout"),
            ]
        );
    }

    #[test]
    fn readiness_line_parses_endpoint_addresses() {
        let readiness =
            parse_readiness_line("hogd-dev", READY).expect("bug: readiness payload should parse");

        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.network_id, "groundhog-local");
        assert_eq!(
            readiness.endpoints.scheduler,
            "127.0.0.1:12345"
                .parse()
                .expect("bug: address should parse")
        );
        assert_eq!(
            readiness.endpoints.read,
            Some(
                "127.0.0.1:12346"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
        assert_eq!(
            readiness.endpoints.control,
            Some(
                "127.0.0.1:12347"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
    }

    #[test]
    fn readiness_line_accepts_v1_scheduler_only_endpoint() {
        let readiness = parse_readiness_line("hogd-dev", V1_READY)
            .expect("bug: v1 readiness payload should parse");

        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.network_id, "groundhog-local");
        assert_eq!(
            readiness.endpoints.scheduler,
            "127.0.0.1:12345"
                .parse()
                .expect("bug: address should parse")
        );
        assert_eq!(readiness.endpoints.read, None);
        assert_eq!(readiness.endpoints.control, None);
    }

    #[test]
    fn non_ready_status_is_rejected() {
        let err = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"starting","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","control":"127.0.0.1:12347"}}"#,
        )
        .expect_err("bug: non-ready status should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidReadinessStatus { status, .. } if status == "starting"
        ));
    }

    #[test]
    fn configured_binary_must_exist() {
        let config = HogdDevConfig::default().with_binary_path(missing_path());
        let err = resolve_hogd_dev_binary(&config).expect_err("bug: missing path should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::ConfiguredPathMissing { .. }
        ));
    }

    #[test]
    fn configured_binary_must_be_executable() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = tmp.path().join(DEFAULT_HOGD_DEV_BINARY);
        fs::write(&path, b"").expect("bug: file should be written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("bug: permissions should be set");
        let config = HogdDevConfig::default().with_binary_path(path);
        let err =
            resolve_hogd_dev_binary(&config).expect_err("bug: non-executable path should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::ConfiguredPathNotExecutable { .. }
        ));
    }

    #[test]
    fn explicit_binary_path_resolves() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
        let config = HogdDevConfig::default().with_binary_path(path.clone());

        assert_eq!(
            resolve_hogd_dev_binary(&config).expect("bug: path should resolve"),
            path
        );
    }

    #[tokio::test]
    async fn fake_child_reports_readiness_and_stops_cleanly() {
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_millis(500));

        let mut process = start_fake_hogd_dev(&config, "ready")
            .await
            .expect("bug: fake hogd-dev should start");

        assert_eq!(process.readiness().network_id, "groundhog-local");

        let status = process.stop().await.expect("bug: fake child should stop");
        assert!(status.success());
    }

    #[tokio::test]
    async fn fake_child_ignoring_sigterm_is_killed_after_grace() {
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_millis(100));

        let mut process = start_fake_hogd_dev(&config, "ignore-term")
            .await
            .expect("bug: fake hogd-dev should start");

        let status = process
            .stop()
            .await
            .expect("bug: SIGKILL escalation should return child status");
        assert!(!status.success());
    }

    #[tokio::test]
    async fn fake_child_helper_is_killed_after_leader_exits() {
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_millis(100));

        let mut process = start_fake_hogd_dev(&config, "spawn-helper-ignore-term")
            .await
            .expect("bug: fake hogd-dev should start");
        let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
            .expect("bug: child pid should fit in pid_t");

        let status = process.stop().await.expect("bug: fake child should stop");
        assert!(status.success());
        assert_process_group_exits(process_group).await;
    }

    #[tokio::test]
    async fn fake_child_exit_before_readiness_is_typed() {
        let config = HogdDevConfig::default().with_startup_timeout(Duration::from_secs(2));

        let err = start_fake_hogd_dev(&config, "exit-before-ready")
            .await
            .expect_err("bug: early child exit should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::ExitedBeforeReady { status, .. } if status.code() == Some(9)
        ));
    }

    #[tokio::test]
    async fn startup_timeout_kills_fake_child() {
        let config = HogdDevConfig::default().with_startup_timeout(Duration::from_millis(100));

        let err = start_fake_hogd_dev(&config, "timeout")
            .await
            .expect_err("bug: startup should time out");

        assert!(
            matches!(err, NodeSupervisorError::ReadinessTimeout { .. }),
            "unexpected startup error: {err:?}"
        );
    }

    #[tokio::test]
    async fn oversized_readiness_line_is_rejected() {
        let config = HogdDevConfig::default().with_startup_timeout(Duration::from_secs(2));

        let err = start_fake_hogd_dev(&config, "oversized-readiness")
            .await
            .expect_err("bug: oversized readiness should fail");

        assert!(
            matches!(err, NodeSupervisorError::ReadinessTooLarge { .. }),
            "unexpected oversized readiness error: {err:?}"
        );
    }

    async fn start_fake_hogd_dev(
        config: &HogdDevConfig,
        mode: &str,
    ) -> Result<HogdDevProcess, NodeSupervisorError> {
        let spec =
            build_hogd_dev_spec(fake_hogd_dev_binary(), config).env(FAKE_HOGD_DEV_MODE_ENV, mode);
        HogdDevProcess::start_with_spec(config, spec).await
    }

    async fn assert_process_group_exits(process_group: i32) {
        for _ in 0..50 {
            if !process_group_exists(process_group) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!("bug: process group {process_group} should exit");
    }

    fn process_group_exists(process_group: i32) -> bool {
        // SAFETY: kill(2) with signal 0 only checks for process-group
        // existence and does not deliver a signal.
        let result = unsafe { libc::kill(-process_group, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn executable_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"").expect("bug: executable file should be written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("bug: permissions should be set");
        path
    }

    fn missing_path() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("bug: system time should be after epoch")
            .as_nanos();
        env::temp_dir().join(format!("missing-hogd-dev-{unique}"))
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            unsafe { env::remove_var(key) };
            Self { key, previous }
        }

        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = env::var_os(key);
            unsafe { env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { env::set_var(self.key, value) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    #[serial]
    fn env_binary_path_resolves() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
        let _guard = EnvGuard::set(HOGD_DEV_BINARY_ENV, &path);

        assert_eq!(
            resolve_hogd_dev_binary(&HogdDevConfig::default())
                .expect("bug: env path should resolve"),
            path
        );
    }

    #[test]
    #[serial]
    fn path_binary_resolves() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
        let _env_guard = EnvGuard::remove(HOGD_DEV_BINARY_ENV);
        let _path_guard = EnvGuard::set("PATH", tmp.path());

        assert_eq!(
            resolve_hogd_dev_binary(&HogdDevConfig::default())
                .expect("bug: PATH binary should resolve"),
            path
        );
    }

    #[test]
    #[serial]
    fn sibling_binary_path_resolves() {
        let current_exe = env::current_exe().expect("bug: current exe should resolve");
        let sibling_dir = current_exe
            .parent()
            .expect("bug: current exe should have a parent");
        let sibling = sibling_dir.join(DEFAULT_HOGD_DEV_BINARY);
        assert!(
            !sibling.exists(),
            "test would clobber existing sibling hogd-dev at {}",
            sibling.display()
        );
        fs::write(&sibling, b"").expect("bug: sibling file should be written");
        fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755))
            .expect("bug: sibling permissions should be set");
        let _file_guard = FileGuard {
            path: sibling.clone(),
        };
        let _env_guard = EnvGuard::remove(HOGD_DEV_BINARY_ENV);
        let empty_path = TempDir::new().expect("bug: temp dir should be created");
        let _path_guard = EnvGuard::set("PATH", empty_path.path());

        assert_eq!(
            resolve_hogd_dev_binary(&HogdDevConfig::default())
                .expect("bug: sibling binary should resolve"),
            sibling
        );
    }

    #[test]
    #[serial]
    fn missing_binary_returns_typed_error() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let _env_guard = EnvGuard::remove(HOGD_DEV_BINARY_ENV);
        let _path_guard = EnvGuard::set("PATH", tmp.path());

        let err = resolve_hogd_dev_binary(&HogdDevConfig::default())
            .expect_err("bug: missing hogd-dev should fail");

        assert!(matches!(err, NodeSupervisorError::HogdDevNotFound));
    }
}
