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

use groundhog_rpc_api_types::{ContractAddress, dev::DevAccount, hex::HexBytes};
use groundhog_signing::LocalSigner;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    process::{ChildStderr, ChildStdout},
    task::JoinHandle,
    time,
};

use crate::{
    ChildProcessSpec, ChildSupervisor, HogdDevConfig, NodeSupervisorError, SupervisedChild,
    pid_file::PidGuard,
};

/// Default hogd-dev binary name.
pub const DEFAULT_HOGD_DEV_BINARY: &str = "hogd-dev";

/// Environment variable used to override the hogd-dev binary path.
pub const HOGD_DEV_BINARY_ENV: &str = "GROUNDHOG_HOGD_DEV_BINARY";

/// Environment variable used to request structured JSON logs from hogd-dev.
pub const HOGD_LOG_FORMAT_ENV: &str = "HOGD_LOG_FORMAT";

/// Maximum accepted hogd-dev readiness line length.
pub const MAX_READINESS_LINE_BYTES: usize = 64 * 1024;

/// Maximum accepted hogd-dev stderr log line length.
pub const MAX_LOG_LINE_BYTES: usize = 64 * 1024;

/// Ready endpoints exposed by hogd-dev.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct HogdDevEndpoints {
    /// `SchedulerCoord` endpoint. Also accepts hogd-dev's compact
    /// readiness key `scheduler`.
    ///
    /// Required for submit, chain tip, and pending tx probes.
    #[serde(alias = "scheduler")]
    pub scheduler_coord: SocketAddr,
    /// `IndexerBlockExplorer` endpoint. Also accepts hogd-dev's compact
    /// readiness key `read`. Absent in the reduced contract.
    #[serde(default, alias = "read")]
    pub indexer_block_explorer: Option<SocketAddr>,
    /// `DbGateway` endpoint for latest-state storage reads. Also accepts
    /// hogd-dev's compact readiness key `gateway`.
    #[serde(default, alias = "gateway")]
    pub db_gateway: Option<SocketAddr>,
    /// `ViewGateway` endpoint for sandboxed contract simulation. Also accepts
    /// hogd-dev's compact readiness key `view`.
    #[serde(default, alias = "view")]
    pub view_gateway: Option<SocketAddr>,
    /// Dev-control endpoint. Absent in the reduced contract.
    #[serde(default)]
    pub dev_control: Option<SocketAddr>,
}

/// Public dev-account metadata advertised by hogd-dev readiness.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub(crate) struct HogdDevReadyAccount {
    /// Deterministic dev-account index.
    pub index: u32,
    /// Wallet contract address bootstrapped by hogd-dev.
    pub address: ContractAddress,
    /// Ed25519 public key authorized by the wallet contract.
    pub public_key: HexBytes,
    /// Storage-token balance at bootstrap; not refreshed thereafter.
    pub initial_storage_tokens: u64,
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
    /// Public dev-account metadata advertised by hogd-dev.
    #[serde(default)]
    pub(crate) dev_accounts: Vec<HogdDevReadyAccount>,
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

/// Build RPC-facing dev accounts from hogd-dev readiness plus local signer derivation.
pub(crate) fn build_dev_accounts(
    readiness: &HogdDevReadiness,
    config: &HogdDevConfig,
) -> Result<Vec<DevAccount>, NodeSupervisorError> {
    validate_dev_account_count(config.dev_account_count, readiness.dev_accounts.len())?;

    readiness
        .dev_accounts
        .iter()
        .enumerate()
        .map(|(position, account)| {
            validate_dev_account_index(position, account.index)?;
            let signer =
                groundhog_signing::derive_dev_account_signer(&config.dev_account_seed, position);
            validate_dev_account_public_key(position, account, &signer)?;

            Ok(DevAccount {
                address: account.address,
                public_key: account.public_key.clone(),
                private_key: HexBytes(signer.to_keypair_bytes()[..32].to_vec()),
                initial_storage_tokens: account.initial_storage_tokens,
            })
        })
        .collect()
}

/// Running hogd-dev child process plus its parsed readiness metadata.
#[derive(Debug)]
pub struct HogdDevProcess {
    readiness: HogdDevReadiness,
    child: SupervisedChild,
    stderr_task: Option<JoinHandle<()>>,
    shutdown_grace: Duration,
    pid_guard: Option<PidGuard>,
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
        let mut pid_guard = match &config.pid_file {
            Some(pid_file) => Some(
                PidGuard::acquire(pid_file.clone(), spec.program(), config.shutdown_grace).await?,
            ),
            None => None,
        };

