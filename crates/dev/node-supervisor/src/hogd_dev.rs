//! hog-devnode process adapter.

use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use groundhog_json_rpc_types::{
    ContractAddress, NetworkHashHex,
    hex::HexBytes,
    runtime_manifest::{
        HOGD_DEV_RUNTIME_MANIFEST_KIND, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest,
        RuntimeManifestChainState, parse_runtime_manifest,
    },
};
use groundhog_signing::LocalSigner;
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
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

/// Default devnode binary name.
pub const DEFAULT_HOGD_DEV_BINARY: &str = "hog-devnode";

/// Legacy devnode binary name accepted during the rename transition.
pub const LEGACY_HOGD_DEV_BINARY: &str = "hogd-dev";

/// Environment variable used to override the devnode binary path.
pub const DEVNODE_BINARY_ENV: &str = "GROUNDHOG_DEVNODE_BINARY";

/// Environment variable used to request structured JSON logs from hogd-dev.
pub const HOGD_LOG_FORMAT_ENV: &str = "HOGD_LOG_FORMAT";

/// Maximum accepted hog-devnode readiness line length.
pub const MAX_READINESS_LINE_BYTES: usize = 64 * 1024;

/// Maximum accepted hogd-dev stderr log line length.
pub const MAX_LOG_LINE_BYTES: usize = 64 * 1024;

/// Maximum accepted runtime manifest size.
pub const MAX_RUNTIME_MANIFEST_BYTES: usize = 1024 * 1024;

/// Bootstrap-account derivation contract supported by the Rust dev tooling.
pub const SUPPORTED_BOOTSTRAP_ACCOUNT_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";

const BOOTSTRAP_MANIFEST_FILE_NAME: &str = "bootstrap-manifest.json";
const PERSISTENT_PID_FILE_NAME: &str = "hog-devnode.pid";

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

/// Public dev-account metadata advertised by hog-devnode readiness.
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

/// hog-devnode readiness payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct HogdDevReadiness {
    /// Readiness status. Must be `ready`.
    pub status: String,
    /// Whether hog-devnode initialized a new chain or recovered a committed chain.
    #[serde(default)]
    pub chain_state: Option<RuntimeManifestChainState>,
    /// Unprefixed 32-byte local chain network hash.
    pub network_hash: NetworkHashHex,
    /// x23 protocol token expected by hogd-dev services.
    #[serde(default)]
    pub protocol: Option<String>,
    /// x23 endpoints available after startup.
    pub endpoints: HogdDevEndpoints,
    /// Public dev-account metadata advertised by hogd-dev.
    #[serde(default)]
    pub(crate) dev_accounts: Vec<HogdDevReadyAccount>,
    /// Full runtime manifest loaded from the advertised readiness pointer.
    #[serde(default)]
    pub runtime_manifest: Option<RuntimeManifest>,
    /// Runtime manifest file path advertised by hog-devnode readiness.
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    /// SHA-256 hash of the exact runtime manifest file bytes.
    #[serde(default)]
    pub manifest_hash: Option<String>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HogdDevReadinessPointer {
    schema_version: u32,
    status: String,
    manifest_path: PathBuf,
    manifest_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HogdDevReadinessEnvelope {
    schema_version: u32,
}

/// Validate hog-devnode readiness account metadata against local dev-account config.
pub(crate) fn validate_dev_accounts(
    readiness: &HogdDevReadiness,
    config: &HogdDevConfig,
) -> Result<(), NodeSupervisorError> {
    let manifest = readiness.runtime_manifest.as_ref().ok_or_else(|| {
        NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_string(),
            reason: "hog-devnode readiness must advertise a runtime manifest".to_string(),
        }
    })?;
    validate_dev_account_count(config.dev_account_count, readiness.dev_accounts.len())?;
    if config.dev_account_count == 0 {
        return Ok(());
    }
    validate_bootstrap_derivation(manifest, config)?;

    for (position, account) in readiness.dev_accounts.iter().enumerate() {
        validate_dev_account_index(position, account.index)?;
        validate_dev_account_tokens(position, account.initial_storage_tokens, config)?;
        let signer =
            groundhog_signing::derive_dev_account_signer(&config.dev_account_seed, position);
        validate_dev_account_public_key(position, account, &signer)?;
    }

    Ok(())
}

pub(crate) fn validate_runtime_identity(
    readiness: &HogdDevReadiness,
    expected_protocol: &str,
) -> Result<(), NodeSupervisorError> {
    let expected_network_hash = local_dev_network_hash();
    if readiness.network_hash != expected_network_hash {
        return Err(NodeSupervisorError::RuntimeNetworkHashMismatch {
            expected: expected_network_hash,
            observed: readiness.network_hash.clone(),
        });
    }
    if readiness.protocol.as_deref() != Some(expected_protocol) {
        return Err(NodeSupervisorError::RuntimeProtocolMismatch {
            expected: expected_protocol.to_string(),
            observed: readiness.protocol.clone(),
        });
    }
    Ok(())
}

/// Running hogd-dev child process plus its parsed readiness metadata.
#[derive(Debug)]
pub struct HogdDevProcess {
    readiness: HogdDevReadiness,
    child: SupervisedChild,
    stderr_task: Option<JoinHandle<()>>,
    shutdown_grace: Duration,
    data_dir: Option<PathBuf>,
    pid_file: Option<PathBuf>,
    pid_guards: Vec<PidGuard>,
    _bootstrap_manifest: Option<BootstrapManifestFile>,
}

impl HogdDevProcess {
    /// Start hog-devnode using the configured binary lookup.
    #[tracing::instrument(skip_all, fields(listen_host = %config.listen_host))]
    pub async fn start(config: &HogdDevConfig) -> Result<Self, NodeSupervisorError> {
        let binary = resolve_hogd_dev_binary(config)?;
        Self::start_with_binary(config, binary).await
    }

    /// Start hog-devnode from an already-resolved binary path.
    #[tracing::instrument(skip_all, fields(binary = %binary.as_ref().display()))]
    pub async fn start_with_binary(
        config: &HogdDevConfig,
        binary: impl AsRef<Path>,
    ) -> Result<Self, NodeSupervisorError> {
        let bootstrap_manifest = write_bootstrap_manifest(config)?;
        let spec = build_hogd_dev_spec_with_bootstrap_manifest(
            binary.as_ref(),
            config,
            bootstrap_manifest.path(),
        );
        Self::start_with_spec(config, spec, Some(bootstrap_manifest)).await
    }

