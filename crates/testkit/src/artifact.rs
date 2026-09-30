//! Contract artifacts: a scenario-level registry of contract binaries.
//!
//! A contract binary must be registered on the network before it can be
//! deployed by hash. Registration means sending the binary in a transaction's
//! `contracts_to_deploy` field; the binary is visible to a `deploy` syscall in
//! that same transaction, and to other transactions only once the registering
//! transaction commits. Sending the binary again is wasteful, so the harness
//! registers each artifact at most once per run.
//!
//! The registry precomputes each artifact's contract hash (its network
//! identity) at scenario build, and tracks whether it has been registered this
//! run. The registration state is shared across agents; the actual register /
//! deploy transactions are performed by an agent's [`crate::agent::AgentContext`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_xdr::contract::{Contract, ContractBlob};
use groundhog_xdr::types::{ContractAddress, SHA256Hash};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};
use xdr_common::BytesM;

use crate::error::{Result, TestkitError};
use crate::nonce::NonceNamespace;

/// Which artifact format a contract binary is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// A lowered RV32 artifact (`.rv32a`).
    Rv32,
}

/// Builder-side description of an artifact, resolved at scenario build.
#[derive(Debug)]
pub(crate) enum ArtifactSpec {
    /// Load bytes from a file; infer the kind from the extension when absent.
    Path {
        name: String,
        path: PathBuf,
        kind: Option<ArtifactKind>,
    },
    /// Use in-memory RV32 bytes.
    Bytes { name: String, bytes: Vec<u8> },
}

impl ArtifactSpec {
    fn name(&self) -> &str {
        match self {
            Self::Path { name, .. } | Self::Bytes { name, .. } => name,
        }
    }
}

/// A loaded artifact: its bytes, precomputed contract hash, and shared
/// on-network registration state.
#[derive(Debug)]
pub(crate) struct ArtifactEntry {
    bytes: Vec<u8>,
    hash: SHA256Hash,
    registered: AtomicBool,
    pending_registration: Mutex<Option<PendingRegistration>>,
}

/// A binary-carrying transaction retained for observation and recovery.
#[derive(Debug, Clone)]
pub(crate) struct PendingRegistration {
    pub(crate) tx_hash: TxHash,
    pub(crate) tx_hex: String,
    pub(crate) owner: RegistrationOwner,
    pub(crate) effects: Vec<u8>,
    pub(crate) address: ContractAddress,
}

/// The account and scenario run that submitted an artifact registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegistrationOwner {
    pub(crate) address: ContractAddress,
    pub(crate) namespace: NonceNamespace,
}

impl ArtifactEntry {
    /// The contract's network identity (`sha256` of the encoded contract).
    pub(crate) fn hash(&self) -> SHA256Hash {
        self.hash
    }

    /// Whether the harness has observed this artifact registered this run.
    pub(crate) fn registered(&self) -> bool {
        self.registered.load(Ordering::Acquire)
    }

    /// Record that a registering transaction has committed.
    pub(crate) fn mark_registered(&self) {
        self.registered.store(true, Ordering::Release);
    }

    /// Serialize and track first-registration submission for this artifact.
    pub(crate) async fn registration_guard(&self) -> MutexGuard<'_, Option<PendingRegistration>> {
        self.pending_registration.lock().await
    }

    /// Resolve a matching in-flight registration attempt.
    pub(crate) async fn finish_registration(&self, tx_hash: TxHash, succeeded: bool) {
        let mut pending = self.pending_registration.lock().await;
        if pending.as_ref().map(|attempt| attempt.tx_hash) != Some(tx_hash) {
            return;
        }
        if succeeded {
            self.mark_registered();
        }
        *pending = None;
    }

    /// Rebuild the contract object for inclusion in `contracts_to_deploy`.
    pub(crate) fn contract(&self) -> Contract {
        contract_from_bytes(self.bytes.clone())
    }
}

/// A scenario's registered contract artifacts, keyed by name.
#[derive(Debug)]
pub(crate) struct Registry {
    entries: HashMap<String, Arc<ArtifactEntry>>,
}

impl Registry {
    /// Load and validate every artifact spec, computing contract hashes.
    pub(crate) fn from_specs(specs: Vec<ArtifactSpec>) -> Result<Self> {
        let mut entries = HashMap::with_capacity(specs.len());
        for spec in specs {
            let name = spec.name().to_owned();
            if entries.contains_key(&name) {
                return Err(TestkitError::DuplicateArtifact { name });
            }
            entries.insert(name, Arc::new(load_entry(spec)?));
        }
        Ok(Self { entries })
    }

    /// Look up an artifact by name.
    pub(crate) fn get(&self, name: &str) -> Result<Arc<ArtifactEntry>> {
        self.entries
            .get(name)
            .map(Arc::clone)
            .ok_or_else(|| TestkitError::UnknownArtifact {
                name: name.to_owned(),
            })
    }
}