        let mut child = ChildSupervisor::new(spec).spawn()?;
        let program = child.program().to_string();
        if let Some(guard) = &mut pid_guard {
            let pid = child.id().ok_or_else(|| NodeSupervisorError::Status {
                program: program.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "child process id is unavailable",
                ),
            })?;
            if let Err(error) = guard.write(pid) {
                kill_unready_child(&mut child).await;
                return Err(error);
            }
        }

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
            pid_guard,
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
        drop(self.pid_guard.take());
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
        drop(self.pid_guard.take());
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
            OsString::from("--dev-accounts"),
            OsString::from(config.dev_account_count.to_string()),
            OsString::from("--dev-account-seed"),
            OsString::from(config.dev_account_seed.as_str()),
            OsString::from("--dev-account-tokens"),
            OsString::from(config.dev_account_tokens.to_string()),
        ])
        .env(HOGD_LOG_FORMAT_ENV, "json")
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

fn validate_dev_account_count(expected: usize, actual: usize) -> Result<(), NodeSupervisorError> {
    if actual == expected {
        return Ok(());
    }
    Err(NodeSupervisorError::DevAccountCountMismatch { expected, actual })
}

fn validate_dev_account_index(position: usize, actual: u32) -> Result<(), NodeSupervisorError> {
    let expected = position;
    if usize::try_from(actual).ok() == Some(expected) {
        return Ok(());
    }
    Err(NodeSupervisorError::DevAccountIndexMismatch {
        position,
        expected,
        actual,
    })
}

fn validate_dev_account_public_key(
    index: usize,
    account: &HogdDevReadyAccount,
    signer: &LocalSigner,
) -> Result<(), NodeSupervisorError> {
    let expected = signer.verifying_key_bytes();
    let actual = account.public_key.0.as_slice();
    if actual.len() != expected.len() {
        return Err(NodeSupervisorError::DevAccountPublicKeyLength {
            index,
            actual: actual.len(),
        });
    }
    if actual == expected.as_slice() {
        return Ok(());
    }

    Err(NodeSupervisorError::DevAccountPublicKeyMismatch {
        index,
        expected: format_hex(&expected),
        actual: format_hex(actual),
    })
}

fn format_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
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
    let mut stdout = BufReader::new(stdout);
    let read_result = time::timeout(
        timeout,
        read_capped_line(&mut stdout, MAX_READINESS_LINE_BYTES),
    )
    .await;
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

