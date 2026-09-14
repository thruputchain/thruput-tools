use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use flate2::read::GzDecoder;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};
use tempfile::{Builder as TempBuilder, NamedTempFile};
use thiserror::Error;

use crate::{
    Artifact, BundleSource, Catalog, CatalogError, DeclaredFile, FileKind, HostTarget, SourceError,
};

const COMPLETION_SCHEMA_VERSION: u32 = 1;
const MAX_ARCHIVE_MEMBERS: usize = 64;
const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;

/// Failures while verifying or installing a managed bundle.
#[derive(Debug, Error)]
pub enum InstallError {
    /// Catalog lookup or validation failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The configured source could not acquire the selected bytes.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// A managed cache filesystem operation failed.
    #[error("failed to {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The post-download worker could not complete.
    #[error("managed installation worker failed: {0}")]
    Worker(#[source] tokio::task::JoinError),
    /// The caller cancelled post-download installation.
    #[error("managed installation cancelled")]
    Cancelled,
    /// Cleanup cannot remove a bundle being installed or used by a node.
    #[error(
        "toolchain {version}/{target} is busy; stop nodes using it or wait for installation before cleaning"
    )]
    Busy { version: String, target: HostTarget },
    /// The downloaded file length differs from the embedded catalog.
    #[error("archive size mismatch: expected {expected} bytes, observed {observed}")]
    SizeMismatch { expected: u64, observed: u64 },
    /// The downloaded file digest differs from the embedded catalog.
    #[error("archive checksum mismatch: expected {expected}, observed {observed}")]
    ChecksumMismatch { expected: String, observed: String },
    /// The gzip or tar stream is malformed.
    #[error("invalid bundle archive {path}: {source}")]
    Archive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The archive contains more members than the bounded parser accepts.
    #[error("bundle archive exceeds the {limit}-member limit")]
    ArchiveMemberCount { limit: usize },
    /// An archive member path is non-UTF-8, absolute, or contains non-normal components.
    #[error("unsafe bundle archive member path {path:?}")]
    UnsafeArchiveMember { path: PathBuf },
    /// An archive member is not a plain regular file.
    #[error("bundle archive member {path:?} has forbidden type byte {entry_type:#x}")]
    ArchiveMemberType { path: PathBuf, entry_type: u8 },
    /// An archive member is absent from the catalog file allowlist.
    #[error("bundle archive member {path:?} is not declared by the catalog")]
    UndeclaredArchiveMember { path: PathBuf },
    /// The archive contains a declared member more than once.
    #[error("bundle archive member {path:?} is duplicated")]
    DuplicateArchiveMember { path: PathBuf },
    /// One member's declared expanded size exceeds the parser bound.
    #[error("bundle archive member {path:?} exceeds the {limit}-byte size limit")]
    ArchiveMemberSize { path: PathBuf, limit: u64 },
    /// The sum of expanded member sizes exceeds the parser bound.
    #[error("bundle archive exceeds the {limit}-byte expanded size limit")]
    ExpandedSize { limit: u64 },
    /// A required catalog file was absent from the archive.
    #[error("bundle archive is missing declared member {path:?}")]
    MissingArchiveMember { path: PathBuf },
    /// A member yielded a different byte count than its tar header declared.
    #[error("bundle archive member {path:?} declared {expected} bytes but yielded {observed}")]
    ArchiveMemberLength {
        path: PathBuf,
        expected: u64,
        observed: u64,
    },
}

/// A complete managed bundle resolved from the cache.
#[derive(Debug, Clone)]
pub struct Installation {
    version: String,
    target: HostTarget,
    path: PathBuf,
    installed_now: bool,
}

impl Installation {
    /// Return the exact selected `hog-devnode` version.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Return the target triple represented by this installation.
    pub const fn target(&self) -> HostTarget {
        self.target
    }
    /// Return the root directory of the complete bundle.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Return the installed `hog-devnode` executable path.
    pub fn binary_path(&self) -> PathBuf {
        self.path.join("hog-devnode")
    }
    /// Return the installed `dev_wallet.rv32a` path.
    pub fn wallet_path(&self) -> PathBuf {
        self.path.join("contracts/dev_wallet.rv32a")
    }
    /// Return whether this operation installed bytes rather than reusing a complete bundle.
    pub const fn installed_now(&self) -> bool {
        self.installed_now
    }
}