pub(crate) fn contract_from_bytes(bytes: Vec<u8>) -> Contract {
    Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(bytes),
        },
    }
}

pub(crate) fn contract_hash(contract: &Contract) -> Result<SHA256Hash> {
    let encoded = xdrs::txs::encode(contract).map_err(TestkitError::encode)?;
    Ok(SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&encoded))))
}

fn load_entry(spec: ArtifactSpec) -> Result<ArtifactEntry> {
    let bytes = match spec {
        ArtifactSpec::Bytes { bytes, .. } => bytes,
        ArtifactSpec::Path { name, path, kind } => {
            if kind.is_none() {
                validate_inferred_kind(&path)?;
            }
            std::fs::read(&path).map_err(|source| TestkitError::ArtifactLoad {
                name,
                path: path.display().to_string(),
                detail: source.to_string(),
            })?
        }
    };
    let hash = contract_hash(&contract_from_bytes(bytes.clone()))?;
    Ok(ArtifactEntry {
        bytes,
        hash,
        registered: AtomicBool::new(false),
        pending_registration: Mutex::new(None),
    })
}

fn validate_inferred_kind(path: &Path) -> Result<()> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rv32a") => Ok(()),
        _ => Err(TestkitError::UnknownArtifactExtension {
            path: path.display().to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use groundhog_json_rpc_types::{hex::Hex0x, transaction::TxHash};
    use groundhog_xdr::contract::Contract;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};

    use crate::nonce::NonceNamespace;

    use super::{
        ArtifactKind, ArtifactSpec, RegistrationOwner, Registry, contract_from_bytes,
        validate_inferred_kind,
    };

    #[test]
    fn raw_artifact_bytes_build_an_rv32_contract() {
        let bytes = vec![1, 2, 3];
        let contract = contract_from_bytes(bytes.clone());

        match contract {
            Contract::kRv32 { rv32_contract } => assert_eq!(rv32_contract.data.0, bytes),
            other => panic!("expected RV32 contract, got {other:?}"),
        }
    }

    #[test]
    fn inferred_artifact_kind_requires_rv32_extension() {
        assert!(validate_inferred_kind(Path::new("counter.rv32a")).is_ok());
        assert!(validate_inferred_kind(Path::new("counter.wasm")).is_err());
        assert!(validate_inferred_kind(Path::new("counter")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn inferred_artifact_kind_rejects_non_utf8_extension() {
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(std::ffi::OsString::from_vec(b"counter.\xff".to_vec()));
        assert!(validate_inferred_kind(&path).is_err());
    }

    #[test]
    fn explicit_rv32_kind_overrides_filename_extension() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/artifact.rs");
        let registry = Registry::from_specs(vec![ArtifactSpec::Path {
            name: "counter".to_owned(),
            path,
            kind: Some(ArtifactKind::Rv32),
        }]);

        assert!(registry.is_ok());
    }

    #[tokio::test]
    async fn registration_resolution_only_clears_the_matching_transaction() {
        let registry = Registry::from_specs(vec![ArtifactSpec::Bytes {
            name: "counter".to_owned(),
            bytes: vec![1, 2, 3],
        }])
        .expect("bug: in-memory artifact should load");
        let entry = registry.get("counter").expect("bug: artifact exists");
        let pending_hash = TxHash(Hex0x(SHA256Hash::from([0x11; 32])));
        let other_hash = TxHash(Hex0x(SHA256Hash::from([0x22; 32])));
        *entry.registration_guard().await = Some(super::PendingRegistration {
            tx_hash: pending_hash,
            tx_hex: "0x01".to_owned(),
            effects: Vec::new(),
            owner: RegistrationOwner {
                address: [0; 32].into(),
                namespace: NonceNamespace::for_run("seed", 0),
            },
            address: [0; 32].into(),
        });

        entry.finish_registration(other_hash, true).await;
        assert_eq!(
            entry
                .registration_guard()
                .await
                .as_ref()
                .map(|attempt| attempt.tx_hash),
            Some(pending_hash)
        );
        assert!(!entry.registered());

        entry.finish_registration(pending_hash, true).await;
        assert!(entry.registration_guard().await.is_none());
        assert!(entry.registered());
    }

    #[test]
    fn registration_owner_distinguishes_repeated_runs_using_the_same_account() {
        let address = ContractAddress::from([0x42; 32]);

        assert_ne!(
            RegistrationOwner {
                address,
                namespace: NonceNamespace::for_run("seed", 0),
            },
            RegistrationOwner {
                address,
                namespace: NonceNamespace::for_run("seed", 1),
            }
        );
    }
}