async fn read_capped_line<R>(reader: &mut R, max_bytes: usize) -> Result<CappedLine, io::Error>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();

    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            if line.is_empty() {
                return Ok(CappedLine::Eof);
            }
            return Ok(CappedLine::Line(
                String::from_utf8_lossy(&line).into_owned(),
            ));
        }

        let newline = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1);
        let line_end = newline.unwrap_or(buffer.len());

        if line.len().saturating_add(line_end) > max_bytes {
            reader.consume(line_end);
            return Ok(CappedLine::TooLarge);
        }

        line.extend_from_slice(&buffer[..line_end]);
        reader.consume(line_end);
        if newline.is_some() {
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

        loop {
            match read_capped_line(&mut stderr, MAX_LOG_LINE_BYTES).await {
                Ok(CappedLine::Eof) => break,
                Ok(CappedLine::Line(line)) => {
                    forward_log_line(line.trim_end_matches(['\r', '\n']));
                }
                Ok(CappedLine::TooLarge) => {
                    tracing::warn!(
                        target: "hogd_dev",
                        max_bytes = MAX_LOG_LINE_BYTES,
                        "discarding oversized hogd-dev stderr log line"
                    );
                }
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
    timestamp_ms: Option<i64>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    fields: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ForwardedLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    Unknown(String),
    Missing,
}

impl ForwardedLogLevel {
    fn from_record(level: Option<&str>) -> Self {
        let Some(level) = level else {
            return Self::Missing;
        };
        if level.eq_ignore_ascii_case("error") {
            Self::Error
        } else if level.eq_ignore_ascii_case("warn") || level.eq_ignore_ascii_case("warning") {
            Self::Warn
        } else if level.eq_ignore_ascii_case("info") {
            Self::Info
        } else if level.eq_ignore_ascii_case("debug") {
            Self::Debug
        } else if level.eq_ignore_ascii_case("trace") {
            Self::Trace
        } else {
            Self::Unknown(level.to_owned())
        }
    }

    fn as_log_field(&self) -> Option<&str> {
        match self {
            Self::Unknown(level) => Some(level.as_str()),
            Self::Missing => Some("<missing>"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForwardedLogRecord {
    timestamp_ms: Option<i64>,
    level: ForwardedLogLevel,
    hogd_target: String,
    message: String,
    fields: ForwardedLogFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ForwardLogOutcome {
    Forwarded(ForwardedLogRecord),
    InvalidJson,
}

fn forward_log_line(line: &str) -> ForwardLogOutcome {
    match serde_json::from_str::<HogdDevLogLine>(line) {
        Ok(record) => {
            let forwarded = ForwardedLogRecord {
                timestamp_ms: record.timestamp_ms,
                level: ForwardedLogLevel::from_record(record.level.as_deref()),
                hogd_target: record.target.unwrap_or_else(|| "hogd_dev".to_owned()),
                message: record.message.unwrap_or_default(),
                fields: parse_log_fields(record.fields.as_ref()),
            };
            emit_forwarded_log(&forwarded);
            ForwardLogOutcome::Forwarded(forwarded)
        }
        Err(error) => {
            tracing::warn!(
                target: "hogd_dev",
                %error,
                line,
                "invalid hogd-dev stderr log line"
            );
            ForwardLogOutcome::InvalidJson
        }
    }
}

macro_rules! emit_hogd_log {
    ($level:ident, $record:expr, $hogd_file:expr, $hogd_line:expr, $hogd_fields:expr) => {
        tracing::$level!(
            target: "hogd_dev",
            hogd_timestamp_ms = $record.timestamp_ms,
            hogd_target = %$record.hogd_target,
            hogd_file = %$hogd_file,
            hogd_line = $hogd_line,
            hogd_fields = %$hogd_fields,
            "{}",
            $record.message
        );
    };
}

fn emit_forwarded_log(record: &ForwardedLogRecord) {
    let hogd_file = record.fields.file.as_deref().unwrap_or("");
    let hogd_line = record.fields.line.unwrap_or(0);
    let hogd_fields = record.fields.raw.as_str();

    match &record.level {
        ForwardedLogLevel::Error => {
            emit_hogd_log!(error, record, hogd_file, hogd_line, hogd_fields);
        }
        ForwardedLogLevel::Warn => {
            emit_hogd_log!(warn, record, hogd_file, hogd_line, hogd_fields);
        }
        ForwardedLogLevel::Info => {
            emit_hogd_log!(info, record, hogd_file, hogd_line, hogd_fields);
        }
        ForwardedLogLevel::Debug => {
            emit_hogd_log!(debug, record, hogd_file, hogd_line, hogd_fields);
        }
        ForwardedLogLevel::Trace => {
            emit_hogd_log!(trace, record, hogd_file, hogd_line, hogd_fields);
        }
        ForwardedLogLevel::Unknown(_) | ForwardedLogLevel::Missing => {
            let level = record.level.as_log_field().unwrap_or("<missing>");
            tracing::warn!(
                target: "hogd_dev",
                hogd_timestamp_ms = record.timestamp_ms,
                hogd_target = %record.hogd_target,
                %hogd_file,
                hogd_line,
                %hogd_fields,
                %level,
                "{}",
                record.message
            );
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ForwardedLogFields {
    raw: String,
    file: Option<String>,
    line: Option<u64>,
}

fn parse_log_fields(fields: Option<&serde_json::Value>) -> ForwardedLogFields {
    let Some(fields) = fields else {
        return ForwardedLogFields::default();
    };

    ForwardedLogFields {
        raw: fields.to_string(),
        file: fields
            .get("file")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        line: fields.get("line").and_then(serde_json::Value::as_u64),
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
        DEFAULT_HOGD_DEV_BINARY, HOGD_DEV_BINARY_ENV, HOGD_LOG_FORMAT_ENV, HogdDevProcess,
        build_dev_accounts, build_hogd_dev_spec, parse_readiness_line, resolve_hogd_dev_binary,
    };
    use crate::{
        HogdDevConfig, NodeSupervisorError,
        test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
    };

    const READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346","db_gateway":"127.0.0.1:12347","view_gateway":"127.0.0.1:12348","dev_control":"127.0.0.1:12349"}}"#;
    const V1_READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler_coord":"127.0.0.1:12345"}}"#;
    const COMPACT_READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","gateway":"127.0.0.1:12347","view":"127.0.0.1:12348"}}"#;

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
                OsString::from("--dev-accounts"),
                OsString::from("10"),
                OsString::from("--dev-account-seed"),
                OsString::from("groundhog-dev-accounts"),
                OsString::from("--dev-account-tokens"),
                OsString::from("1000000000"),
            ]
        );
        assert_eq!(
            spec.program_envs(),
            &[(OsString::from(HOGD_LOG_FORMAT_ENV), OsString::from("json"))]
        );
    }

    #[test]
    fn log_fields_extract_file_line_and_preserve_raw_fields() {
        let raw = serde_json::json!({
            "file": "rpc_from_coord.cc",
            "line": 1833,
            "stage": "commit",
        });

        let fields = super::parse_log_fields(Some(&raw));

        assert_eq!(fields.file.as_deref(), Some("rpc_from_coord.cc"));
        assert_eq!(fields.line, Some(1833));
        let round_trip: serde_json::Value =
            serde_json::from_str(&fields.raw).expect("bug: raw fields should stay JSON");
        assert_eq!(round_trip, raw);
    }

    #[test]
    fn log_fields_tolerate_absent_metadata() {
        let fields = super::parse_log_fields(None);

        assert_eq!(fields.file, None);
        assert_eq!(fields.line, None);
        assert!(fields.raw.is_empty());
    }

    #[test]
    fn forward_log_line_reports_malformed_json() {
        let outcome = super::forward_log_line("not json");

        assert_eq!(outcome, super::ForwardLogOutcome::InvalidJson);
    }

    #[test]
    fn forward_log_line_tolerates_missing_level_and_message() {
        let outcome = super::forward_log_line(r#"{"target":"indexer"}"#);

        let super::ForwardLogOutcome::Forwarded(record) = outcome else {
            panic!("expected forwarded log record");
        };
        assert_eq!(record.level, super::ForwardedLogLevel::Missing);
        assert_eq!(record.timestamp_ms, None);
        assert_eq!(record.hogd_target, "indexer");
        assert!(record.message.is_empty());
        assert_eq!(record.fields, super::ForwardedLogFields::default());
    }

    #[test]
    fn forward_log_line_tolerates_empty_fields() {
        let outcome = super::forward_log_line(
            r#"{"timestamp_ms":1770000000123,"level":"info","target":"hogdb","message":"ready","fields":{}}"#,
        );

        let super::ForwardLogOutcome::Forwarded(record) = outcome else {
            panic!("expected forwarded log record");
        };
        assert_eq!(record.timestamp_ms, Some(1_770_000_000_123));
        assert_eq!(record.level, super::ForwardedLogLevel::Info);
        assert_eq!(record.hogd_target, "hogdb");
        assert_eq!(record.message, "ready");
        assert_eq!(
            record.fields,
            super::ForwardedLogFields {
                raw: "{}".to_owned(),
                file: None,
                line: None,
            }
        );
    }

    #[test]
    fn readiness_line_parses_endpoint_addresses() {
        let readiness =
            parse_readiness_line("hogd-dev", READY).expect("bug: readiness payload should parse");

        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.network_id, "groundhog-local");
        assert_eq!(
            readiness.endpoints.scheduler_coord,
            "127.0.0.1:12345"
                .parse()
                .expect("bug: address should parse")
        );
        assert_eq!(
            readiness.endpoints.indexer_block_explorer,
            Some(
                "127.0.0.1:12346"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
        assert_eq!(
            readiness.endpoints.dev_control,
            Some(
                "127.0.0.1:12349"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
        assert_eq!(
            readiness.endpoints.db_gateway,
            Some(
                "127.0.0.1:12347"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
        assert_eq!(
            readiness.endpoints.view_gateway,
            Some(
                "127.0.0.1:12348"
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
            readiness.endpoints.scheduler_coord,
            "127.0.0.1:12345"
                .parse()
                .expect("bug: address should parse")
        );
        assert_eq!(readiness.endpoints.indexer_block_explorer, None);
        assert_eq!(readiness.endpoints.db_gateway, None);
        assert_eq!(readiness.endpoints.view_gateway, None);
        assert_eq!(readiness.endpoints.dev_control, None);
    }

    #[test]
    fn readiness_line_accepts_compact_endpoint_names() {
        let readiness = parse_readiness_line("hogd-dev", COMPACT_READY)
            .expect("bug: compact readiness payload should parse");

        assert_eq!(
            readiness.endpoints.scheduler_coord,
            "127.0.0.1:12345"
                .parse()
                .expect("bug: address should parse")
        );
        assert_eq!(
            readiness.endpoints.indexer_block_explorer,
            Some(
                "127.0.0.1:12346"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
        assert_eq!(
            readiness.endpoints.db_gateway,
            Some(
                "127.0.0.1:12347"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
        assert_eq!(
            readiness.endpoints.view_gateway,
            Some(
                "127.0.0.1:12348"
                    .parse()
                    .expect("bug: address should parse")
            )
        );
    }

    #[test]
    fn readiness_line_parses_dev_accounts() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let readiness = parse_readiness_line(
            "hogd-dev",
            &format!(
                r#"{{"status":"ready","network_id":"groundhog-local","endpoints":{{"scheduler_coord":"127.0.0.1:12345"}},"dev_accounts":[{{"index":0,"address":"0x{}","public_key":"0x{}","initial_storage_tokens":1000000000}}]}}"#,
                "11".repeat(32),
                hex::encode(signer.verifying_key_bytes()),
            ),
        )
        .expect("bug: readiness payload should parse");

        let config = HogdDevConfig::default().with_dev_account_count(1);
        let accounts = build_dev_accounts(&readiness, &config)
            .expect("bug: matching readiness dev account should validate");

        assert_eq!(readiness.dev_accounts.len(), 1);
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            serde_json::to_value(accounts[0].address).expect("bug: address serializes"),
            format!("0x{}", "11".repeat(32)),
        );
        assert_eq!(
            accounts[0].public_key.0,
            signer.verifying_key_bytes().to_vec()
        );
        assert_eq!(
            accounts[0].private_key.0,
            signer.to_keypair_bytes()[..32].to_vec()
        );
        assert_eq!(accounts[0].initial_storage_tokens, 1_000_000_000);
    }

    #[test]
    fn dev_account_count_mismatch_is_rejected() {
        let readiness =
            parse_readiness_line("hogd-dev", V1_READY).expect("bug: readiness payload parses");
        let err = build_dev_accounts(&readiness, &HogdDevConfig::default())
            .expect_err("bug: missing default dev accounts should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::DevAccountCountMismatch {
                expected: 10,
                actual: 0
            }
        ));
    }

    #[test]
    fn dev_account_public_key_mismatch_is_rejected() {
        let readiness = parse_readiness_line(
            "hogd-dev",
            &format!(
                r#"{{"status":"ready","network_id":"groundhog-local","endpoints":{{"scheduler_coord":"127.0.0.1:12345"}},"dev_accounts":[{{"index":0,"address":"0x{}","public_key":"0x{}","initial_storage_tokens":1000000000}}]}}"#,
                "11".repeat(32),
                "22".repeat(32),
            ),
        )
        .expect("bug: readiness payload should parse");
        let config = HogdDevConfig::default().with_dev_account_count(1);
        let err = build_dev_accounts(&readiness, &config)
            .expect_err("bug: wrong readiness public key should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::DevAccountPublicKeyMismatch { index: 0, .. }
        ));
    }

    #[test]
    fn non_ready_status_is_rejected() {
        let err = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"starting","network_id":"groundhog-local","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346","dev_control":"127.0.0.1:12347"}}"#,
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
    async fn start_with_pid_file_writes_and_cleans_up() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let pid_file = temp.path().join("nested").join("hogd-dev.pid");
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_millis(500))
            .with_pid_file(pid_file.clone());

        let mut process = start_fake_hogd_dev(&config, "ready")
            .await
            .expect("bug: fake hogd-dev should start");
        let child_id = process
            .child_id()
            .expect("bug: fake hogd-dev pid should be available");

        assert_eq!(
            fs::read_to_string(&pid_file).expect("bug: PID file should be readable"),
            format!("{child_id}\n")
        );

        let status = process.stop().await.expect("bug: fake child should stop");
        assert!(status.success());
        assert!(!pid_file.exists());
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
