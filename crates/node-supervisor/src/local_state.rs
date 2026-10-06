//! Private state and lock ownership for one local chain.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use groundhog_json_rpc_types::{bootstrap_manifest::BootstrapManifestAllocation, hex::Hex0x};
use groundhog_signing::LocalSigner;
use groundhog_tx_validation::ConfigParamName;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{
    HogdDevConfig, NodeSupervisorError, config::LOCAL_FAUCET_STORAGE_TOKENS,
    funding_journal::FundingJournal,
};

/// Private faucet credential included in a local-chain backup.
pub const LOCAL_FAUCET_KEY_FILE: &str = "groundhog-faucet.key";
/// Retained signed funding requests included in a local-chain backup.
pub const LOCAL_FUNDING_ATTEMPTS_FILE: &str = "groundhog-funding.json";
const STATE_METADATA_FILE: &str = "hog-devnode-data.json";
const MAX_METADATA_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub(crate) struct LocalChainState {
    pub(crate) signer: Arc<LocalSigner>,
    pub(crate) config: HogdDevConfig,
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
    network_config_overrides: BTreeMap<ConfigParamName, u32>,
}

fn read_overrides<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ConfigParamName, u32>, D::Error> {
    struct Overrides;
    impl<'de> serde::de::Visitor<'de> for Overrides {
        type Value = BTreeMap<ConfigParamName, u32>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("named uint32 network overrides")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut overrides = BTreeMap::new();
            while let Some((name, value)) = map.next_entry::<String, u32>()? {
                let parameter = name.parse().map_err(serde::de::Error::custom)?;
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
            let signer = Arc::new(random_signer(Path::new("ephemeral faucet"))?);
            let faucet = Hex0x(
                groundhog_signing::PubkeyAccount::from_public_key_bytes(
                    signer.verifying_key_bytes(),
                    0,
                )
                .address(),
            );
            return Ok(Self {
                signer,
                config: config.clone(),
                lock: None,
                journal: Arc::new(Mutex::new(FundingJournal::memory([0; 32].into(), faucet))),
            });
        };
        let lock = acquire_chain_lock(dir)?;
        let metadata = read_chain_metadata(dir)?;
        if metadata.is_none() {
            reject_unidentified_state(dir)?;
        }
        let key_path = dir.join(LOCAL_FAUCET_KEY_FILE);
        let signer = load_or_create_faucet(&key_path, metadata.is_none())?;
        let faucet = Hex0x(
            groundhog_signing::PubkeyAccount::from_public_key_bytes(
                signer.verifying_key_bytes(),
                0,
            )
            .address(),
        );
        let signer = Arc::new(signer);
        let journal =
            FundingJournal::open(dir, [0; 32].into(), faucet, metadata.is_none(), &signer)?;
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
            lock: Some(Arc::new(lock)),
            journal: Arc::new(Mutex::new(journal)),
        })
    }

    pub(crate) fn public_key(&self) -> groundhog_json_rpc_types::Ed25519PublicKey {
        Hex0x(groundhog_xdr::types::Ed25519PublicKey::from(
            self.signer.verifying_key_bytes(),
        ))
    }
}

/// Check old versions before orphan cleanup, then check again under the chain lock.
pub(crate) fn preflight_state_version(config: &HogdDevConfig) -> Result<(), NodeSupervisorError> {
    if let Some(dir) = config.data_dir.as_deref() {
        read_chain_metadata(dir)?;
    }
    Ok(())
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
    if envelope.schema_version != 4 {
        return Err(state_error(
            &path,
            "old local state requires its previous runtime and tooling; choose a new data directory for this version",
        ));
    }
    let metadata: ChainMetadata =
        serde_json::from_slice(&bytes).map_err(|e| state_error(&path, e))?;
    if metadata.schema_version != 4 || metadata.kind != "hog-devnode-data" {
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

#[cfg(unix)]
fn acquire_chain_lock(dir: &Path) -> Result<File, NodeSupervisorError> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
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
    // SAFETY: file owns a valid descriptor; flock retains no pointers.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(state_error(dir, "local chain is locked by another process"));
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
                "schemaVersion": 4, "kind": "hog-devnode-data", "bootstrapConfig": {
                    "bootstrapFaucet": intent.bootstrap_faucet,
                    "bootstrapAccounts": intent.bootstrap_accounts,
                }
            }))
            .unwrap(),
        )
        .unwrap();
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
                Hex0x(groundhog_xdr::types::Ed25519PublicKey::from([0x11; 32])),
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
    fn unidentified_and_legacy_state_fail_before_credential_creation() {
        for version in [1, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let config = config(dir.path());
            fs::write(
                dir.path().join(STATE_METADATA_FILE),
                format!(r#"{{"schemaVersion":{version}}}"#),
            )
            .unwrap();
            let error = preflight_state_version(&config).unwrap_err();
            assert!(error.to_string().contains("previous runtime and tooling"));
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

    #[test]
    fn metadata_overrides_reject_unknown_duplicate_and_out_of_range_values() {
        for text in [
            r#"{"unknown":1}"#,
            r#"{"max_bytestring_len":1,"max_bytestring_len":2}"#,
            r#"{"max_bytestring_len":4294967296}"#,
        ] {
            let json = format!(
                r#"{{"bootstrapFaucet":{{"publicKey":"0x{}","initialStorageTokens":"1"}},"bootstrapAccounts":[],"networkConfigOverrides":{text}}}"#,
                "11".repeat(32)
            );
            assert!(serde_json::from_str::<PersistedBootstrap>(&json).is_err());
        }
    }
}