/// A cached bundle protected from cleanup until this guard is dropped.
/// Hold it until the node has stopped using the executable and contract artifacts.
#[derive(Debug)]
pub struct InstallationLease {
    installation: Installation,
    lock: InstallLock,
}

impl InstallationLease {
    /// Transfer the locked descriptor to a child process for inheritance across exec.
    /// The lock remains held until every descriptor for this open file description closes.
    pub fn into_lock_file(self) -> File {
        self.lock.file
    }

    /// Return the executable protected by this lease.
    pub fn binary_path(&self) -> PathBuf {
        self.installation.binary_path()
    }
}

/// Installs catalog-authorized bundles beneath one Groundhog cache root.
#[derive(Debug)]
pub struct Installer {
    cache_root: PathBuf,
}

impl Installer {
    /// Construct an installer using the Groundhog cache root, not the toolchain subdirectory.
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: cache_root.into(),
        }
    }

    /// Return the final versioned installation path without touching the filesystem.
    pub fn installation_path(&self, version: &str, target: HostTarget) -> PathBuf {
        self.toolchain_root().join(version).join(target.triple())
    }

    /// Resolve a complete local installation without downloading or mutating cache state.
    pub fn find(
        &self,
        catalog: &Catalog,
        version: &str,
        target: HostTarget,
    ) -> Result<Option<Installation>, InstallError> {
        let version = catalog.resolve_version(Some(version))?;
        let artifact = catalog.artifact(version, target)?;
        let path = self.installation_path(version, target);
        if !installation_is_complete(&path, version, target, artifact)? {
            return Ok(None);
        }
        Ok(Some(Installation {
            version: version.to_string(),
            target,
            path,
            installed_now: false,
        }))
    }

    /// Resolve a complete bundle under a shared usage lock.
    /// Unlike `find`, this may create lock state. Multiple nodes may share a bundle;
    /// cleanup refuses it until every lease is dropped. Missing bundles return `None`.
    pub async fn lease(
        &self,
        catalog: &Catalog,
        version: &str,
        target: HostTarget,
    ) -> Result<Option<InstallationLease>, InstallError> {
        let version = catalog.resolve_version(Some(version))?;
        let lock = acquire_shared_lock(self.lock_path(version, target)).await?;
        Ok(self
            .find(catalog, version, target)?
            .map(|installation| InstallationLease { installation, lock }))
    }

    /// Remove one version and target while holding the same lock used by installation.
    /// Retired versions may be cleaned even when they are absent from the current catalog.
    /// Returns `Busy` immediately while an installation or node holds the lock.
    pub fn clean(&self, version: &str, target: HostTarget) -> Result<bool, InstallError> {
        crate::catalog::validate_version_name(version)?;
        let file = open_lock(&self.lock_path(version, target))?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(InstallError::Busy {
                    version: version.to_string(),
                    target,
                });
            }
            Err(source) => {
                return Err(io_error(
                    "acquire cleanup lock",
                    &self.lock_path(version, target),
                    source,
                ));
            }
        }
        let _lock = InstallLock { file };
        let removed = remove_installation(&self.installation_path(version, target))?;
        let staging_removed =
            remove_abandoned_staging(&self.toolchain_root().join(version), target)?;
        Ok(removed || staging_removed)
    }

    /// Acquire, verify, and atomically install one exact catalog entry.
    pub async fn install<S: BundleSource + ?Sized>(
        &self,
        catalog: &Catalog,
        version: &str,
        target: HostTarget,
        source: &S,
    ) -> Result<Installation, InstallError> {
        let version = catalog.resolve_version(Some(version))?;
        let artifact = catalog.artifact(version, target)?;
        let version_dir = self.toolchain_root().join(version);
        create_dir_all(&version_dir)?;
        // A concurrent installer may finish and acquire a node lease before us.
        // Recheck completion under a shared lock between exclusive-lock attempts.
        let lock_path = self.lock_path(version, target);
        let lock = loop {
            if let Some(lease) = self.lease(catalog, version, target).await? {
                return Ok(lease.installation.clone());
            }
            let file = open_lock(&lock_path)?;
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => break InstallLock { file },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(source) => {
                    return Err(io_error("acquire installation lock", &lock_path, source));
                }
            }
        };
        let final_path = self.installation_path(version, target);

        if installation_is_complete(&final_path, version, target, artifact)? {
            return Ok(Installation {
                version: version.to_string(),
                target,
                path: final_path,
                installed_now: false,
            });
        }

        remove_abandoned_staging(&version_dir, target)?;
        let download = TempBuilder::new()
            .prefix(&format!(".download-{}-", target.triple()))
            .tempfile_in(&version_dir)
            .map_err(|source| io_error("create download temporary file", &version_dir, source))?;
        source.fetch_to(artifact, download.path()).await?;
        let cancellation = CancelOnDrop::default();
        let pending = PendingInstallation {
            download,
            version_dir,
            final_path,
            version: version.to_string(),
            target,
            artifact: artifact.clone(),
            _lock: lock,
            cancelled: cancellation.0.clone(),
        };
        tokio::task::spawn_blocking(move || pending.finish())
            .await
            .map_err(InstallError::Worker)?
    }

    fn toolchain_root(&self) -> PathBuf {
        self.cache_root.join("toolchain/hog-devnode")
    }

    fn lock_path(&self, version: &str, target: HostTarget) -> PathBuf {
        self.toolchain_root()
            .join(".locks")
            .join(format!("{version}-{}.lock", target.triple()))
    }
}

