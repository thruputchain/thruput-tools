//! Private state and lock ownership for one local chain.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use serde::Deserialize;
use thruput_json_rpc_types::{
    NetworkHashText, base58::Base58, bootstrap_manifest::BootstrapManifestAllocation,
};
use thruput_signing::LocalSigner;
use thruput_tx_validation::ConfigParamName;
use zeroize::Zeroizing;

use crate::{
    HogdDevConfig, NodeSupervisorError, config::LOCAL_FAUCET_STORAGE_TOKENS,
    funding_journal::FundingJournal,
};

/// Private faucet credential included in a local-chain backup.
/// Keep this storage identity stable across tool branding changes.
pub const LOCAL_FAUCET_KEY_FILE: &str = "groundhog-faucet.key";
/// Retained signed funding requests included in a local-chain backup.
pub const LOCAL_FUNDING_ATTEMPTS_FILE: &str = "groundhog-funding.json";
const STATE_METADATA_FILE: &str = "hog-devnode-data.json";
const MAX_METADATA_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub(crate) struct LocalChainState {
    pub(crate) signer: Arc<LocalSigner>,
    pub(crate) config: HogdDevConfig,
    pub(crate) network_hash: NetworkHashText,
    pub(crate) lock: Option<Arc<File>>,
    pub(crate) journal: Arc<Mutex<FundingJournal>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChainMetadata {
    schema_version: u32,
    kind: String,
    bootstrap_config: PersistedBootstrap,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedBootstrap {
    bootstrap_faucet: BootstrapManifestAllocation,
    bootstrap_accounts: Vec<BootstrapManifestAllocation>,
    #[serde(default, deserialize_with = "read_overrides")]
    network_config_overrides: BTreeMap<ConfigParamName, u64>,
}

/// hog-devnode writes a parameter wider than 32 bits as a decimal string, as
/// genesis JSON does, and the rest as JSON integers.
#[derive(Deserialize)]
#[serde(untagged)]
enum OverrideValue {
    Integer(u64),
    Decimal(String),
}

fn read_overrides<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ConfigParamName, u64>, D::Error> {
    struct Overrides;
    impl<'de> serde::de::Visitor<'de> for Overrides {
        type Value = BTreeMap<ConfigParamName, u64>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("named uint64 network overrides")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut overrides = BTreeMap::new();
            while let Some((name, value)) = map.next_entry::<String, OverrideValue>()? {
                let parameter = name.parse().map_err(serde::de::Error::custom)?;
                let value = match value {
                    OverrideValue::Integer(value) => value,
                    OverrideValue::Decimal(text) => text.parse().map_err(|_| {
                        serde::de::Error::custom(format!(
                            "network override {parameter} is not a uint64 decimal: {text:?}"
                        ))
                    })?,
                };
                if overrides.insert(parameter, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate network override"));
                }
            }
            Ok(overrides)
        }
    }
    deserializer.deserialize_map(Overrides)
}

impl LocalChainState {
    pub(crate) fn prepare(config: &HogdDevConfig) -> Result<Self, NodeSupervisorError> {
        let Some(dir) = config.data_dir.as_deref() else {
            let network_hash = config
                .init_network_hash
                .clone()
                .unwrap_or_else(|| [0; 32].into());
            let signer = Arc::new(random_signer(Path::new("ephemeral faucet"))?);
            let faucet = Base58(
                thruput_signing::PubkeyAccount::from_public_key_bytes(
                    signer.verifying_key_bytes(),
                    0,
                )
                .address(),
            );
            return Ok(Self {
                signer,
                config: config.clone(),
                network_hash: network_hash.clone(),
                lock: None,
                journal: Arc::new(Mutex::new(FundingJournal::memory(network_hash, faucet))),
            });
        };
        let lock = acquire_chain_lock(dir)?;
        let metadata = read_chain_metadata(dir)?;
        reject_reinitialization(config, metadata.is_some())?;
        if metadata.is_none() {
            reject_unidentified_state(dir)?;
        }
        let key_path = dir.join(LOCAL_FAUCET_KEY_FILE);
        let signer = load_or_create_faucet(&key_path, metadata.is_none())?;
        let faucet = Base58(
            thruput_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0)
                .address(),
        );
        let signer = Arc::new(signer);
        let journal = FundingJournal::open(
            dir,
            config.init_network_hash.clone(),
            faucet,
            metadata.is_none(),
            &signer,
        )?;
        let mut effective = config.clone();
        if let Some(metadata) = metadata {
            if metadata
                .bootstrap_config
                .bootstrap_faucet
                .public_key
                .as_ref()
                != signer.verifying_key_bytes()
                || metadata
                    .bootstrap_config
                    .bootstrap_faucet
                    .initial_storage_tokens
                    != LOCAL_FAUCET_STORAGE_TOKENS.to_string()
            {
                return Err(state_error(
                    &key_path,
                    "faucet credential does not match the chain's public bootstrap intent",
                ));
            }
            if metadata.bootstrap_config.network_config_overrides != config.network_config_overrides
            {
                return Err(state_error(
                    dir,
                    "network overrides differ from the chain's persisted intent",
                ));
            }
            effective.bootstrap_accounts = metadata.bootstrap_config.bootstrap_accounts;
        }
        Ok(Self {
            signer,
            config: effective,
            network_hash: journal.network_hash().clone(),
            lock: Some(Arc::new(lock)),
            journal: Arc::new(Mutex::new(journal)),
        })
    }

    pub(crate) fn public_key(&self) -> thruput_json_rpc_types::Ed25519PublicKey {
        Base58(thruput_xdr::types::Ed25519PublicKey::from(
            self.signer.verifying_key_bytes(),
        ))
    }
}

