//! hog-devnode process adapter.

use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};
use thruput_json_rpc_types::base58::FixedIdentifier;

use serde::{Deserialize, de::DeserializeOwned};
use tempfile::TempDir;
use thruput_json_rpc_types::{
    ContractAddress, NetworkHashText,
    hex::HexBytes,
    runtime_manifest::{
        BOOTSTRAP_ACCOUNT_ROLE_FAUCET, BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER,
        HOGD_DEV_RUNTIME_MANIFEST_KIND, RuntimeManifest, RuntimeManifestChainState,
        parse_runtime_manifest,
    },
};
use thruput_signing::LocalSigner;
use thruput_tx_validation::ConfigParamName;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    process::{ChildStderr, ChildStdout},
    task::JoinHandle,
    time,
};

use crate::{
    ChildProcessSpec, ChildSupervisor, HogdDevConfig, NodeSupervisorError, SupervisedChild,
    local_state::{LocalChainState, preflight_state_version, state_error},
    pid_file::PidGuard,
};

/// Default devnode binary name.
pub const DEFAULT_HOGD_DEV_BINARY: &str = "hog-devnode";

/// Legacy devnode binary name accepted during the rename transition.
pub const LEGACY_HOGD_DEV_BINARY: &str = "hogd-dev";

/// Environment variable used to override the devnode binary path.
pub const DEVNODE_BINARY_ENV: &str = "THRUPUT_DEVNODE_BINARY";

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
    /// Required for genesis and local block driving.
    #[serde(alias = "scheduler")]
    pub scheduler_coord: SocketAddr,
    /// `SchedGateway` endpoint for public submit and read-side scheduler probes.
    /// Absent only in legacy readiness payloads without a runtime manifest.
    #[serde(default)]
    pub scheduler_gateway: Option<SocketAddr>,
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

/// Public native account metadata advertised by hog-devnode readiness.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub(crate) struct HogdDevReadyAccount {
    /// Runtime list position used only for internal diagnostics.
    pub index: u32,
    /// Infrastructure or local-user role advertised by the runtime manifest.
    pub role: String,
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
    pub network_hash: NetworkHashText,
    /// x23 protocol token expected by hogd-dev services.
    #[serde(default)]
    pub protocol: Option<String>,
    /// x23 endpoints available after startup.
    pub endpoints: HogdDevEndpoints,
    /// Public native account metadata advertised by hogd-dev.
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

/// Validate readiness accounts against explicit public bootstrap allocations.
pub(crate) fn validate_bootstrap_accounts(
    readiness: &HogdDevReadiness,
    config: &HogdDevConfig,
    network_hash: &NetworkHashText,
    faucet_public_key: thruput_json_rpc_types::Ed25519PublicKey,
) -> Result<(), NodeSupervisorError> {
    let fail = |reason: String| NodeSupervisorError::InvalidRuntimeManifest {
        program: DEFAULT_HOGD_DEV_BINARY.to_owned(),
        reason,
    };
    let manifest = readiness.runtime_manifest.as_ref().ok_or_else(|| {
        fail("hog-devnode readiness must advertise a schema-4 runtime manifest".into())
    })?;
    let expected = config.to_bootstrap_manifest(network_hash.clone(), faucet_public_key);
    if manifest.bootstrap_accounts.len() != expected.bootstrap_accounts.len() + 1 {
        return Err(fail(
            "runtime bootstrap allocations do not match public intent".into(),
        ));
    }
    let mut found = std::collections::HashSet::new();
    for allocation in
        std::iter::once(&expected.bootstrap_faucet).chain(expected.bootstrap_accounts.iter())
    {
        let account = manifest
            .bootstrap_accounts
            .iter()
            .find(|account| account.public_key == allocation.public_key)
            .ok_or_else(|| fail("runtime is missing an explicitly allocated public key".into()))?;
        if !found.insert(account.public_key.as_ref().to_vec()) {
            return Err(fail("duplicate bootstrap public key".into()));
        }
        let role = if account.public_key == expected.bootstrap_faucet.public_key {
            BOOTSTRAP_ACCOUNT_ROLE_FAUCET
        } else {
            BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER
        };
        let public_key: [u8; 32] = account
            .public_key
            .as_ref()
            .try_into()
            .map_err(|_| fail("invalid bootstrap public key width".into()))?;
        if account.role != role
            || account.initial_storage_tokens != allocation.initial_storage_tokens
            || account.wallet_address.0
                != thruput_signing::PubkeyAccount::from_public_key_bytes(public_key, 0).address()
        {
            return Err(fail("runtime bootstrap account does not match its public allocation and native identity".into()));
        }
    }
    Ok(())
}