#[derive(Default)]
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), InstallError> {
    if cancelled.load(Ordering::Acquire) {
        Err(InstallError::Cancelled)
    } else {
        Ok(())
    }
}

struct PendingInstallation {
    download: NamedTempFile,
    version_dir: PathBuf,
    final_path: PathBuf,
    version: String,
    target: HostTarget,
    artifact: Artifact,
    _lock: InstallLock,
    cancelled: Arc<AtomicBool>,
}

impl PendingInstallation {
    fn finish(self) -> Result<Installation, InstallError> {
        check_cancelled(&self.cancelled)?;
        verify_archive(self.download.path(), &self.artifact, &self.cancelled)?;
        let staging = TempBuilder::new()
            .prefix(&format!(".staging-{}-", self.target.triple()))
            .tempdir_in(&self.version_dir)
            .map_err(|source| {
                io_error(
                    "create installation staging directory",
                    &self.version_dir,
                    source,
                )
            })?;
        extract_archive(
            self.download.path(),
            staging.path(),
            &self.artifact,
            &self.cancelled,
        )?;
        check_cancelled(&self.cancelled)?;
        write_completion(staging.path(), &self.version, self.target, &self.artifact)?;
        check_cancelled(&self.cancelled)?;
        remove_installation(&self.final_path)?;
        fs::rename(staging.path(), &self.final_path)
            .map_err(|source| io_error("commit installation", &self.final_path, source))?;
        // Persist the renamed directory entry before reporting a committed installation.
        File::open(&self.version_dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error("sync installation directory", &self.version_dir, source))?;
        Ok(Installation {
            version: self.version,
            target: self.target,
            path: self.final_path,
            installed_now: true,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Completion {
    schema_version: u32,
    version: String,
    target: String,
    archive_sha256: String,
    layout_version: u32,
}

#[derive(Debug)]
struct InstallLock {
    file: File,
}

// Close only: explicit LOCK_UN would also release inherited descriptors' lock.

fn open_lock(path: &Path) -> Result<File, InstallError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            "resolve installation lock parent",
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "lock path has no parent"),
        )
    })?;
    create_dir_all(parent)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error("open installation lock", path, source))?;
    Ok(file)
}

