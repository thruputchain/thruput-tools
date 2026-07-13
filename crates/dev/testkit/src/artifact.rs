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

use groundhog_xdr::contract::{Contract, ContractBlob};
use groundhog_xdr::types::SHA256Hash;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};
use xdr_common::BytesM;

use crate::error::{Result, TestkitError};

/// Which artifact format a contract binary is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// A WebAssembly artifact (`.wasm`).
    Wasm,
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
    /// Use in-memory bytes with an explicit kind.
    Bytes {
        name: String,
        bytes: Vec<u8>,
        kind: ArtifactKind,
    },
}

impl ArtifactSpec {
    fn name(&self) -> &str {
        match self {
            Self::Path { name, .. } | Self::Bytes { name, .. } => name,
        }
    }
}

/// A loaded artifact: its bytes, format, precomputed contract hash, and shared
/// on-network registration state.
#[derive(Debug)]
pub(crate) struct ArtifactEntry {
    kind: ArtifactKind,
    bytes: Vec<u8>,
    hash: SHA256Hash,
    registered: AtomicBool,
    register_lock: Mutex<()>,
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

    /// Serialize concurrent first-registrations of this artifact.
    pub(crate) async fn register_guard(&self) -> MutexGuard<'_, ()> {
        self.register_lock.lock().await
    }

    /// Rebuild the contract object for inclusion in `contracts_to_deploy`.
    pub(crate) fn contract(&self) -> Contract {
        contract_from_bytes(self.kind, self.bytes.clone())
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

pub(crate) fn contract_from_bytes(kind: ArtifactKind, bytes: Vec<u8>) -> Contract {
    let blob = ContractBlob {
        data: BytesM(bytes),
    };
    match kind {
        ArtifactKind::Wasm => Contract::kWasmi {
            wasmi_contract: blob,
        },
        ArtifactKind::Rv32 => Contract::kRv32 {
            rv32_contract: blob,
        },
    }
}

pub(crate) fn contract_hash(contract: &Contract) -> Result<SHA256Hash> {
    let encoded = xdrs::txs::encode(contract).map_err(TestkitError::encode)?;
    Ok(SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&encoded))))
}

fn load_entry(spec: ArtifactSpec) -> Result<ArtifactEntry> {
    let (kind, bytes) = match spec {
        ArtifactSpec::Bytes { bytes, kind, .. } => (kind, bytes),
        ArtifactSpec::Path { name, path, kind } => {
            let kind = match kind {
                Some(kind) => kind,
                None => infer_kind(&path)?,
            };
            let bytes = std::fs::read(&path).map_err(|source| TestkitError::ArtifactLoad {
                name,
                path: path.display().to_string(),
                detail: source.to_string(),
            })?;
            (kind, bytes)
        }
    };
    let hash = contract_hash(&contract_from_bytes(kind, bytes.clone()))?;
    Ok(ArtifactEntry {
        kind,
        bytes,
        hash,
        registered: AtomicBool::new(false),
        register_lock: Mutex::new(()),
    })
}

fn infer_kind(path: &Path) -> Result<ArtifactKind> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("wasm") => Ok(ArtifactKind::Wasm),
        Some("rv32a") => Ok(ArtifactKind::Rv32),
        _ => Err(TestkitError::UnknownArtifactExtension {
            path: path.display().to_string(),
        }),
    }
}