    async fn start_with_spec(
        config: &HogdDevConfig,
        spec: ChildProcessSpec,
        bootstrap_manifest: Option<BootstrapManifestFile>,
    ) -> Result<Self, NodeSupervisorError> {
        prepare_persistent_data_dir(config)?;
        let persistent_pid_file = config
            .data_dir
            .as_ref()
            .map(|data_dir| data_dir.join(PERSISTENT_PID_FILE_NAME));
        let mut pid_guards = Vec::new();
        if let Some(pid_file) = &persistent_pid_file {
            pid_guards.push(
                PidGuard::acquire_persistent(
                    pid_file.clone(),
                    spec.program(),
                    config.shutdown_grace,
                )
                .await?,
            );
        }
        if let Some(pid_file) = &config.pid_file
            && Some(pid_file) != persistent_pid_file.as_ref()
        {
            let guard = if config.data_dir.is_some() {
                PidGuard::acquire_persistent(
                    pid_file.clone(),
                    spec.program(),
                    config.shutdown_grace,
                )
                .await?
            } else {
                PidGuard::acquire(pid_file.clone(), spec.program(), config.shutdown_grace).await?
            };
            pid_guards.push(guard);
        }

        let mut child = ChildSupervisor::new(spec).spawn()?;
        let program = child.program().to_string();
        if !pid_guards.is_empty() {
            let pid = child.id().ok_or_else(|| NodeSupervisorError::Status {
                program: program.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "child process id is unavailable",
                ),
            })?;
            for guard in &mut pid_guards {
                if let Err(error) = guard.write(pid) {
                    kill_unready_child(&mut child).await;
                    return Err(error);
                }
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

        let readiness_result =
            read_readiness_line(&mut child, stdout, config.startup_timeout).await;
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
            data_dir: config.data_dir.clone(),
            pid_file: config.pid_file.clone(),
            pid_guards,
            _bootstrap_manifest: bootstrap_manifest,
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
        let mut status = Some(self.child.wait().await?);
        #[cfg(unix)]
        if self.child.process_group_exists()? {
            tracing::warn!(
                program = %self.child.program(),
                process_group = ?self.child.process_group_id(),
                persistent = self.data_dir.is_some(),
                data_dir = ?self.data_dir,
                pid_file = ?self.pid_file,
                "hog-devnode leader exited while descendants remained; stopping its process group"
            );
            self.child.terminate_process_group()?;
            self.drain_process_group(&mut status).await?;
        }
        self.wait_for_stderr_task().await;
        self.release_pid_guard();
        Ok(status.expect("bug: stopped child has an exit status"))
    }

    /// Gracefully stop hogd-dev, escalating after the configured timeout.
    #[tracing::instrument(skip_all, fields(shutdown_grace = ?self.shutdown_grace))]
    pub async fn stop(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        #[cfg(unix)]
        let status = self.stop_process_group().await?;

        #[cfg(not(unix))]
        let status = match self.child.try_wait()? {
            Some(status) => status,
            None => self.child.kill_and_wait().await?,
        };

        self.wait_for_stderr_task().await;
        self.release_pid_guard();
        Ok(status)
    }

    #[cfg(unix)]
    async fn stop_process_group(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        let mut status = self.child.try_wait()?;
        if self.child.process_group_exists()? {
            self.child.terminate_process_group()?;
            self.drain_process_group(&mut status).await?;
        }

        match status {
            Some(status) => Ok(status),
            None => time::timeout(Duration::from_millis(500), self.child.wait())
                .await
                .map_err(|_| self.shutdown_incomplete_error())?,
        }
    }

    #[cfg(unix)]
    async fn drain_process_group(
        &mut self,
        status: &mut Option<ExitStatus>,
    ) -> Result<(), NodeSupervisorError> {
        const POST_KILL_DRAIN: Duration = Duration::from_millis(500);

        let deadline = time::Instant::now() + self.shutdown_grace;
        let mut kill_deadline = None;

        loop {
            if status.is_none() {
                *status = self.child.try_wait()?;
            }

            if !self.child.process_group_exists()? {
                return Ok(());
            }

            let now = time::Instant::now();
            if kill_deadline.is_none() && now >= deadline {
                tracing::warn!(
                    program = %self.child.program(),
                    pid = ?self.child.id(),
                    process_group = ?self.child.process_group_id(),
                    shutdown_grace = ?self.shutdown_grace,
                    persistent = self.data_dir.is_some(),
                    data_dir = ?self.data_dir,
                    pid_file = ?self.pid_file,
                    "hog-devnode did not stop within its graceful shutdown deadline; sending SIGKILL; persistent state may require recovery"
                );
                self.child.kill_process_group()?;
                kill_deadline = Some(now + POST_KILL_DRAIN);
            }

            if let Some(kill_deadline) = kill_deadline
                && now >= kill_deadline
            {
                tracing::warn!(
                    program = %self.child.program(),
                    pid = ?self.child.id(),
                    process_group = ?self.child.process_group_id(),
                    persistent = self.data_dir.is_some(),
                    data_dir = ?self.data_dir,
                    pid_file = ?self.pid_file,
                    "hog-devnode process group is still present after the SIGKILL drain"
                );
                return Err(self.shutdown_incomplete_error());
            }

            let next_deadline = kill_deadline.unwrap_or(deadline);
            time::sleep(
                next_deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(10)),
            )
            .await;
        }
    }

    #[cfg(unix)]
    fn shutdown_incomplete_error(&self) -> NodeSupervisorError {
        NodeSupervisorError::ShutdownIncomplete {
            program: self.child.program().to_string(),
            process_group: self.child.process_group_id(),
        }
    }

    async fn wait_for_stderr_task(&mut self) {
        const STDERR_DRAIN: Duration = Duration::from_millis(500);

        let Some(mut task) = self.stderr_task.take() else {
            return;
        };
        if time::timeout(STDERR_DRAIN, &mut task).await.is_err() {
            tracing::warn!(
                program = %self.child.program(),
                timeout = ?STDERR_DRAIN,
                "hog-devnode stderr remained open after process-group shutdown; aborting log forwarding"
            );
            task.abort();
            let _ = task.await;
        }
    }

    fn release_pid_guard(&mut self) {
        for guard in self.pid_guards.drain(..) {
            guard.release();
        }
    }
}

#[cfg(unix)]
fn prepare_persistent_data_dir(config: &HogdDevConfig) -> Result<(), NodeSupervisorError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let Some(data_dir) = config.data_dir.as_deref() else {
        return Ok(());
    };
    match fs::symlink_metadata(data_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(NodeSupervisorError::DataDir {
                path: data_dir.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "persistent data path must be a directory and must not be a symlink",
                ),
            })
        }
        Ok(_) => {
            fs::set_permissions(data_dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
                NodeSupervisorError::DataDir {
                    path: data_dir.to_path_buf(),
                    source,
                }
            })
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(data_dir)
                .map_err(|source| NodeSupervisorError::DataDir {
                    path: data_dir.to_path_buf(),
                    source,
                })
        }
        Err(source) => Err(NodeSupervisorError::DataDir {
            path: data_dir.to_path_buf(),
            source,
        }),
    }
}

#[cfg(not(unix))]
fn prepare_persistent_data_dir(config: &HogdDevConfig) -> Result<(), NodeSupervisorError> {
    if config.data_dir.is_some() {
        return Err(NodeSupervisorError::PersistentStateUnsupported);
    }
    Ok(())
}

#[derive(Debug)]
struct BootstrapManifestFile {
    _dir: TempDir,
    path: PathBuf,
}

impl BootstrapManifestFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn write_bootstrap_manifest(
    config: &HogdDevConfig,
) -> Result<BootstrapManifestFile, NodeSupervisorError> {
    let manifest = config
        .to_bootstrap_manifest(local_dev_network_hash())
        .map_err(
            |source| NodeSupervisorError::InvalidBootstrapManifestConfig {
                reason: format!("dev_account_count does not fit u32: {source}"),
            },
        )?;
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|source| NodeSupervisorError::BootstrapManifestSerialize { source })?;
    let dir = tempfile::Builder::new()
        .prefix("groundhog-hogd-bootstrap-")
        .tempdir()
        .map_err(|source| NodeSupervisorError::BootstrapManifestTempDir { source })?;
    let path = dir.path().join(BOOTSTRAP_MANIFEST_FILE_NAME);
    fs::write(&path, bytes).map_err(|source| NodeSupervisorError::BootstrapManifestWrite {
        path: path.clone(),
        source,
    })?;
    Ok(BootstrapManifestFile { _dir: dir, path })
}

fn local_dev_network_hash() -> NetworkHashHex {
    NetworkHashHex::from([0; 32])
}

/// Build the hogd-dev child process command specification.
///
/// This compatibility helper preserves the original shorthand-flag command
/// shape for external callers. The supervisor startup path uses
/// `--bootstrap-manifest` via `build_hogd_dev_spec_with_bootstrap_manifest`.
pub fn build_hogd_dev_spec(binary: impl AsRef<Path>, config: &HogdDevConfig) -> ChildProcessSpec {
    let mut spec = ChildProcessSpec::new(binary.as_ref().as_os_str().to_owned())
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
        .kill_on_drop(config.data_dir.is_none())
        .stdout_piped()
        .stderr_piped();

    if let Some(data_dir) = &config.data_dir {
        spec = spec.args([
            OsString::from("--data-dir"),
            data_dir.as_os_str().to_owned(),
        ]);
    }

    spec
}