async fn acquire_shared_lock(path: PathBuf) -> Result<InstallLock, InstallError> {
    let file = open_lock(&path)?;
    loop {
        match FileExt::try_lock_shared(&file) {
            Ok(()) => return Ok(InstallLock { file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(source) => return Err(io_error("acquire installation lock", &path, source)),
        }
    }
}

fn verify_archive(
    path: &Path,
    artifact: &Artifact,
    cancelled: &AtomicBool,
) -> Result<(), InstallError> {
    let mut file = File::open(path).map_err(|source| io_error("open download", path, source))?;
    let observed_size = file
        .metadata()
        .map_err(|source| io_error("read download metadata", path, source))?
        .len();
    if observed_size != artifact.size() {
        return Err(InstallError::SizeMismatch {
            expected: artifact.size(),
            observed: observed_size,
        });
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        check_cancelled(cancelled)?;
        let count = file
            .read(&mut buffer)
            .map_err(|source| io_error("hash download", path, source))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let observed = hex::encode(hasher.finalize());
    if observed != artifact.sha256() {
        return Err(InstallError::ChecksumMismatch {
            expected: artifact.sha256().to_string(),
            observed,
        });
    }
    Ok(())
}

fn extract_archive(
    path: &Path,
    destination: &Path,
    artifact: &Artifact,
    cancelled: &AtomicBool,
) -> Result<(), InstallError> {
    let file =
        File::open(path).map_err(|source| io_error("open verified archive", path, source))?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = Archive::new(decoder);
    // Raw iteration deliberately rejects PAX and GNU extension headers. The embedded
    // catalog authorizes exact regular-file paths; metadata extensions are not part
    // of layout version 1 and must not bypass member-count/type validation.
    let entries = archive
        .entries()
        .map_err(|source| archive_error(path, source))?
        .raw(true);
    let mut seen_files = HashSet::new();
    let mut seen_directories = HashSet::new();
    let mut expanded = 0u64;

    for (index, entry) in entries.enumerate() {
        check_cancelled(cancelled)?;
        if index >= MAX_ARCHIVE_MEMBERS {
            return Err(InstallError::ArchiveMemberCount {
                limit: MAX_ARCHIVE_MEMBERS,
            });
        }
        let mut entry = entry.map_err(|source| archive_error(path, source))?;
        let member_path = entry
            .path()
            .map_err(|source| archive_error(path, source))?
            .into_owned();
        if !safe_member_path(&member_path) {
            return Err(InstallError::UnsafeArchiveMember { path: member_path });
        }
        let entry_type = entry.header().entry_type();
        if entry_type == EntryType::Directory {
            let directory = member_path
                .to_str()
                .map(|path| path.trim_end_matches('/'))
                .ok_or_else(|| InstallError::UnsafeArchiveMember {
                    path: member_path.clone(),
                })?;
            if entry.size() != 0 || !is_declared_directory(directory, artifact.files()) {
                return Err(InstallError::UndeclaredArchiveMember { path: member_path });
            }
            if !seen_directories.insert(directory.to_string()) {
                return Err(InstallError::DuplicateArchiveMember { path: member_path });
            }
            continue;
        }
        if entry_type != EntryType::Regular {
            return Err(InstallError::ArchiveMemberType {
                path: member_path,
                entry_type: entry_type.as_byte(),
            });
        }
        let member_name =
            member_path
                .to_str()
                .ok_or_else(|| InstallError::UnsafeArchiveMember {
                    path: member_path.clone(),
                })?;
        let declared = artifact
            .files()
            .iter()
            .find(|file| file.path() == member_name)
            .ok_or_else(|| InstallError::UndeclaredArchiveMember {
                path: member_path.clone(),
            })?;
        if !seen_files.insert(member_name.to_string()) {
            return Err(InstallError::DuplicateArchiveMember { path: member_path });
        }
        let size = entry.size();
        if size > MAX_MEMBER_BYTES {
            return Err(InstallError::ArchiveMemberSize {
                path: member_path,
                limit: MAX_MEMBER_BYTES,
            });
        }
        expanded = expanded
            .checked_add(size)
            .filter(|total| *total <= MAX_EXPANDED_BYTES)
            .ok_or(InstallError::ExpandedSize {
                limit: MAX_EXPANDED_BYTES,
            })?;
        write_member(
            &mut entry,
            destination,
            &member_path,
            declared,
            size,
            cancelled,
        )?;
    }
    for declared in artifact.files() {
        if !seen_files.contains(declared.path()) {
            return Err(InstallError::MissingArchiveMember {
                path: PathBuf::from(declared.path()),
            });
        }
    }
    Ok(())
}

fn is_declared_directory(directory: &str, files: &[DeclaredFile]) -> bool {
    !directory.is_empty()
        && files.iter().any(|file| {
            Path::new(file.path())
                .parent()
                .is_some_and(|parent| parent == Path::new(directory))
        })
}

fn write_member<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    root: &Path,
    relative: &Path,
    declared: &DeclaredFile,
    expected: u64,
    cancelled: &AtomicBool,
) -> Result<(), InstallError> {
    let destination = root.join(relative);
    if let Some(parent) = destination.parent() {
        create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options
        .open(&destination)
        .map_err(|source| io_error("create extracted file", &destination, source))?;
    let mut input = entry.take(expected.saturating_add(1));
    let mut observed = 0;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        check_cancelled(cancelled)?;
        let count = input
            .read(&mut buffer)
            .map_err(|source| archive_error(&destination, source))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|source| io_error("write extracted file", &destination, source))?;
        observed += count as u64;
    }
    if observed != expected {
        return Err(InstallError::ArchiveMemberLength {
            path: relative.to_path_buf(),
            expected,
            observed,
        });
    }
    output
        .flush()
        .map_err(|source| io_error("flush extracted file", &destination, source))?;
    output
        .sync_all()
        .map_err(|source| io_error("sync extracted file", &destination, source))?;
    set_file_permissions(&destination, declared.kind())
}

fn write_completion(
    root: &Path,
    version: &str,
    target: HostTarget,
    artifact: &Artifact,
) -> Result<(), InstallError> {
    let completion = Completion {
        schema_version: COMPLETION_SCHEMA_VERSION,
        version: version.to_string(),
        target: target.triple().to_string(),
        archive_sha256: artifact.sha256().to_string(),
        layout_version: artifact.layout_version(),
    };
    let path = root.join(".ok");
    let bytes = serde_json::to_vec_pretty(&completion).map_err(|source| InstallError::Io {
        action: "serialize completion metadata",
        path: path.clone(),
        source: io::Error::new(io::ErrorKind::InvalidData, source),
    })?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|source| io_error("create completion metadata", &path, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write completion metadata", &path, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync completion metadata", &path, source))
}