/// Check old versions before orphan cleanup, then check again under the chain lock.
pub(crate) fn preflight_state_version(config: &HogdDevConfig) -> Result<(), NodeSupervisorError> {
    if let Some(dir) = config.data_dir.as_deref() {
        let metadata = read_chain_metadata(dir)?;
        reject_reinitialization(config, metadata.is_some())?;
    }
    Ok(())
}

fn reject_reinitialization(
    config: &HogdDevConfig,
    has_metadata: bool,
) -> Result<(), NodeSupervisorError> {
    if let Some(dir) = config.data_dir.as_deref()
        && config.init_network_hash.is_some()
        && (has_metadata || entry_exists(&dir.join(LOCAL_FUNDING_ATTEMPTS_FILE))?)
    {
        return Err(already_initialized(dir));
    }
    Ok(())
}

pub(crate) fn already_initialized(path: &Path) -> NodeSupervisorError {
    state_error(
        path,
        "directory is already initialized; --init-network-hash is only for a new chain; omit it to restart with the persisted network identity",
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    schema_version: u32,
}

fn read_chain_metadata(dir: &Path) -> Result<Option<ChainMetadata>, NodeSupervisorError> {
    let path = dir.join(STATE_METADATA_FILE);
    let file = match regular_file(&path, false) {
        Ok(file) => file,
        Err(_error) if !entry_exists(&path)? => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| state_error(&path, e))?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(state_error(&path, "bootstrap metadata exceeds size limit"));
    }
    let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|e| state_error(&path, e))?;
    if envelope.schema_version != 5 {
        return Err(state_error(
            &path,
            "unsupported local state schema; choose a new data directory for this version",
        ));
    }
    let metadata: ChainMetadata =
        serde_json::from_slice(&bytes).map_err(|e| state_error(&path, e))?;
    if metadata.schema_version != 5 || metadata.kind != "hog-devnode-data" {
        return Err(state_error(&path, "unexpected local state metadata family"));
    }
    Ok(Some(metadata))
}

fn reject_unidentified_state(dir: &Path) -> Result<(), NodeSupervisorError> {
    fn contains_files(path: &Path) -> io::Result<bool> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || contains_files(&entry.path())? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    for name in ["ramdb", "events", "indexer"] {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if !metadata.is_dir()
                    || contains_files(&path).map_err(|e| state_error(&path, e))? =>
            {
                return Err(state_error(
                    dir,
                    "persisted state has no public bootstrap metadata; do not generate a replacement faucet",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(state_error(&path, e)),
        }
    }
    Ok(())
}

fn entry_exists(path: &Path) -> Result<bool, NodeSupervisorError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(state_error(path, e)),
    }
}