pub(crate) fn validate_runtime_identity(
    readiness: &HogdDevReadiness,
    expected_protocol: &str,
    expected_network_hash: &NetworkHashText,
) -> Result<(), NodeSupervisorError> {
    if readiness.network_hash != *expected_network_hash {
        return Err(NodeSupervisorError::RuntimeNetworkHashMismatch {
            expected: expected_network_hash.clone(),
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
    local_state: Option<LocalChainState>,
}

impl HogdDevProcess {
    /// Start hog-devnode using the configured binary lookup.
    #[tracing::instrument(skip_all, fields(listen_host = %config.listen_host))]
    pub async fn start(config: &HogdDevConfig) -> Result<Self, NodeSupervisorError> {
        Self::start_with_inherited_file(config, None).await
    }

    pub(crate) async fn start_with_inherited_file(
        config: &HogdDevConfig,
        inherited_file: Option<std::fs::File>,
    ) -> Result<Self, NodeSupervisorError> {
        let binary = resolve_hogd_dev_binary(config)?;
        Self::start_with_binary_and_file(config, binary, inherited_file).await
    }

    /// Start hog-devnode from an already-resolved binary path.
    #[tracing::instrument(skip_all, fields(binary = %binary.as_ref().display()))]
    pub async fn start_with_binary(
        config: &HogdDevConfig,
        binary: impl AsRef<Path>,
    ) -> Result<Self, NodeSupervisorError> {
        Self::start_with_binary_and_file(config, binary, None).await
    }

    async fn start_with_binary_and_file(
        config: &HogdDevConfig,
        binary: impl AsRef<Path>,
        inherited_file: Option<std::fs::File>,
    ) -> Result<Self, NodeSupervisorError> {
        preflight_state_version(config)?;
        prepare_persistent_data_dir(config)?;
        let binary = binary.as_ref();
        let pid_guards = acquire_pid_guards(config, binary.as_os_str()).await?;
        // Preparing the chain does blocking file I/O and may wait out a
        // contended chain lock, so it runs off the async thread.
        let prepare_config = config.clone();
        let state =
            match tokio::task::spawn_blocking(move || LocalChainState::prepare(&prepare_config))
                .await
            {
                Ok(state) => state?,
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(_) => {
                    return Err(state_error(
                        config
                            .data_dir
                            .as_deref()
                            .unwrap_or(Path::new("ephemeral faucet")),
                        "local chain preparation was cancelled",
                    ));
                }
            };
        let effective = state.config.clone();
        let bootstrap_manifest =
            write_bootstrap_manifest(&effective, &state.network_hash, state.public_key())?;
        let spec = build_hogd_dev_spec_with_bootstrap_manifest(
            binary,
            &effective,
            bootstrap_manifest.path(),
        );
        Self::start_with_spec(
            &effective,
            spec,
            Some(bootstrap_manifest),
            inherited_file,
            Some(PreparedLaunch { state, pid_guards }),
        )
        .await
    }

    async fn start_with_spec(
        config: &HogdDevConfig,
        spec: ChildProcessSpec,
        bootstrap_manifest: Option<BootstrapManifestFile>,
        inherited_file: Option<std::fs::File>,
        prepared: Option<PreparedLaunch>,
    ) -> Result<Self, NodeSupervisorError> {
        prepare_persistent_data_dir(config)?;
        let (mut pid_guards, local_state) = match prepared {
            Some(prepared) => (prepared.pid_guards, Some(prepared.state)),
            None => (acquire_pid_guards(config, spec.program()).await?, None),
        };
        let chain_lock = local_state
            .as_ref()
            .and_then(|state| state.lock.as_ref())
            .map(|file| file.try_clone())
            .transpose()
            .map_err(|source| NodeSupervisorError::DataDir {
                path: config.data_dir.clone().unwrap_or_default(),
                source,
            })?;
        let mut child = ChildSupervisor::new(spec).spawn_with_files(inherited_file, chain_lock)?;
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

        if let Some(state) = &local_state
            && let Err(error) = validate_bootstrap_accounts(
                &readiness,
                config,
                &state.network_hash,
                state.public_key(),
            )
        {
            kill_unready_child(&mut child).await;
            wait_for_stderr_task(stderr_task.take()).await;
            return Err(error);
        }
        Ok(Self {
            readiness,
            child,
            stderr_task,
            shutdown_grace: config.shutdown_grace,
            data_dir: config.data_dir.clone(),
            pid_file: config.pid_file.clone(),
            pid_guards,
            _bootstrap_manifest: bootstrap_manifest,
            local_state,
        })
    }

    /// Parsed readiness metadata.
    pub fn readiness(&self) -> &HogdDevReadiness {
        &self.readiness
    }

    /// Identity selected from initialization input or persisted state before spawning.
    pub(crate) fn expected_network_hash(&self) -> &NetworkHashText {
        &self
            .local_state
            .as_ref()
            .expect("bug: supervised node must retain its prepared local chain state")
            .network_hash
    }

    pub(crate) fn faucet_signer(&self) -> Result<std::sync::Arc<LocalSigner>, NodeSupervisorError> {
        self.local_state
            .as_ref()
            .map(|state| state.signer.clone())
            .ok_or_else(|| NodeSupervisorError::InvalidBootstrapManifestConfig {
                reason: "local faucet credential was not prepared".into(),
            })
    }

    pub(crate) fn funding_journal(
        &self,
    ) -> Result<
        std::sync::Arc<std::sync::Mutex<crate::funding_journal::FundingJournal>>,
        NodeSupervisorError,
    > {
        self.local_state
            .as_ref()
            .map(|state| state.journal.clone())
            .ok_or_else(|| NodeSupervisorError::InvalidBootstrapManifestConfig {
                reason: "local funding journal was not prepared".into(),
            })
    }

    pub(crate) fn funding_lock_lease(&self) -> Option<std::sync::Arc<File>> {
        self.local_state
            .as_ref()
            .and_then(|state| state.lock.clone())
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
        self.release_chain_lock();
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
        self.release_chain_lock();
        Ok(status)
    }

    fn release_chain_lock(&mut self) {
        if let Some(state) = &mut self.local_state {
            state.lock.take();
        }
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

struct PreparedLaunch {
    state: LocalChainState,
    pid_guards: Vec<PidGuard>,
}

async fn acquire_pid_guards(
    config: &HogdDevConfig,
    program: &std::ffi::OsStr,
) -> Result<Vec<PidGuard>, NodeSupervisorError> {
    let persistent_pid_file = config
        .data_dir
        .as_ref()
        .map(|data_dir| data_dir.join(PERSISTENT_PID_FILE_NAME));
    let mut pid_guards = Vec::new();
    if let Some(pid_file) = &persistent_pid_file {
        pid_guards.push(
            PidGuard::acquire_persistent(pid_file.clone(), program, config.shutdown_grace).await?,
        );
    }
    if let Some(pid_file) = &config.pid_file
        && Some(pid_file) != persistent_pid_file.as_ref()
    {
        let guard = if config.data_dir.is_some() {
            PidGuard::acquire_persistent(pid_file.clone(), program, config.shutdown_grace).await?
        } else {
            PidGuard::acquire(pid_file.clone(), program, config.shutdown_grace).await?
        };
        pid_guards.push(guard);
    }

    Ok(pid_guards)
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
    network_hash: &NetworkHashText,
    faucet_public_key: thruput_json_rpc_types::Ed25519PublicKey,
) -> Result<BootstrapManifestFile, NodeSupervisorError> {
    let manifest = config.to_bootstrap_manifest(network_hash.clone(), faucet_public_key);
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

/// Build the hogd-dev command specification used by the supervisor path.
pub fn build_hogd_dev_spec_with_bootstrap_manifest(
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

    spec = append_contract_artifact_directories(spec, &config.contract_artifact_directories);
    spec = append_network_config_overrides(spec, &config.network_config_overrides);

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

    if let Some(ramdb_bytes) = config.ramdb_bytes {
        spec = spec.args([
            OsString::from("--ramdb-bytes"),
            OsString::from(ramdb_bytes.to_string()),
        ]);
    }

    // Last, so hog-devnode's last-wins parsing lets these override the flags above.
    spec = spec.args(config.extra_args.iter().cloned());

    #[cfg(test)]
    for (key, value) in &config.test_child_env {
        spec = spec.env(key.clone(), value.clone());
    }

    #[cfg(unix)]
    let spec = spec.process_group(0);

    spec
}

fn append_contract_artifact_directories(
    mut spec: ChildProcessSpec,
    directories: &[PathBuf],
) -> ChildProcessSpec {
    for directory in directories {
        spec = spec.args([
            OsString::from("--contract-artifacts-dir"),
            directory.as_os_str().to_owned(),
        ]);
    }
    spec
}

fn append_network_config_overrides(
    mut spec: ChildProcessSpec,
    overrides: &BTreeMap<ConfigParamName, u64>,
) -> ChildProcessSpec {
    for (name, value) in overrides {
        spec = spec.args([
            OsString::from("--network-config"),
            OsString::from(format!("{name}={value}")),
        ]);
    }
    spec
}

/// Resolve the devnode binary path using the v1 lookup order.
pub fn resolve_hogd_dev_binary(config: &HogdDevConfig) -> Result<PathBuf, NodeSupervisorError> {
    if let Some(binary) = resolve_hogd_dev_binary_before_path(config)? {
        return Ok(binary);
    }

    resolve_hogd_dev_binary_on_path().ok_or(NodeSupervisorError::HogdDevNotFound)
}

/// Resolve configured, environment, or sibling binaries without consulting `PATH`.
///
/// Node-start orchestration uses this boundary to insert managed-toolchain acquisition
/// ahead of the final `PATH` lookup. This function performs no downloads or filesystem
/// mutations.
pub fn resolve_hogd_dev_binary_before_path(
    config: &HogdDevConfig,
) -> Result<Option<PathBuf>, NodeSupervisorError> {
    if let Some(binary_path) = &config.binary_path {
        return validate_configured_binary(binary_path).map(Some);
    }

    if let Some(env_path) = env::var_os(DEVNODE_BINARY_ENV) {
        return validate_configured_binary(Path::new(&env_path)).map(Some);
    }

    let current_exe =
        env::current_exe().map_err(|source| NodeSupervisorError::CurrentExe { source })?;
    if let Some(parent) = current_exe.parent() {
        for binary in [DEFAULT_HOGD_DEV_BINARY, LEGACY_HOGD_DEV_BINARY] {
            let sibling = parent.join(binary);
            if is_executable_file(&sibling) {
                return Ok(Some(sibling));
            }
        }
    }

    Ok(None)
}

/// Resolve the canonical or legacy devnode binary from `PATH` only.
pub fn resolve_hogd_dev_binary_on_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            for binary in [DEFAULT_HOGD_DEV_BINARY, LEGACY_HOGD_DEV_BINARY] {
                let candidate = dir.join(binary);
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }

    None
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
        if envelope.schema_version != 4 {
            return Err(NodeSupervisorError::InvalidReadinessSchemaVersion {
                program: program.to_string(),
                expected: 4,
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
    debug_assert_eq!(pointer.schema_version, 4);
    if pointer.status != "ready" {
        return Err(NodeSupervisorError::InvalidReadinessStatus {
            program: program.to_string(),
            status: pointer.status,
        });
    }

    validate_manifest_hash_format(program, &pointer.manifest_hash)?;
    let manifest_path = pointer.manifest_path;
    let advertised_manifest_hash = pointer.manifest_hash;
    let manifest_bytes = read_runtime_manifest_bytes(program, &manifest_path)?;
    let actual_hash = runtime_manifest_hash(&manifest_bytes);
    if actual_hash != advertised_manifest_hash {
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
    let scheduler_gateway = manifest.endpoints.scheduler_gateway.ok_or_else(|| {
        NodeSupervisorError::InvalidRuntimeManifest {
            program: program.to_string(),
            reason: "hog-devnode runtime manifest is missing endpoints.schedulerGateway"
                .to_string(),
        }
    })?;
    let dev_accounts = manifest
        .bootstrap_accounts
        .iter()
        .enumerate()
        .map(|(position, account)| {
            let initial_storage_tokens = account.initial_storage_tokens.parse::<u64>().map_err(
                |source| NodeSupervisorError::InvalidRuntimeManifest {
                    program: program.to_string(),
                    reason: format!(
                        "bootstrap account {position} initialStorageTokens is not a u64: {source}"
                    ),
                },
            )?;
            Ok(HogdDevReadyAccount {
                index: u32::try_from(position).map_err(|_| {
                    NodeSupervisorError::InvalidRuntimeManifest {
                        program: program.to_owned(),
                        reason: "too many bootstrap accounts".into(),
                    }
                })?,
                role: account.role.clone(),
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
            scheduler_gateway: Some(scheduler_gateway),
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

fn runtime_manifest_hash(bytes: &[u8]) -> String {
    thruput_json_rpc_types::SHA256Hash::from(thruput_xdr::types::SHA256Hash::from(
        <[u8; 32]>::from(Sha256::digest(bytes)),
    ))
    .to_string()
}

fn validate_manifest_hash_format(program: &str, hash: &str) -> Result<(), NodeSupervisorError> {
    if thruput_xdr::types::SHA256Hash::decode_base58(hash).is_ok() {
        return Ok(());
    }
    Err(NodeSupervisorError::InvalidRuntimeManifestHash {
        program: program.to_string(),
        hash: hash.to_string(),
    })
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
mod tests;