/// Build the hogd-dev command specification used by the supervisor path.
pub(crate) fn build_hogd_dev_spec_with_bootstrap_manifest(
    binary: impl AsRef<Path>,
    config: &HogdDevConfig,
    bootstrap_manifest_path: impl AsRef<Path>,
) -> ChildProcessSpec {
    let mut spec = ChildProcessSpec::new(binary.as_ref().as_os_str().to_owned())
        .args([
            OsString::from("--listen-host"),
            OsString::from(config.listen_host.as_str()),
            OsString::from("--scheduler-port"),
            OsString::from("0"),
            OsString::from("--readiness"),
            OsString::from("stdout"),
            OsString::from("--bootstrap-manifest"),
            bootstrap_manifest_path.as_ref().as_os_str().to_owned(),
        ])
        .env(HOGD_LOG_FORMAT_ENV, "json")
        .kill_on_drop(config.data_dir.is_none())
        .stdout_piped()
        .stderr_piped();

    if let Some(log_level) = config.log_level {
        spec = spec.args([
            OsString::from("--log-level"),
            OsString::from(log_level.as_str()),
        ]);
    }

    if let Some(data_dir) = &config.data_dir {
        spec = spec.args([
            OsString::from("--data-dir"),
            data_dir.as_os_str().to_owned(),
        ]);
    }

    #[cfg(unix)]
    let spec = spec.process_group(0);

    spec
}

/// Resolve the devnode binary path using the v1 lookup order.
pub fn resolve_hogd_dev_binary(config: &HogdDevConfig) -> Result<PathBuf, NodeSupervisorError> {
    if let Some(binary_path) = &config.binary_path {
        return validate_configured_binary(binary_path);
    }

    if let Some(env_path) = env::var_os(DEVNODE_BINARY_ENV) {
        return validate_configured_binary(Path::new(&env_path));
    }

    let current_exe =
        env::current_exe().map_err(|source| NodeSupervisorError::CurrentExe { source })?;
    if let Some(parent) = current_exe.parent() {
        for binary in [DEFAULT_HOGD_DEV_BINARY, LEGACY_HOGD_DEV_BINARY] {
            let sibling = parent.join(binary);
            if is_executable_file(&sibling) {
                return Ok(sibling);
            }
        }
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            for binary in [DEFAULT_HOGD_DEV_BINARY, LEGACY_HOGD_DEV_BINARY] {
                let candidate = dir.join(binary);
                if is_executable_file(&candidate) {
                    return Ok(candidate);
                }
            }
        }
    }

    Err(NodeSupervisorError::HogdDevNotFound)
}

/// Parse one hog-devnode readiness line.
pub fn parse_readiness_line(
    program: &str,
    line: &str,
) -> Result<HogdDevReadiness, NodeSupervisorError> {
    let value: serde_json::Value = parse_readiness_str(program, line)?;

    if has_manifest_pointer_fields(&value) {
        let envelope: HogdDevReadinessEnvelope =
            parse_readiness_value(program, value.clone(), line)?;
        if envelope.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
            return Err(NodeSupervisorError::InvalidReadinessSchemaVersion {
                program: program.to_string(),
                expected: RUNTIME_MANIFEST_SCHEMA_VERSION,
                actual: envelope.schema_version,
            });
        }
        let pointer: HogdDevReadinessPointer = parse_readiness_value(program, value, line)?;
        return parse_manifest_readiness_pointer(program, pointer);
    }

    let readiness: HogdDevReadiness = parse_readiness_value(program, value, line)?;
    readiness.ensure_ready(program)
}

fn parse_readiness_str<T>(program: &str, line: &str) -> Result<T, NodeSupervisorError>
where
    T: DeserializeOwned,
{
    serde_json::from_str(line).map_err(|source| NodeSupervisorError::InvalidReadinessJson {
        program: program.to_string(),
        line: line.to_string(),
        source,
    })
}

fn parse_readiness_value<T>(
    program: &str,
    value: serde_json::Value,
    original_line: &str,
) -> Result<T, NodeSupervisorError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(value).map_err(|source| NodeSupervisorError::InvalidReadinessJson {
        program: program.to_string(),
        line: original_line.to_string(),
        source,
    })
}

fn has_manifest_pointer_fields(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("schemaVersion")
        || object.contains_key("manifestPath")
        || object.contains_key("manifestHash")
}

fn parse_manifest_readiness_pointer(
    program: &str,
    pointer: HogdDevReadinessPointer,
) -> Result<HogdDevReadiness, NodeSupervisorError> {
    debug_assert_eq!(pointer.schema_version, RUNTIME_MANIFEST_SCHEMA_VERSION);
    if pointer.status != "ready" {
        return Err(NodeSupervisorError::InvalidReadinessStatus {
            program: program.to_string(),
            status: pointer.status,
        });
    }

    validate_manifest_hash_format(program, &pointer.manifest_hash)?;
    let manifest_path = pointer.manifest_path;
    let advertised_manifest_hash = pointer.manifest_hash;
    let manifest_hash = advertised_manifest_hash.to_ascii_lowercase();
    let manifest_bytes = read_runtime_manifest_bytes(program, &manifest_path)?;
    let actual_hash = sha256_hex(&manifest_bytes);
    if actual_hash != manifest_hash {
        return Err(NodeSupervisorError::RuntimeManifestHashMismatch {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            expected: advertised_manifest_hash,
            actual: actual_hash,
        });
    }

    let manifest = parse_runtime_manifest(&manifest_bytes).map_err(|source| {
        NodeSupervisorError::InvalidRuntimeManifest {
            program: program.to_string(),
            reason: source.to_string(),
        }
    })?;
    readiness_from_runtime_manifest(program, manifest, manifest_path, actual_hash)
}

fn read_runtime_manifest_bytes(
    program: &str,
    manifest_path: &Path,
) -> Result<Vec<u8>, NodeSupervisorError> {
    if !manifest_path.is_absolute() {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            reason: "manifestPath must be absolute".to_string(),
        });
    }

    validate_manifest_parent_dir(program, manifest_path)?;
    let file = open_runtime_manifest_file(program, manifest_path)?;
    read_capped_runtime_manifest(program, manifest_path, file)
}

fn read_capped_runtime_manifest(
    program: &str,
    manifest_path: &Path,
    file: File,
) -> Result<Vec<u8>, NodeSupervisorError> {
    let mut bytes = Vec::new();
    let mut reader = file.take((MAX_RUNTIME_MANIFEST_BYTES + 1) as u64);
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| NodeSupervisorError::RuntimeManifestRead {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            source,
        })?;
    if bytes.len() > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(NodeSupervisorError::InvalidRuntimeManifest {
            program: program.to_string(),
            reason: format!("runtime manifest exceeds {MAX_RUNTIME_MANIFEST_BYTES} bytes"),
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn validate_manifest_parent_dir(
    program: &str,
    manifest_path: &Path,
) -> Result<(), NodeSupervisorError> {
    use std::os::unix::fs::MetadataExt;

    let parent = manifest_path.parent().ok_or_else(|| {
        NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            reason: "manifestPath has no parent directory".to_string(),
        }
    })?;
    let metadata = std::fs::symlink_metadata(parent).map_err(|source| {
        NodeSupervisorError::RuntimeManifestRead {
            program: program.to_string(),
            path: parent.display().to_string(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: parent.display().to_string(),
            reason: "manifest parent must be a directory, not a symlink".to_string(),
        });
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: parent.display().to_string(),
            reason: format!(
                "manifest parent uid {} does not match current uid {expected_uid}",
                metadata.uid()
            ),
        });
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: parent.display().to_string(),
            reason: format!("manifest parent mode {mode:03o} permits group or other access"),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_manifest_parent_dir(
    _program: &str,
    _manifest_path: &Path,
) -> Result<(), NodeSupervisorError> {
    Ok(())
}

#[cfg(unix)]
fn open_runtime_manifest_file(
    program: &str,
    manifest_path: &Path,
) -> Result<File, NodeSupervisorError> {
    use std::{
        fs::OpenOptions,
        mem::MaybeUninit,
        os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    };

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(manifest_path)
        .map_err(|source| {
            if source.raw_os_error() == Some(libc::ELOOP) {
                return NodeSupervisorError::RuntimeManifestUnsafePermissions {
                    program: program.to_string(),
                    path: manifest_path.display().to_string(),
                    reason: "manifestPath must not be a symlink".to_string(),
                };
            }
            NodeSupervisorError::RuntimeManifestRead {
                program: program.to_string(),
                path: manifest_path.display().to_string(),
                source,
            }
        })?;

    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(NodeSupervisorError::RuntimeManifestRead {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            source: io::Error::last_os_error(),
        });
    }
    let stat = unsafe { stat.assume_init() };
    let mode = stat.st_mode & 0o777;
    let expected_uid = unsafe { libc::geteuid() };
    if stat.st_uid != expected_uid {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            reason: format!(
                "manifest uid {} does not match current uid {expected_uid}",
                stat.st_uid
            ),
        });
    }
    if mode & 0o077 != 0 {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            reason: format!("manifest mode {mode:03o} permits group or other access"),
        });
    }

    let metadata = file
        .metadata()
        .map_err(|source| NodeSupervisorError::RuntimeManifestRead {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(NodeSupervisorError::RuntimeManifestUnsafePermissions {
            program: program.to_string(),
            path: manifest_path.display().to_string(),
            reason: "manifest must be a regular file, not a symlink".to_string(),
        });
    }

    Ok(file)
}