fn load_or_create_faucet(key_path: &Path, fresh: bool) -> Result<LocalSigner, NodeSupervisorError> {
    let signer = match private_file(key_path) {
        Ok(file) => {
            let mut bytes = Zeroizing::new([0u8; 64]);
            let mut reader = file.take(65);
            reader.read_exact(bytes.as_mut()).map_err(|_| {
                state_error(
                    key_path,
                    "faucet credential is corrupt; do not replace it for an existing chain",
                )
            })?;
            let mut extra = [0];
            if reader
                .read(&mut extra)
                .map_err(|e| state_error(key_path, e))?
                != 0
            {
                return Err(state_error(
                    key_path,
                    "faucet credential must contain exactly 64 bytes",
                ));
            }
            LocalSigner::from_keypair_bytes(*bytes).map_err(|_| {
                state_error(key_path, "faucet credential has inconsistent keypair bytes")
            })?
        }
        Err(_error) if !entry_exists(key_path)? && fresh => {
            let signer = random_signer(key_path)?;
            let bytes = Zeroizing::new(signer.to_keypair_bytes());
            atomic_private_write(key_path, bytes.as_ref(), false)
                .map_err(|e| state_error(key_path, e))?;
            signer
        }
        Err(error) => return Err(error),
    };
    Ok(signer)
}

fn random_signer(path: &Path) -> Result<LocalSigner, NodeSupervisorError> {
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(secret.as_mut()).map_err(|e| state_error(path, e))?;
    Ok(LocalSigner::from_secret_key(*secret))
}

#[allow(clippy::needless_pass_by_value)] // Error boundary accepts both borrowed reasons and owned I/O errors.
pub(crate) fn state_error(path: &Path, reason: impl ToString) -> NodeSupervisorError {
    NodeSupervisorError::LocalFundingState {
        path: path.to_owned(),
        reason: reason.to_string(),
    }
}

pub(crate) fn private_file(path: &Path) -> Result<File, NodeSupervisorError> {
    regular_file(path, true)
}