fn installation_is_complete(
    path: &Path,
    version: &str,
    target: HostTarget,
    artifact: &Artifact,
) -> Result<bool, InstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Ok(false),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(source) => return Err(io_error("inspect installation", path, source)),
    }
    let bytes = match fs::read(path.join(".ok")) {
        Ok(bytes) => bytes,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(source) => return Err(io_error("read completion metadata", path, source)),
    };
    let Ok(completion) = serde_json::from_slice::<Completion>(&bytes) else {
        return Ok(false);
    };
    let expected = Completion {
        schema_version: COMPLETION_SCHEMA_VERSION,
        version: version.to_string(),
        target: target.triple().to_string(),
        archive_sha256: artifact.sha256().to_string(),
        layout_version: artifact.layout_version(),
    };
    if completion != expected {
        return Ok(false);
    }
    for file in artifact.files() {
        let metadata = match fs::symlink_metadata(path.join(file.path())) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(source) => return Err(io_error("inspect installed file", path, source)),
        };
        if !metadata.file_type().is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_installation(path: &Path) -> Result<bool, InstallError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error("inspect incomplete installation", path, source)),
    };
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
            .map_err(|source| io_error("remove managed installation", path, source))?;
    } else {
        fs::remove_file(path)
            .map_err(|source| io_error("remove managed installation", path, source))?;
    }
    Ok(true)
}

// Called only with this target's exclusive lock. Target-specific prefixes avoid
// deleting another target's concurrent download in the same version directory.
fn remove_abandoned_staging(version_dir: &Path, target: HostTarget) -> Result<bool, InstallError> {
    let entries = match fs::read_dir(version_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error("inspect abandoned downloads", version_dir, source)),
    };
    let download_prefix = format!(".download-{}-", target.triple());
    let staging_prefix = format!(".staging-{}-", target.triple());
    let mut removed = false;
    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("inspect abandoned download", version_dir, source))?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| {
            name.starts_with(&download_prefix) || name.starts_with(&staging_prefix)
        }) {
            removed |= remove_installation(&entry.path())?;
        }
    }
    Ok(removed)
}

fn safe_member_path(path: &Path) -> bool {
    path.to_str().is_some_and(|name| {
        crate::catalog::is_safe_relative_path(name.strip_suffix('/').unwrap_or(name))
    })
}

#[cfg(unix)]
fn set_file_permissions(path: &Path, kind: FileKind) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match kind {
        FileKind::Executable => 0o755,
        FileKind::Artifact | FileKind::RuntimeLibrary => 0o644,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| io_error("set extracted file permissions", path, source))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path, _kind: FileKind) -> Result<(), InstallError> {
    Ok(())
}

fn create_dir_all(path: &Path) -> Result<(), InstallError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))
}

fn archive_error(path: &Path, source: io::Error) -> InstallError {
    InstallError::Archive {
        path: path.to_path_buf(),
        source,
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> InstallError {
    InstallError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}