#[cfg(not(unix))]
fn open_runtime_manifest_file(
    program: &str,
    manifest_path: &Path,
) -> Result<File, NodeSupervisorError> {
    File::open(manifest_path).map_err(|source| NodeSupervisorError::RuntimeManifestRead {
        program: program.to_string(),
        path: manifest_path.display().to_string(),
        source,
    })
}

fn readiness_from_runtime_manifest(
    program: &str,
    manifest: RuntimeManifest,
    manifest_path: PathBuf,
    manifest_hash: String,
) -> Result<HogdDevReadiness, NodeSupervisorError> {
    if manifest.kind != HOGD_DEV_RUNTIME_MANIFEST_KIND {
        return Err(NodeSupervisorError::InvalidRuntimeManifest {
            program: program.to_string(),
            reason: format!(
                "unexpected runtime manifest kind `{}`; expected `{HOGD_DEV_RUNTIME_MANIFEST_KIND}`",
                manifest.kind
            ),
        });
    }
    let chain_state =
        manifest
            .chain_state
            .ok_or_else(|| NodeSupervisorError::InvalidRuntimeManifest {
                program: program.to_string(),
                reason: "hog-devnode runtime manifest is missing chainState".to_string(),
            })?;
    let dev_accounts = manifest
        .bootstrap_accounts
        .iter()
        .map(|account| {
            let initial_storage_tokens =
                account
                    .initial_storage_tokens
                    .parse::<u64>()
                    .map_err(|source| NodeSupervisorError::InvalidRuntimeManifest {
                        program: program.to_string(),
                        reason: format!(
                            "bootstrap account {} initialStorageTokens is not a u64: {source}",
                            account.index
                        ),
                    })?;
            Ok(HogdDevReadyAccount {
                index: account.index,
                address: account.wallet_address,
                public_key: HexBytes(account.public_key.as_ref().to_vec()),
                initial_storage_tokens,
            })
        })
        .collect::<Result<Vec<_>, NodeSupervisorError>>()?;

    Ok(HogdDevReadiness {
        status: "ready".to_string(),
        chain_state: Some(chain_state),
        network_hash: manifest.network_hash.clone(),
        protocol: Some(manifest.protocol.clone()),
        endpoints: HogdDevEndpoints {
            scheduler_coord: manifest.endpoints.scheduler,
            indexer_block_explorer: manifest.endpoints.indexer,
            db_gateway: manifest.endpoints.db_gateway,
            view_gateway: manifest.endpoints.view_gateway,
            dev_control: None,
        },
        dev_accounts,
        runtime_manifest: Some(manifest),
        manifest_path: Some(manifest_path),
        manifest_hash: Some(manifest_hash),
    })
}

fn validate_manifest_hash_format(program: &str, hash: &str) -> Result<(), NodeSupervisorError> {
    let Some(hex) = hash.strip_prefix("0x") else {
        return Err(NodeSupervisorError::InvalidRuntimeManifestHash {
            program: program.to_string(),
            hash: hash.to_string(),
        });
    };
    if hex.len() == 64 && hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Ok(());
    }
    Err(NodeSupervisorError::InvalidRuntimeManifestHash {
        program: program.to_string(),
        hash: hash.to_string(),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(Sha256::digest(bytes)))
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

fn validate_bootstrap_derivation(
    manifest: &RuntimeManifest,
    config: &HogdDevConfig,
) -> Result<(), NodeSupervisorError> {
    let derivation = &manifest.bootstrap_account_derivation;
    if derivation.scheme != SUPPORTED_BOOTSTRAP_ACCOUNT_DERIVATION_SCHEME {
        return Err(NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_string(),
            reason: format!(
                "unsupported bootstrap account derivation scheme `{}`",
                derivation.scheme
            ),
        });
    }
    if derivation.seed != config.dev_account_seed {
        return Err(NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_string(),
            reason: "bootstrap account derivation seed does not match supervisor config"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_dev_account_tokens(
    index: usize,
    actual: u64,
    config: &HogdDevConfig,
) -> Result<(), NodeSupervisorError> {
    if actual == config.dev_account_tokens {
        return Ok(());
    }
    Err(NodeSupervisorError::InvalidRuntimeManifest {
        program: DEFAULT_HOGD_DEV_BINARY.to_string(),
        reason: format!(
            "bootstrap account dev-{index} initialStorageTokens {actual} does not match requested {}",
            config.dev_account_tokens
        ),
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
                    Err(error)
                }
            }
        }
        Ok(Ok(CappedLine::TooLarge)) => {
            kill_unready_child(child).await;
            Err(NodeSupervisorError::ReadinessTooLarge {
                program,
                max_bytes: MAX_READINESS_LINE_BYTES,
            })
        }
        Ok(Err(source)) => {
            kill_unready_child(child).await;
            Err(NodeSupervisorError::ReadinessRead { program, source })
        }
        Err(_) => {
            kill_unready_child(child).await;
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
    read_capped_line_inner(reader, max_bytes, false).await
}

async fn read_capped_line_discarding_tail<R>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<CappedLine, io::Error>
where
    R: AsyncBufRead + Unpin,
{
    read_capped_line_inner(reader, max_bytes, true).await
}

async fn read_capped_line_inner<R>(
    reader: &mut R,
    max_bytes: usize,
    discard_oversized_tail: bool,
) -> Result<CappedLine, io::Error>
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
            if discard_oversized_tail && newline.is_none() {
                discard_until_newline(reader).await?;
            }
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

async fn discard_until_newline<R>(reader: &mut R) -> Result<(), io::Error>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(());
        }

        let newline = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1);
        let consume = newline.unwrap_or(buffer.len());
        reader.consume(consume);
        if newline.is_some() {
            return Ok(());
        }
    }
}

const UNREADY_SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
const UNREADY_KILL_DRAIN: Duration = Duration::from_millis(500);

async fn kill_unready_child(child: &mut SupervisedChild) {
    #[cfg(unix)]
    kill_unready_process_group(child).await;

    #[cfg(not(unix))]
    match time::timeout(UNREADY_KILL_DRAIN, child.kill_and_wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(
            target: "hogd_dev",
            %error,
            program = child.program(),
            "failed to kill unready hogd-dev child"
        ),
        Err(_) => tracing::warn!(
            target: "hogd_dev",
            program = child.program(),
            timeout = ?UNREADY_KILL_DRAIN,
            "timed out waiting for unready hog-devnode child after forced termination"
        ),
    }
}