fn regular_file(path: &Path, private: bool) -> Result<File, NodeSupervisorError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| state_error(path, e))?;
    let metadata = file.metadata().map_err(|e| state_error(path, e))?;
    if !metadata.is_file() {
        return Err(state_error(path, "expected a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments or pointer contracts.
        if private && (metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() })
        {
            return Err(state_error(
                path,
                "private local state must be owned by the current user with mode 0600",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(file)
}

pub(crate) fn atomic_private_write(path: &Path, bytes: &[u8], replace: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing state parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    if replace {
        temporary.persist(path).map_err(|e| e.error)?;
    } else {
        temporary.persist_noclobber(path).map_err(|e| e.error)?;
    }
    File::open(parent)?.sync_all()
}

/// How long to retry a contended chain lock before reporting another holder.
/// As with the PID-file lock, a child spawned concurrently by another thread
/// briefly co-owns this process's descriptors (and their `flock`s) between
/// fork and exec, so an instantly contended lock does not yet prove another
/// process holds the chain.
#[cfg(unix)]
const CHAIN_LOCK_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
#[cfg(unix)]
const CHAIN_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[cfg(unix)]
fn acquire_chain_lock(dir: &Path) -> Result<File, NodeSupervisorError> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    use std::{thread, time::Instant};
    let path = dir.join(".hog-devnode.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|e| state_error(&path, e))?;
    if !file
        .metadata()
        .map_err(|e| state_error(&path, e))?
        .is_file()
    {
        return Err(state_error(&path, "chain lock must be a regular file"));
    }
    let deadline = Instant::now() + CHAIN_LOCK_RETRY_WINDOW;
    // SAFETY: file owns a valid descriptor; flock retains no pointers.
    while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let contended = io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock;
        if !contended || Instant::now() >= deadline {
            return Err(state_error(dir, "local chain is locked by another process"));
        }
        thread::sleep(CHAIN_LOCK_RETRY_INTERVAL);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn acquire_chain_lock(_dir: &Path) -> Result<File, NodeSupervisorError> {
    Err(NodeSupervisorError::PersistentStateUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn config(dir: &Path) -> HogdDevConfig {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        HogdDevConfig::default().with_data_dir(dir)
    }

    fn persist_metadata(dir: &Path, state: &LocalChainState) {
        let intent = state
            .config
            .to_bootstrap_manifest([0; 32].into(), state.public_key());
        fs::write(
            dir.join(STATE_METADATA_FILE),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 5, "kind": "hog-devnode-data", "bootstrapConfig": {
                    "bootstrapFaucet": intent.bootstrap_faucet,
                    "bootstrapAccounts": intent.bootstrap_accounts,
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn initialized_identity_restarts_without_flag_and_rejects_any_initialization_without_writes() {
        for original_hash in [[0; 32], [0xab; 32]] {
            let dir = tempfile::tempdir().unwrap();
            let initial = config(dir.path()).with_init_network_hash(original_hash.into());
            let state = LocalChainState::prepare(&initial).unwrap();
            assert_eq!(state.network_hash, original_hash.into());
            persist_metadata(dir.path(), &state);
            drop(state);
            let files = [
                LOCAL_FUNDING_ATTEMPTS_FILE,
                LOCAL_FAUCET_KEY_FILE,
                STATE_METADATA_FILE,
            ];
            let original: Vec<_> = files
                .iter()
                .map(|name| fs::read(dir.path().join(name)).unwrap())
                .collect();
            for requested in [[0; 32], [0xab; 32], [0xcd; 32]] {
                let attempt = config(dir.path()).with_init_network_hash(requested.into());
                let preflight = preflight_state_version(&attempt).unwrap_err();
                assert!(preflight.to_string().contains("already initialized"));
                let error = LocalChainState::prepare(&attempt).unwrap_err();
                assert!(error.to_string().contains("omit it to restart"));
                for (name, bytes) in files.iter().zip(&original) {
                    assert_eq!(&fs::read(dir.path().join(name)).unwrap(), bytes);
                }
            }
            let restarted = LocalChainState::prepare(&config(dir.path())).unwrap();
            assert_eq!(restarted.network_hash, original_hash.into());
        }
    }

    #[test]
    fn fresh_ephemeral_and_persistent_identity_defaults_to_zero() {
        for init_network_hash in [None, Some([0; 32].into()), Some([0xab; 32].into())] {
            let expected = init_network_hash.clone().unwrap_or_else(|| [0; 32].into());
            let ephemeral = HogdDevConfig {
                init_network_hash: init_network_hash.clone(),
                ..HogdDevConfig::default()
            };
            let state = LocalChainState::prepare(&ephemeral).unwrap();
            assert_eq!(state.network_hash, expected);
            let dir = tempfile::tempdir().unwrap();
            let persistent = HogdDevConfig {
                data_dir: Some(dir.path().into()),
                ..ephemeral
            };
            let state = LocalChainState::prepare(&persistent).unwrap();
            assert_eq!(state.network_hash, expected);
            assert_eq!(state.journal.lock().unwrap().network_hash(), &expected);
        }
    }

    #[test]
    fn prepared_identity_survives_an_interrupted_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let initial = config(dir.path()).with_init_network_hash([0xab; 32].into());
        let state = LocalChainState::prepare(&initial).unwrap();
        let key = state.public_key();
        drop(state); // No runtime metadata yet, but the journal already binds the identity.
        assert!(
            preflight_state_version(&initial)
                .unwrap_err()
                .to_string()
                .contains("already initialized")
        );
        assert!(
            LocalChainState::prepare(&initial)
                .unwrap_err()
                .to_string()
                .contains("already initialized")
        );
        let retry = LocalChainState::prepare(&config(dir.path())).unwrap();
        assert_eq!(retry.network_hash, [0xab; 32].into());
        assert_eq!(retry.public_key(), key);
    }

    /// A child spawned from another thread and held between fork and exec,
    /// where it co-owns every descriptor this process had open at the fork.
    struct PausedChild {
        release: io::PipeWriter,
        spawner: std::thread::JoinHandle<std::process::ExitStatus>,
    }

    impl PausedChild {
        fn spawn() -> Self {
            use std::os::{fd::AsRawFd, unix::process::CommandExt};

            let (mut forked, forked_writer) = io::pipe().unwrap();
            let (release_reader, release) = io::pipe().unwrap();
            let (forked_fd, release_fd) = (forked_writer.as_raw_fd(), release_reader.as_raw_fd());
            let spawner = std::thread::spawn(move || {
                let mut command = std::process::Command::new("true");
                // SAFETY: the hook only calls write(2) and read(2), which are
                // async-signal-safe, on descriptors this thread keeps open.
                unsafe {
                    command.pre_exec(move || {
                        let mut byte = 0u8;
                        libc::write(forked_fd, (&raw const byte).cast(), 1);
                        libc::read(release_fd, (&raw mut byte).cast(), 1);
                        Ok(())
                    });
                }
                let status = command.status().unwrap();
                drop((forked_writer, release_reader));
                status
            });
            forked.read_exact(&mut [0]).unwrap();
            Self { release, spawner }
        }

        /// Let the child exec after `delay`, from another thread.
        fn release_after(mut self, delay: std::time::Duration) -> std::thread::JoinHandle<()> {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                self.release.write_all(&[0]).unwrap();
                assert!(self.spawner.join().unwrap().success());
            })
        }
    }

    // A child spawned by another thread keeps the released lock's descriptor
    // until it execs, so the next owner must wait out that window rather than
    // report another process.
    #[test]
    fn chain_lock_handoff_waits_out_a_concurrent_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let first = LocalChainState::prepare(&config).unwrap();
        let child = PausedChild::spawn();
        drop(first);
        let released = child.release_after(std::time::Duration::from_millis(50));
        LocalChainState::prepare(&config).unwrap();
        released.join().unwrap();
    }

    #[test]
    fn credential_and_journal_survive_pending_start_and_persisted_restart() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let first = LocalChainState::prepare(&config).unwrap();
        let key = first.public_key();
        assert!(
            LocalChainState::prepare(&config).is_err(),
            "chain lock must stay held"
        );
        for file in [LOCAL_FAUCET_KEY_FILE, LOCAL_FUNDING_ATTEMPTS_FILE] {
            assert_eq!(
                fs::metadata(dir.path().join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(first);
        let pending = LocalChainState::prepare(&config).unwrap();
        assert_eq!(pending.public_key(), key);
        persist_metadata(dir.path(), &pending);
        drop(pending);
        let restarted = LocalChainState::prepare(&config).unwrap();
        assert_eq!(restarted.public_key(), key);
    }

    #[test]
    fn restart_keeps_original_public_allocations_when_user_selection_changes() {
        let dir = tempfile::tempdir().unwrap();
        let original =
            config(dir.path()).with_bootstrap_accounts(vec![BootstrapManifestAllocation::new(
                Base58(thruput_xdr::types::Ed25519PublicKey::from([0x11; 32])),
                "1234",
            )]);
        let state = LocalChainState::prepare(&original).unwrap();
        persist_metadata(dir.path(), &state);
        drop(state);
        let restart = LocalChainState::prepare(&config(dir.path())).unwrap();
        assert_eq!(
            restart.config.bootstrap_accounts,
            original.bootstrap_accounts
        );
        drop(restart);
        let changed = original
            .with_network_config_overrides([(ConfigParamName::MaxBytestringLen, 42)].into());
        assert!(LocalChainState::prepare(&changed).is_err());
    }

    #[test]
    fn existing_chain_never_replaces_missing_corrupt_or_mismatched_private_state() {
        for damaged in [
            "missing-key",
            "corrupt-key",
            "wrong-key",
            "missing-journal",
            "wrong-journal",
            "permissions",
            "symlink",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = config(dir.path());
            let state = LocalChainState::prepare(&config).unwrap();
            persist_metadata(dir.path(), &state);
            drop(state);
            let key = dir.path().join(LOCAL_FAUCET_KEY_FILE);
            let journal = dir.path().join(LOCAL_FUNDING_ATTEMPTS_FILE);
            match damaged {
                "missing-key" => fs::remove_file(&key).unwrap(),
                "corrupt-key" => fs::write(&key, [0; 10]).unwrap(),
                "wrong-key" => fs::write(
                    &key,
                    LocalSigner::from_secret_key([0x33; 32]).to_keypair_bytes(),
                )
                .unwrap(),
                "missing-journal" => fs::remove_file(&journal).unwrap(),
                "wrong-journal" => {
                    let mut data: serde_json::Value =
                        serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
                    data["faucet"] = serde_json::json!(format!("0x{}", "11".repeat(32)));
                    fs::write(&journal, serde_json::to_vec(&data).unwrap()).unwrap();
                }
                "permissions" => {
                    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
                }
                _ => {
                    fs::remove_file(&key).unwrap();
                    symlink(dir.path().join("absent"), &key).unwrap();
                }
            }
            let before = fs::read(&key).ok();
            assert!(LocalChainState::prepare(&config).is_err(), "{damaged}");
            assert_eq!(fs::read(&key).ok(), before, "must not replace {damaged}");
        }
    }

    #[test]
    fn unsupported_state_fails_before_credential_creation() {
        for version in [1, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let config = config(dir.path());
            fs::write(
                dir.path().join(STATE_METADATA_FILE),
                format!(r#"{{"schemaVersion":{version}}}"#),
            )
            .unwrap();
            let error = preflight_state_version(&config).unwrap_err();
            assert!(error.to_string().contains("unsupported local state schema"));
            assert!(!dir.path().join(LOCAL_FAUCET_KEY_FILE).exists());
        }
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        fs::create_dir(dir.path().join("ramdb")).unwrap();
        fs::write(dir.path().join("ramdb/data"), "original").unwrap();
        assert!(LocalChainState::prepare(&config).is_err());
        assert!(!dir.path().join(LOCAL_FAUCET_KEY_FILE).exists());
        assert_eq!(
            fs::read(dir.path().join("ramdb/data")).unwrap(),
            b"original"
        );
    }

    fn persisted_with_overrides(text: &str) -> serde_json::Result<PersistedBootstrap> {
        let faucet = serde_json::to_string(&Base58(thruput_xdr::types::Ed25519PublicKey::from(
            [0x11; 32],
        )))
        .unwrap();
        serde_json::from_str(&format!(
            r#"{{"bootstrapFaucet":{{"publicKey":{faucet},"initialStorageTokens":"1"}},"bootstrapAccounts":[],"networkConfigOverrides":{text}}}"#
        ))
    }

    #[test]
    fn metadata_overrides_read_integers_and_uint64_decimals() {
        // hog-devnode writes uint64 parameters as decimal strings and the
        // rest as JSON integers; which values a parameter admits is its call.
        let persisted = persisted_with_overrides(
            r#"{"max_bytestring_len":42,"max_tx_gas":"18446744073709551615"}"#,
        )
        .unwrap();

        assert_eq!(
            persisted.network_config_overrides,
            [
                (ConfigParamName::MaxBytestringLen, 42),
                (ConfigParamName::MaxTxGas, u64::MAX),
            ]
            .into()
        );
    }

    #[test]
    fn metadata_overrides_reject_unknown_duplicate_and_malformed_values() {
        for text in [
            r#"{"unknown":1}"#,
            r#"{"max_bytestring_len":1,"max_bytestring_len":2}"#,
            r#"{"max_tx_gas":-1}"#,
            r#"{"max_tx_gas":"18446744073709551616"}"#,
            r#"{"max_tx_gas":"1e9"}"#,
            r#"{"max_tx_gas":true}"#,
        ] {
            assert!(persisted_with_overrides(text).is_err(), "{text}");
        }
    }
}