#[cfg(unix)]
async fn kill_unready_process_group(child: &mut SupervisedChild) {
    if let Err(error) = child.terminate_process_group() {
        tracing::warn!(
            target: "hogd_dev",
            %error,
            program = child.program(),
            "failed to SIGTERM unready hog-devnode process group"
        );
    }
    let term_deadline = time::Instant::now() + UNREADY_SHUTDOWN_GRACE;
    loop {
        if let Err(error) = child.try_wait() {
            tracing::warn!(
                target: "hogd_dev",
                %error,
                program = child.program(),
                "failed checking unready hog-devnode child status"
            );
            break;
        }
        match child.process_group_exists() {
            Ok(false) => {
                let _ = time::timeout(UNREADY_KILL_DRAIN, child.wait()).await;
                return;
            }
            Ok(true) if time::Instant::now() < term_deadline => {
                time::sleep(Duration::from_millis(10)).await;
            }
            Ok(true) => break,
            Err(error) => {
                tracing::warn!(
                    target: "hogd_dev",
                    %error,
                    program = child.program(),
                    "failed checking unready hog-devnode process group"
                );
                break;
            }
        }
    }
    tracing::warn!(
        target: "hogd_dev",
        program = child.program(),
        process_group = ?child.process_group_id(),
        shutdown_grace = ?UNREADY_SHUTDOWN_GRACE,
        "unready hog-devnode did not stop after SIGTERM; sending SIGKILL"
    );
    if let Err(error) = child.kill_process_group() {
        tracing::warn!(
            target: "hogd_dev",
            %error,
            program = child.program(),
            "failed to SIGKILL unready hog-devnode process group"
        );
    }
    match time::timeout(UNREADY_KILL_DRAIN, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(
                target: "hogd_dev",
                %error,
                program = child.program(),
                "failed waiting for unready hogd-dev cleanup"
        ),
        Err(_) => tracing::warn!(
                target: "hogd_dev",
                program = child.program(),
                timeout = ?UNREADY_KILL_DRAIN,
                "timed out waiting for unready hog-devnode child after SIGKILL"
        ),
    }
    let deadline = time::Instant::now() + UNREADY_KILL_DRAIN;
    loop {
        match child.process_group_exists() {
            Ok(false) => break,
            Ok(true) if time::Instant::now() < deadline => {
                time::sleep(Duration::from_millis(10)).await;
            }
            Ok(true) => {
                tracing::warn!(
                    target: "hogd_dev",
                    program = child.program(),
                    process_group = ?child.process_group_id(),
                    "unready hog-devnode process group remained after SIGKILL drain"
                );
                break;
            }
            Err(error) => {
                tracing::warn!(
                    target: "hogd_dev",
                    %error,
                    program = child.program(),
                    "failed checking unready hog-devnode process group"
                );
                break;
            }
        }
    }
}

fn spawn_stderr_forwarder(stderr: ChildStderr) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr);

        loop {
            match read_capped_line_discarding_tail(&mut stderr, MAX_LOG_LINE_BYTES).await {
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
    const STDERR_DRAIN: Duration = Duration::from_millis(500);

    let Some(mut task) = task else {
        return;
    };
    if time::timeout(STDERR_DRAIN, &mut task).await.is_err() {
        tracing::warn!(
            timeout = ?STDERR_DRAIN,
            "hog-devnode stderr remained open after failed startup; aborting log forwarding"
        );
        task.abort();
        let _ = task.await;
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
        path::{Path, PathBuf},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use serial_test::serial;
    use tempfile::TempDir;

    use super::{
        DEFAULT_HOGD_DEV_BINARY, DEVNODE_BINARY_ENV, HOGD_LOG_FORMAT_ENV, HogdDevProcess,
        PERSISTENT_PID_FILE_NAME, RuntimeManifestChainState, build_hogd_dev_spec,
        build_hogd_dev_spec_with_bootstrap_manifest, parse_readiness_line,
        prepare_persistent_data_dir, resolve_hogd_dev_binary, validate_dev_accounts,
        validate_runtime_identity, write_bootstrap_manifest,
    };
    use crate::{
        HogdDevConfig, NodeSupervisorError,
        test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
    };

    const V1_READY: &str = r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345"}}"#;
    const COMPACT_READY: &str = r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","gateway":"127.0.0.1:12347","view":"127.0.0.1:12348"}}"#;
    const GRACEFUL_SHUTDOWN_FILE_ENV: &str = "GROUNDHOG_FAKE_GRACEFUL_SHUTDOWN_FILE";

    #[test]
    fn command_shape_matches_contract() {
        let config = HogdDevConfig::default()
            .with_listen_host("127.0.0.2")
            .with_data_dir("/tmp/hog-devnode-data");
        let spec = build_hogd_dev_spec_with_bootstrap_manifest(
            "/tmp/hog-devnode",
            &config,
            "/tmp/bootstrap-manifest.json",
        );

        assert_eq!(spec.program(), std::ffi::OsStr::new("/tmp/hog-devnode"));
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
                OsString::from("--bootstrap-manifest"),
                OsString::from("/tmp/bootstrap-manifest.json"),
                OsString::from("--data-dir"),
                OsString::from("/tmp/hog-devnode-data"),
            ]
        );
        assert_eq!(
            spec.program_envs(),
            &[(OsString::from(HOGD_LOG_FORMAT_ENV), OsString::from("json"))]
        );
    }

    #[test]
    fn public_command_builder_keeps_shorthand_compatibility() {
        let config = HogdDevConfig::default()
            .with_listen_host("127.0.0.2")
            .with_data_dir("/tmp/hog-devnode-data");
        let spec = build_hogd_dev_spec("/tmp/hog-devnode", &config);

        assert_eq!(
            spec.program_args(),
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
                OsString::from("--data-dir"),
                OsString::from("/tmp/hog-devnode-data"),
            ]
        );
    }

    #[test]
    fn bootstrap_manifest_file_serializes_config() {
        let config = HogdDevConfig::default()
            .with_dev_account_count(2)
            .with_dev_account_seed("manifest-seed")
            .with_dev_account_tokens(1234);

        let manifest = write_bootstrap_manifest(&config)
            .expect("bug: bootstrap manifest file should be written");
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(manifest.path()).expect("bug: bootstrap manifest should be readable"),
        )
        .expect("bug: bootstrap manifest should parse");

        assert_eq!(value["kind"], "groundhog-bootstrap");
        assert_eq!(
            value["networkHash"],
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(value["profile"], "local-dev");
        assert_eq!(value["devWallet"]["profile"], "hog-devnode-default");
        assert_eq!(value["bootstrapAccounts"]["count"], 2);
        assert_eq!(value["bootstrapAccounts"]["seed"], "manifest-seed");
        assert_eq!(value["bootstrapAccounts"]["initialStorageTokens"], "1234");
        assert_eq!(value["storageCostModel"]["profile"], "hogd-default");
    }

    #[test]
    fn runtime_identity_rejects_non_bootstrap_network() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness should parse");
        readiness.network_hash = groundhog_json_rpc_types::NetworkHashHex::from([0xff; 32]);

        let err = validate_runtime_identity(&readiness, "hogp/test")
            .expect_err("bug: a non-bootstrap network should fail closed");
        assert!(matches!(
            err,
            NodeSupervisorError::RuntimeNetworkHashMismatch { expected, observed }
                if expected == groundhog_json_rpc_types::NetworkHashHex::from([0; 32])
                    && observed == groundhog_json_rpc_types::NetworkHashHex::from([0xff; 32])
        ));
    }

    #[test]
    fn runtime_identity_rejects_protocol_mismatch() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness should parse");

        let err = validate_runtime_identity(&readiness, "hogp/expected")
            .expect_err("bug: a protocol mismatch should fail closed");
        assert!(matches!(
            err,
            NodeSupervisorError::RuntimeProtocolMismatch { expected, observed }
                if expected == "hogp/expected" && observed.as_deref() == Some("hogp/test")
        ));
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

    #[tokio::test]
    async fn read_capped_line_discards_oversized_line_tail() {
        let input = b"abcdef\n{\"level\":\"info\",\"message\":\"next\"}\n";
        let mut reader = tokio::io::BufReader::with_capacity(4, &input[..]);

        assert!(matches!(
            super::read_capped_line_discarding_tail(&mut reader, 3).await,
            Ok(super::CappedLine::TooLarge)
        ));

        let next = super::read_capped_line(&mut reader, 64)
            .await
            .expect("bug: next line should read");
        let super::CappedLine::Line(line) = next else {
            panic!("expected next physical line after oversized line");
        };
        assert_eq!(line, "{\"level\":\"info\",\"message\":\"next\"}\n");
    }

    #[test]
    fn readiness_line_parses_endpoint_addresses() {
        let line = manifest_readiness_line(
            r#"{
              "scheduler":"127.0.0.1:12345",
              "indexer":"127.0.0.1:12346",
              "dbGateway":"127.0.0.1:12347",
              "viewGateway":"127.0.0.1:12348"
            }"#,
            r"[]",
        );
        let pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let pointer_keys = pointer
            .as_object()
            .expect("bug: pointer should be an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            sorted(pointer_keys),
            sorted(["manifestHash", "manifestPath", "schemaVersion", "status"])
        );

        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");

        assert_eq!(readiness.status, "ready");
        assert_eq!(
            readiness.chain_state,
            Some(RuntimeManifestChainState::Fresh)
        );
        assert_eq!(readiness.network_hash.as_str(), "00".repeat(32));
        assert_eq!(readiness.protocol.as_deref(), Some("hogp/test"));
        assert_eq!(
            readiness.manifest_hash,
            pointer["manifestHash"].as_str().map(str::to_owned)
        );
        assert!(readiness.manifest_path.is_some());
        assert!(readiness.runtime_manifest.is_some());
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
        assert_eq!(readiness.endpoints.dev_control, None);
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
    fn readiness_line_carries_recovered_chain_state() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let path = PathBuf::from(
            pointer["manifestPath"]
                .as_str()
                .expect("bug: pointer has manifest path"),
        );
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
                .expect("bug: manifest should parse");
        manifest["chainState"] = serde_json::json!("recovered");
        let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
        fs::write(&path, &bytes).expect("bug: manifest should be writable");
        pointer["manifestHash"] = serde_json::json!(super::sha256_hex(&bytes));

        let readiness = parse_readiness_line("hogd-dev", &pointer.to_string())
            .expect("bug: recovered readiness should parse");

        assert_eq!(
            readiness.chain_state,
            Some(RuntimeManifestChainState::Recovered)
        );
    }

    #[test]
    fn manifest_readiness_requires_chain_state() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let path = PathBuf::from(
            pointer["manifestPath"]
                .as_str()
                .expect("bug: pointer has manifest path"),
        );
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
                .expect("bug: manifest should parse");
        manifest
            .as_object_mut()
            .expect("bug: manifest is an object")
            .remove("chainState");
        let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
        fs::write(&path, &bytes).expect("bug: manifest should be writable");
        pointer["manifestHash"] = serde_json::json!(super::sha256_hex(&bytes));

        let error = parse_readiness_line("hogd-dev", &pointer.to_string())
            .expect_err("manifest readiness without chainState should fail");

        assert!(matches!(
            error,
            NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
                if reason.contains("chainState")
        ));
    }

    #[test]
    fn manifest_readiness_requires_hogd_dev_runtime_kind() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let path = PathBuf::from(
            pointer["manifestPath"]
                .as_str()
                .expect("bug: pointer has manifest path"),
        );
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
                .expect("bug: manifest should parse");
        manifest["kind"] = serde_json::json!("hog-devnode-runtime");
        let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
        fs::write(&path, &bytes).expect("bug: manifest should be writable");
        pointer["manifestHash"] = serde_json::json!(super::sha256_hex(&bytes));

        let error = parse_readiness_line("hog-devnode", &pointer.to_string())
            .expect_err("unexpected runtime manifest kind should fail");

        assert!(matches!(
            error,
            NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
                if reason.contains("hog-devnode-runtime")
                    && reason.contains("hogd-dev-runtime")
        ));
    }

    #[test]
    fn readiness_line_accepts_v1_scheduler_only_endpoint() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let readiness = parse_readiness_line("hogd-dev", &line)
            .expect("bug: v1 readiness payload should parse");

        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.network_hash.as_str(), "00".repeat(32));
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
    fn readiness_manifest_hash_mismatch_is_rejected() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let path = PathBuf::from(
            pointer["manifestPath"]
                .as_str()
                .expect("bug: pointer has manifest path"),
        );
        fs::write(&path, b"tampered").expect("bug: manifest should be writable");
        pointer["manifestHash"] = serde_json::Value::String(super::sha256_hex(b"original"));
        let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

        let err = parse_readiness_line("hogd-dev", &line)
            .expect_err("bug: manifest hash mismatch should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::RuntimeManifestHashMismatch { .. }
        ));
    }

    #[test]
    fn readiness_manifest_hash_accepts_uppercase_hex_digits() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let hash = pointer["manifestHash"]
            .as_str()
            .expect("bug: pointer has manifest hash")
            .to_owned();
        pointer["manifestHash"] =
            serde_json::Value::String(format!("0x{}", hash[2..].to_uppercase()));
        let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

        let readiness = parse_readiness_line("hogd-dev", &line)
            .expect("bug: uppercase manifest hash should parse");

        assert_eq!(readiness.manifest_hash, Some(hash));
    }

    #[test]
    fn readiness_manifest_rejects_group_or_world_permissions() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let path = PathBuf::from(
            pointer["manifestPath"]
                .as_str()
                .expect("bug: pointer has manifest path"),
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("bug: permissions should be set");

        let err = parse_readiness_line("hogd-dev", &line)
            .expect_err("bug: unsafe manifest permissions should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::RuntimeManifestUnsafePermissions { .. }
        ));
    }

    #[test]
    fn readiness_manifest_rejects_symlink_path() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let path = PathBuf::from(
            pointer["manifestPath"]
                .as_str()
                .expect("bug: pointer has manifest path"),
        );
        let symlink_path = path.with_file_name("runtime-manifest-link.json");
        std::os::unix::fs::symlink(&path, &symlink_path).expect("bug: symlink should be created");
        pointer["manifestPath"] = serde_json::Value::String(symlink_path.display().to_string());
        let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

        let err = parse_readiness_line("hogd-dev", &line)
            .expect_err("bug: symlink manifest path should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::RuntimeManifestUnsafePermissions { .. }
        ));
    }

    #[test]
    fn readiness_manifest_rejects_relative_path() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let mut pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        pointer["manifestPath"] = serde_json::Value::String("runtime-manifest.json".into());
        let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

        let err = parse_readiness_line("hogd-dev", &line)
            .expect_err("bug: relative manifest path should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::RuntimeManifestUnsafePermissions { .. }
        ));
    }

    #[test]
    fn manifest_pointer_fields_do_not_fall_back_to_legacy_readiness() {
        let line = format!(
            r#"{{"schemaVersion":2,"status":"ready","manifestPath":"/tmp/missing","manifestHash":"{}","network_id":"groundhog-local","endpoints":{{"scheduler_coord":"127.0.0.1:12345"}}}}"#,
            super::sha256_hex(b"missing"),
        );

        let err = parse_readiness_line("hogd-dev", &line)
            .expect_err("bug: mixed pointer and legacy readiness should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidReadinessJson { .. }
        ));
    }

    #[test]
    fn manifest_pointer_rejects_future_schema_before_decoding_body() {
        let err = parse_readiness_line(
            "hogd-dev",
            r#"{"schemaVersion":99,"manifestPath":42,"futureBody":true}"#,
        )
        .expect_err("bug: future schema should fail before pointer body decoding");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidReadinessSchemaVersion {
                expected: 2,
                actual: 99,
                ..
            }
        ));
    }

    #[test]
    fn full_runtime_manifest_uses_canonical_endpoint_names() {
        let line = manifest_readiness_line(
            r#"{
              "scheduler":"127.0.0.1:12345",
              "indexer":"127.0.0.1:12346",
              "dbGateway":"127.0.0.1:12347",
              "viewGateway":"127.0.0.1:12348"
            }"#,
            r"[]",
        );
        let pointer: serde_json::Value =
            serde_json::from_str(&line).expect("bug: pointer line should parse");
        let manifest_path = pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path");
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(manifest_path).expect("bug: manifest should be readable"),
        )
        .expect("bug: manifest should parse");
        let endpoint_keys = manifest["endpoints"]
            .as_object()
            .expect("bug: endpoints should be object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        assert_eq!(
            sorted(endpoint_keys),
            sorted(["dbGateway", "indexer", "scheduler", "viewGateway"])
        );
    }

    #[test]
    fn readiness_line_parses_dev_accounts() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let accounts = format!(
            r#"[{{"index":0,"role":"local_user","walletAddress":"0x{}","publicKey":"0x{}","initialStorageTokens":"1000000000"}}]"#,
            "11".repeat(32),
            hex::encode(signer.verifying_key_bytes()),
        );
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, &accounts);
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");

        let config = HogdDevConfig::default().with_dev_account_count(1);
        validate_dev_accounts(&readiness, &config)
            .expect("bug: matching readiness dev account should validate");

        assert_eq!(readiness.dev_accounts.len(), 1);
        assert_eq!(
            serde_json::to_value(readiness.dev_accounts[0].address)
                .expect("bug: address serializes"),
            format!("0x{}", "11".repeat(32)),
        );
        assert_eq!(
            readiness.dev_accounts[0].public_key.0,
            signer.verifying_key_bytes().to_vec()
        );
        assert_eq!(
            readiness.dev_accounts[0].initial_storage_tokens,
            1_000_000_000
        );
    }

    #[test]
    fn legacy_readiness_without_manifest_is_rejected_by_supervisor_validation() {
        let readiness =
            parse_readiness_line("hogd-dev", V1_READY).expect("bug: readiness payload parses");
        let err = validate_dev_accounts(&readiness, &HogdDevConfig::default())
            .expect_err("bug: manifest-less readiness should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
                if reason.contains("runtime manifest")
        ));
    }

    #[test]
    fn dev_account_count_mismatch_is_rejected() {
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, r"[]");
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload parses");
        let err = validate_dev_accounts(&readiness, &HogdDevConfig::default())
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
        let accounts = format!(
            r#"[{{"index":0,"role":"local_user","walletAddress":"0x{}","publicKey":"0x{}","initialStorageTokens":"1000000000"}}]"#,
            "11".repeat(32),
            "22".repeat(32),
        );
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, &accounts);
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");
        let config = HogdDevConfig::default().with_dev_account_count(1);
        let err = validate_dev_accounts(&readiness, &config)
            .expect_err("bug: wrong readiness public key should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::DevAccountPublicKeyMismatch { index: 0, .. }
        ));
    }

    #[test]
    fn unsupported_manifest_derivation_scheme_is_rejected() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let accounts = format!(
            r#"[{{"index":0,"role":"local_user","walletAddress":"0x{}","publicKey":"0x{}","initialStorageTokens":"1000000000"}}]"#,
            "11".repeat(32),
            hex::encode(signer.verifying_key_bytes()),
        );
        let line = manifest_readiness_line_with_derivation(
            r#"{"scheduler":"127.0.0.1:12345"}"#,
            &accounts,
            "groundhog-bootstrap-account-v2",
            "groundhog-dev-accounts",
        );
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");
        let config = HogdDevConfig::default().with_dev_account_count(1);
        let err = validate_dev_accounts(&readiness, &config)
            .expect_err("bug: unsupported derivation scheme should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
                if reason.contains("unsupported bootstrap account derivation scheme")
        ));
    }

    #[test]
    fn manifest_derivation_seed_mismatch_is_rejected() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let accounts = format!(
            r#"[{{"index":0,"role":"local_user","walletAddress":"0x{}","publicKey":"0x{}","initialStorageTokens":"1000000000"}}]"#,
            "11".repeat(32),
            hex::encode(signer.verifying_key_bytes()),
        );
        let line = manifest_readiness_line_with_derivation(
            r#"{"scheduler":"127.0.0.1:12345"}"#,
            &accounts,
            super::SUPPORTED_BOOTSTRAP_ACCOUNT_DERIVATION_SCHEME,
            "different-seed",
        );
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");
        let config = HogdDevConfig::default().with_dev_account_count(1);
        let err = validate_dev_accounts(&readiness, &config)
            .expect_err("bug: derivation seed mismatch should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
                if reason.contains("seed")
        ));
    }

    #[test]
    fn zero_dev_accounts_skips_manifest_derivation_validation() {
        let line = manifest_readiness_line_with_derivation(
            r#"{"scheduler":"127.0.0.1:12345"}"#,
            r"[]",
            "groundhog-bootstrap-account-v2",
            "different-seed",
        );
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");
        let config = HogdDevConfig::default().with_dev_account_count(0);

        validate_dev_accounts(&readiness, &config)
            .expect("bug: count-zero dev accounts should not validate derivation metadata");
    }

    #[test]
    fn manifest_token_funding_mismatch_is_rejected() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let accounts = format!(
            r#"[{{"index":0,"role":"local_user","walletAddress":"0x{}","publicKey":"0x{}","initialStorageTokens":"999"}}]"#,
            "11".repeat(32),
            hex::encode(signer.verifying_key_bytes()),
        );
        let line = manifest_readiness_line(r#"{"scheduler":"127.0.0.1:12345"}"#, &accounts);
        let readiness =
            parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");
        let config = HogdDevConfig::default().with_dev_account_count(1);
        let err = validate_dev_accounts(&readiness, &config)
            .expect_err("bug: token mismatch should fail validation");

        assert!(matches!(
            err,
            NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
                if reason.contains("initialStorageTokens")
        ));
    }

    #[test]
    fn non_ready_status_is_rejected() {
        let err = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"starting","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346","dev_control":"127.0.0.1:12347"}}"#,
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

    #[test]
    fn persistent_data_dir_is_private() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let data_dir = tmp.path().join("persistent");
        fs::create_dir(&data_dir).expect("bug: data dir should be created");
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o755))
            .expect("bug: fixture permissions should be set");
        let config = HogdDevConfig::default().with_data_dir(&data_dir);

        prepare_persistent_data_dir(&config).expect("bug: data dir should be prepared");

        assert_eq!(
            fs::metadata(data_dir)
                .expect("bug: data dir metadata should be readable")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn persistent_data_dir_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let target = tmp.path().join("target");
        let data_dir = tmp.path().join("persistent");
        fs::create_dir(&target).expect("bug: target dir should be created");
        symlink(&target, &data_dir).expect("bug: data dir symlink should be created");
        let config = HogdDevConfig::default().with_data_dir(data_dir);

        let err = prepare_persistent_data_dir(&config)
            .expect_err("bug: symlinked data dir should fail closed");
        assert!(matches!(err, NodeSupervisorError::DataDir { .. }));
    }

    #[tokio::test]
    async fn persistent_start_uses_data_dir_pid_file() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let data_dir = temp.path().join("persistent");
        let pid_file = data_dir.join(PERSISTENT_PID_FILE_NAME);
        let config = HogdDevConfig::default()
            .with_data_dir(&data_dir)
            .with_startup_timeout(Duration::from_secs(2));

        let mut process = start_fake_hogd_dev(&config, "ready")
            .await
            .expect("bug: persistent startup should create its canonical PID file");
        assert!(pid_file.exists());
        process.stop().await.expect("bug: fake child should stop");
        assert!(!pid_file.exists());
    }

    #[tokio::test]
    async fn persistent_data_dir_cannot_use_multiple_explicit_pid_files() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let data_dir = temp.path().join("persistent");
        let first_config = HogdDevConfig::default()
            .with_data_dir(&data_dir)
            .with_pid_file(temp.path().join("first.pid"))
            .with_startup_timeout(Duration::from_secs(2));
        let second_config = HogdDevConfig::default()
            .with_data_dir(&data_dir)
            .with_pid_file(temp.path().join("second.pid"))
            .with_startup_timeout(Duration::from_secs(2));
        let mut process = start_fake_hogd_dev(&first_config, "ready")
            .await
            .expect("bug: first persistent child should start");

        let err = start_fake_hogd_dev(&second_config, "ready")
            .await
            .expect_err("bug: the data-directory PID guard should prevent a second child");
        assert!(matches!(
            err,
            NodeSupervisorError::LocalNodeAlreadyRunning { path, .. }
                if path == data_dir.join(PERSISTENT_PID_FILE_NAME)
        ));

        process.stop().await.expect("bug: fake child should stop");
    }

    #[tokio::test]
    async fn fake_child_reports_readiness_and_stops_cleanly() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let marker = temp.path().join("graceful-shutdown");
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_millis(500));

        let mut process = start_fake_hogd_dev_with_env(
            &config,
            "ready",
            GRACEFUL_SHUTDOWN_FILE_ENV,
            marker.as_os_str(),
        )
        .await
        .expect("bug: fake hogd-dev should start");

        assert_eq!(process.readiness().network_hash.as_str(), "00".repeat(32));
        assert!(process.readiness().manifest_path.is_some());
        assert!(process.readiness().manifest_hash.is_some());
        assert!(process.readiness().runtime_manifest.is_some());

        let status = process.stop().await.expect("bug: fake child should stop");
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(marker).expect("bug: graceful marker should be written"),
            "graceful\n"
        );
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
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
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
    async fn unexpected_leader_exit_cleans_descendants_before_releasing_pid_file() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let pid_file = temp.path().join("hog-devnode.pid");
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_millis(100))
            .with_pid_file(pid_file.clone());
        let mut process = start_fake_hogd_dev(&config, "spawn-helper-then-exit")
            .await
            .expect("bug: fake hogd-dev should start");
        let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
            .expect("bug: child pid should fit in pid_t");

        let status = process
            .stopped()
            .await
            .expect("bug: unexpected leader exit cleanup should complete");

        assert!(status.success());
        assert!(!pid_file.exists());
        assert_process_group_exits(process_group).await;
    }

    #[tokio::test]
    async fn cancelled_shutdown_preserves_pid_file_for_recovery() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let pid_file = temp.path().join("hog-devnode.pid");
        let config = HogdDevConfig::default()
            .with_startup_timeout(Duration::from_secs(2))
            .with_shutdown_grace(Duration::from_secs(1))
            .with_pid_file(pid_file.clone());
        let mut process = start_fake_hogd_dev(&config, "ignore-term")
            .await
            .expect("bug: fake hogd-dev should start");
        let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
            .expect("bug: child pid should fit in pid_t");

        let result = tokio::time::timeout(Duration::from_millis(20), process.stop()).await;
        assert!(
            result.is_err(),
            "shutdown should still be in its grace period"
        );
        drop(process);

        assert!(pid_file.exists());
        assert_process_group_exits(process_group).await;
        fs::remove_file(pid_file).expect("bug: test should remove recovery PID file");
    }

    #[tokio::test]
    async fn dropping_persistent_process_sends_sigterm_and_preserves_pid_file() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let marker = temp.path().join("graceful-shutdown");
        let pid_file = temp.path().join("hog-devnode.pid");
        let config = HogdDevConfig::default()
            .with_data_dir(temp.path().join("data"))
            .with_startup_timeout(Duration::from_secs(2))
            .with_pid_file(pid_file.clone());
        let process = start_fake_hogd_dev_with_env(
            &config,
            "ready",
            GRACEFUL_SHUTDOWN_FILE_ENV,
            marker.as_os_str(),
        )
        .await
        .expect("bug: persistent fake hogd-dev should start");
        let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
            .expect("bug: child pid should fit in pid_t");

        drop(process);

        assert_file_contents(&marker, b"graceful\n").await;
        assert_process_group_exits(process_group).await;
        assert!(pid_file.exists());
        fs::remove_file(pid_file).expect("bug: test should remove recovery PID file");
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
    async fn unready_cleanup_sends_sigterm_before_escalating_to_sigkill() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        let marker = temp.path().join("sigterm-observed");
        let pid_file = temp.path().join("hog-devnode.pid");
        let config = HogdDevConfig::default()
            .with_data_dir(temp.path().join("data"))
            .with_pid_file(pid_file.clone())
            .with_startup_timeout(Duration::from_millis(500));

        let error = start_fake_hogd_dev_with_env(
            &config,
            "timeout-record-term-ignore",
            GRACEFUL_SHUTDOWN_FILE_ENV,
            marker.as_os_str(),
        )
        .await
        .expect_err("bug: unready fake should time out and be killed");
        assert!(
            matches!(&error, NodeSupervisorError::ReadinessTimeout { .. }),
            "unexpected startup error: {error:?}"
        );
        assert_eq!(
            fs::read_to_string(marker).expect("bug: SIGTERM marker should be readable"),
            "graceful\n"
        );
        let process_group = fs::read_to_string(&pid_file)
            .expect("bug: recovery PID should remain")
            .trim()
            .parse::<i32>()
            .expect("bug: recovery PID should parse");
        assert_process_group_exits(process_group).await;
        fs::remove_file(pid_file).expect("bug: test should remove recovery PID file");
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
        let bootstrap_manifest = write_bootstrap_manifest(config)
            .expect("bug: bootstrap manifest file should be written");
        let spec = build_hogd_dev_spec_with_bootstrap_manifest(
            fake_hogd_dev_binary(),
            config,
            bootstrap_manifest.path(),
        )
        .env(FAKE_HOGD_DEV_MODE_ENV, mode);
        HogdDevProcess::start_with_spec(config, spec, Some(bootstrap_manifest)).await
    }

    async fn start_fake_hogd_dev_with_env(
        config: &HogdDevConfig,
        mode: &str,
        key: &str,
        value: &std::ffi::OsStr,
    ) -> Result<HogdDevProcess, NodeSupervisorError> {
        let bootstrap_manifest = write_bootstrap_manifest(config)
            .expect("bug: bootstrap manifest file should be written");
        let spec = build_hogd_dev_spec_with_bootstrap_manifest(
            fake_hogd_dev_binary(),
            config,
            bootstrap_manifest.path(),
        )
        .env(FAKE_HOGD_DEV_MODE_ENV, mode)
        .env(key, value);
        HogdDevProcess::start_with_spec(config, spec, Some(bootstrap_manifest)).await
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

    async fn assert_file_contents(path: &Path, expected: &[u8]) {
        for _ in 0..100 {
            if fs::read(path).is_ok_and(|contents| contents == expected) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "bug: expected file contents at {}: {:?}",
            path.display(),
            String::from_utf8_lossy(expected)
        );
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
        env::temp_dir().join(format!("missing-hog-devnode-{unique}"))
    }

    fn manifest_readiness_line(endpoints: &str, accounts: &str) -> String {
        manifest_readiness_line_with_derivation(
            endpoints,
            accounts,
            super::SUPPORTED_BOOTSTRAP_ACCOUNT_DERIVATION_SCHEME,
            "groundhog-dev-accounts",
        )
    }

    fn manifest_readiness_line_with_derivation(
        endpoints: &str,
        accounts: &str,
        derivation_scheme: &str,
        derivation_seed: &str,
    ) -> String {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let manifest_dir = tmp.keep();
        fs::set_permissions(&manifest_dir, fs::Permissions::from_mode(0o700))
            .expect("bug: manifest dir permissions should be set");
        let manifest_path = manifest_dir.join("runtime-manifest.json");
        let manifest = format!(
            r#"{{
              "schemaVersion":2,
              "kind":"hogd-dev-runtime",
              "chainState":"fresh",
              "networkHash":"0000000000000000000000000000000000000000000000000000000000000000",
              "protocol":"hogp/test",
              "genesisHash":"0x{}",
              "producer":{{"binary":"hog-devnode","version":"0.1.0","commitHash":"abcdef0"}},
              "endpoints":{endpoints},
              "bootstrapAccountDerivation":{{
                "scheme":"{derivation_scheme}",
                "seed":"{derivation_seed}"
              }},
              "bootstrapAccounts":{accounts},
              "bootstrap":{{
                "devWallet":{{
                  "contentHash":"0x{}",
                  "deployedCodeHash":"0x{}",
                  "artifactHint":"./dev-wallet.wasm",
                  "deployEntrypoint":"0xadac498222148e4f",
                  "createWalletEntrypoint":"0x6372656174655761",
                  "doActionEntrypoint":"0x646f416374696f6e"
                }},
                "storageCostModel":{{
                  "hash":"0x{}",
                  "baseDataItemCost":"12000",
                  "bytestringCostPerByte":"10",
                  "baseContractDeploymentCost":"12000",
                  "contractDeploymentCostPerByte":"10",
                  "authPerKeyCost":"1111",
                  "rcqCostPerEntry":"8",
                  "replayCacheCostPerEntry":"7",
                  "balanceCost":"1234"
                }}
              }}
            }}"#,
            "42".repeat(32),
            "51".repeat(32),
            "52".repeat(32),
            "53".repeat(32),
        );
        fs::write(&manifest_path, manifest.as_bytes()).expect("bug: manifest should be written");
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600))
            .expect("bug: manifest permissions should be set");
        format!(
            r#"{{"schemaVersion":2,"status":"ready","manifestPath":"{}","manifestHash":"{}"}}"#,
            manifest_path.display(),
            super::sha256_hex(manifest.as_bytes()),
        )
    }

    fn sorted<I, S>(values: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut values = values.into_iter().map(Into::into).collect::<Vec<_>>();
        values.sort();
        values
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
        let _guard = EnvGuard::set(DEVNODE_BINARY_ENV, &path);

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
        let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
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
            "test would clobber existing sibling hog-devnode at {}",
            sibling.display()
        );
        fs::write(&sibling, b"").expect("bug: sibling file should be written");
        fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755))
            .expect("bug: sibling permissions should be set");
        let _file_guard = FileGuard {
            path: sibling.clone(),
        };
        let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
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
        let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
        let _path_guard = EnvGuard::set("PATH", tmp.path());

        let err = resolve_hogd_dev_binary(&HogdDevConfig::default())
            .expect_err("bug: missing hog-devnode should fail");

        assert!(matches!(err, NodeSupervisorError::HogdDevNotFound));
    }
}
