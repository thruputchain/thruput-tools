use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use toml_edit::{DocumentMut, Item, TableLike};

use crate::CliError;

const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const TEMP_FILE_ATTEMPTS: usize = 16;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigFileKind {
    Global,
    Project,
}

/// Load a TOML document, returning an empty document when `path` does not exist.
pub(crate) fn load_document(path: &Path) -> Result<DocumentMut, CliError> {
    load_document_at(path, path)
}

fn load_document_at(path: &Path, reported_path: &Path) -> Result<DocumentMut, CliError> {
    let contents = read_contents(path)?;
    parse_document(reported_path, &contents)
}

pub(crate) fn insert_preserving_decor(table: &mut dyn TableLike, key: &str, mut item: Item) {
    if let Some(previous) = table.get_mut(key) {
        if let Some(previous_value) = previous.as_value()
            && let Some(next) = item.as_value_mut()
        {
            next.decor_mut().clone_from(previous_value.decor());
        }
        *previous = item;
    } else {
        table.insert(key, item);
    }
}

/// Mutate and validate a fresh TOML document while holding its sidecar lock.
///
/// Returns whether the serialized document changed. An unchanged document is
/// not replaced on disk.
pub(crate) fn transact_document(
    path: &Path,
    kind: ConfigFileKind,
    mutate: impl FnOnce(&mut DocumentMut) -> Result<(), CliError>,
    validate: impl FnOnce(&DocumentMut) -> Result<(), CliError>,
) -> Result<bool, CliError> {
    transact_document_with_timeout(path, kind, LOCK_TIMEOUT, mutate, validate)
}

/// Mutate one document while holding stable locks for it and a read-only peer.
pub(crate) fn transact_document_with_peer(
    path: &Path,
    kind: ConfigFileKind,
    peer_path: &Path,
    mutate: impl FnOnce(&mut DocumentMut, &DocumentMut) -> Result<(), CliError>,
    validate: impl FnOnce(&DocumentMut, &DocumentMut) -> Result<(), CliError>,
) -> Result<bool, CliError> {
    let resolved_path = resolve_config_path(path)?;
    let resolved_peer_path = resolve_config_path(peer_path)?;
    if resolved_path == resolved_peer_path {
        return Err(CliError::ConfigFileInvalid {
            path: path.to_path_buf(),
            message: format!(
                "config and peer `{}` resolve to the same file",
                peer_path.display()
            ),
        });
    }
    let (_first_lock, _second_lock) = ConfigLock::acquire_resolved_pair(
        &resolved_path,
        path,
        &resolved_peer_path,
        peer_path,
        LOCK_TIMEOUT,
    )?;
    ensure_config_path_identity(path, &resolved_path)?;
    ensure_config_path_identity(peer_path, &resolved_peer_path)?;
    #[cfg(unix)]
    {
        reject_hardlinked_config(&resolved_path, path)?;
        reject_hardlinked_config(&resolved_peer_path, peer_path)?;
    }
    let mut document = load_document_at(&resolved_path, path)?;
    let peer = load_document_at(&resolved_peer_path, peer_path)?;
    let original = document.to_string();

    mutate(&mut document, &peer)?;
    validate(&document, &peer)?;

    let updated = document.to_string();
    if updated == original {
        normalize_global_permissions(&resolved_path, kind)?;
        return Ok(false);
    }

    atomic_replace(&resolved_path, updated.as_bytes(), kind)?;
    Ok(true)
}

fn transact_document_with_timeout(
    path: &Path,
    kind: ConfigFileKind,
    timeout: Duration,
    mutate: impl FnOnce(&mut DocumentMut) -> Result<(), CliError>,
    validate: impl FnOnce(&DocumentMut) -> Result<(), CliError>,
) -> Result<bool, CliError> {
    let resolved_path = resolve_config_path(path)?;
    let _lock = ConfigLock::acquire_identity(&resolved_path, path, timeout)?;
    ensure_config_path_identity(path, &resolved_path)?;
    #[cfg(unix)]
    reject_hardlinked_config(&resolved_path, path)?;
    let mut document = load_document_at(&resolved_path, path)?;
    let original = document.to_string();

    mutate(&mut document)?;
    validate(&document)?;

    let updated = document.to_string();
    if updated == original {
        normalize_global_permissions(&resolved_path, kind)?;
        return Ok(false);
    }

    atomic_replace(&resolved_path, updated.as_bytes(), kind)?;
    Ok(true)
}

fn atomic_replace(path: &Path, data: &[u8], kind: ConfigFileKind) -> Result<(), CliError> {
    create_parent_directories(path)?;

    let mut attempts_remaining = TEMP_FILE_ATTEMPTS;
    loop {
        let temporary_path = unique_temp_path(path);
        match atomic_replace_at(path, data, kind, &temporary_path) {
            Err(CliError::Io(error))
                if error.kind() == io::ErrorKind::AlreadyExists && attempts_remaining > 1 =>
            {
                attempts_remaining -= 1;
            }
            result => return result,
        }
    }
}

fn atomic_replace_at(
    path: &Path,
    data: &[u8],
    kind: ConfigFileKind,
    temporary_path: &Path,
) -> Result<(), CliError> {
    let permissions = replacement_permissions(path, kind)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if kind == ConfigFileKind::Global {
        options.mode(0o600);
    }

    // Do not clean up until this invocation has successfully created the file.
    let mut file = options.open(temporary_path)?;
    let result = (|| -> Result<(), CliError> {
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    let result = result.and_then(|()| {
        std::fs::rename(temporary_path, path)?;
        sync_parent_directory(path)?;
        Ok(())
    });

    if result.is_err() {
        let _ = std::fs::remove_file(temporary_path);
    }
    result
}

#[cfg(unix)]
fn normalize_global_permissions(path: &Path, kind: ConfigFileKind) -> Result<(), CliError> {
    if kind != ConfigFileKind::Global {
        return Ok(());
    }

    match std::fs::set_permissions(path, Permissions::from_mode(0o600)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn normalize_global_permissions(_path: &Path, _kind: ConfigFileKind) -> Result<(), CliError> {
    Ok(())
}

fn replacement_permissions(
    path: &Path,
    kind: ConfigFileKind,
) -> Result<Option<Permissions>, CliError> {
    #[cfg(unix)]
    if kind == ConfigFileKind::Global {
        return Ok(Some(Permissions::from_mode(0o600)));
    }

    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.permissions())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_contents(path: &Path) -> Result<String, CliError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn parse_document(path: &Path, contents: &str) -> Result<DocumentMut, CliError> {
    contents.parse().map_err(|error: toml_edit::TomlError| {
        let (line, column) = error
            .span()
            .map(|span| super::line_column(contents, span.start))
            .map_or((None, None), |(line, column)| (Some(line), Some(column)));
        CliError::ConfigParse {
            path: path.to_path_buf(),
            message: error.message().to_owned(),
            line,
            column,
        }
    })
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("tmp");
    path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        counter
    ))
}

struct ConfigLock {
    _file: File,
}

impl ConfigLock {
    #[cfg(test)]
    fn acquire(config_path: &Path, timeout: Duration) -> Result<Self, CliError> {
        let identity = resolve_config_path(config_path)?;
        Self::acquire_identity(&identity, config_path, timeout)
    }

    fn acquire_identity(
        identity: &Path,
        reported_path: &Path,
        timeout: Duration,
    ) -> Result<Self, CliError> {
        let lock_path = lock_path(identity);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&lock_path)?;
        let started = Instant::now();

        loop {
            match try_lock(&file) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                        return Err(lock_timeout(reported_path));
                    };
                    if remaining.is_zero() {
                        return Err(lock_timeout(reported_path));
                    }
                    std::thread::sleep(remaining.min(LOCK_RETRY_INTERVAL));
                }
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted
                        && started.elapsed() < timeout => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Err(lock_timeout(reported_path));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    #[cfg(test)]
    fn acquire_pair(
        first_path: &Path,
        second_path: &Path,
        timeout: Duration,
    ) -> Result<(Self, Option<Self>), CliError> {
        let first_identity = resolve_config_path(first_path)?;
        let second_identity = resolve_config_path(second_path)?;
        Self::acquire_resolved_pair(
            &first_identity,
            first_path,
            &second_identity,
            second_path,
            timeout,
        )
    }

    fn acquire_resolved_pair(
        first_identity: &Path,
        first_reported: &Path,
        second_identity: &Path,
        second_reported: &Path,
        timeout: Duration,
    ) -> Result<(Self, Option<Self>), CliError> {
        if first_identity == second_identity {
            return Self::acquire_identity(first_identity, first_reported, timeout)
                .map(|lock| (lock, None));
        }
        let (first_identity, first_reported, second_identity, second_reported) =
            if first_identity <= second_identity {
                (
                    first_identity,
                    first_reported,
                    second_identity,
                    second_reported,
                )
            } else {
                (
                    second_identity,
                    second_reported,
                    first_identity,
                    first_reported,
                )
            };
        let started = Instant::now();
        let first = Self::acquire_identity(first_identity, first_reported, timeout)?;
        let remaining = timeout.saturating_sub(started.elapsed());
        let second = Self::acquire_identity(second_identity, second_reported, remaining)?;
        Ok((first, Some(second)))
    }
}

fn resolve_config_path(config_path: &Path) -> Result<PathBuf, CliError> {
    create_parent_directories(config_path)?;
    match std::fs::canonicalize(config_path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(config_path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(CliError::ConfigFileInvalid {
                    path: config_path.to_path_buf(),
                    message: "config path is a dangling symbolic link".into(),
                });
            }
            let Some(parent) = config_path.parent() else {
                return Ok(config_path.to_path_buf());
            };
            let parent = std::fs::canonicalize(parent)?;
            let Some(file_name) = config_path.file_name() else {
                return Ok(parent);
            };
            Ok(parent.join(file_name))
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_config_path_identity(config_path: &Path, expected: &Path) -> Result<(), CliError> {
    if resolve_config_path(config_path)? == expected {
        return Ok(());
    }

    Err(CliError::ConfigFileInvalid {
        path: config_path.to_path_buf(),
        message: "config path changed while acquiring its lock; retry the command".into(),
    })
}

#[cfg(unix)]
fn reject_hardlinked_config(path: &Path, reported_path: &Path) -> Result<(), CliError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.nlink() > 1 => {
            Err(CliError::ConfigFileInvalid {
                path: reported_path.to_path_buf(),
                message: "config files with multiple hard links are not supported".into(),
            })
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn create_parent_directories(path: &Path) -> Result<(), CliError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut missing = Vec::new();
    let mut candidate = parent;
    while !candidate.exists() {
        missing.push(candidate.to_path_buf());
        let Some(next) = candidate.parent() else {
            break;
        };
        candidate = next;
    }
    std::fs::create_dir_all(parent)?;
    for directory in missing.iter().rev() {
        sync_parent_directory(directory)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

fn lock_path(config_path: &Path) -> PathBuf {
    let mut path = config_path.as_os_str().to_os_string();
    path.push(".lock");
    path.into()
}

fn lock_timeout(config_path: &Path) -> CliError {
    CliError::ConfigLockTimeout {
        path: config_path.to_path_buf(),
    }
}

#[cfg(unix)]
fn try_lock(file: &File) -> Result<(), io::Error> {
    // SAFETY: `file` owns a valid descriptor for the duration of this call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) || error.raw_os_error() == Some(libc::EAGAIN)
    {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Err(error)
}

#[cfg(not(unix))]
fn try_lock(_file: &File) -> Result<(), io::Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "config locking is supported only on Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[test]
    fn load_document_preserves_comments_and_unknown_fields() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let contents = "# retained\nunknown = \"value\"\n";
        std::fs::write(&path, contents).expect("fixture should be written");

        let document = load_document(&path).expect("document should load");

        assert_eq!(document.to_string(), contents);
    }

    #[test]
    fn transaction_mutates_and_validates_fresh_document() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "# retained\nvalue = 1\nunknown = true\n")
            .expect("fixture should be written");

        let changed = transact_document(
            &path,
            ConfigFileKind::Global,
            |document| {
                document["value"] = toml_edit::value(2);
                Ok(())
            },
            |document| {
                if document["value"].as_integer() == Some(2) {
                    Ok(())
                } else {
                    Err(CliError::Other("value was not updated".into()))
                }
            },
        )
        .expect("transaction should succeed");

        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(path).expect("document should remain readable"),
            "# retained\nvalue = 2\nunknown = true\n"
        );
    }

    #[test]
    fn transaction_times_out_when_sidecar_is_locked() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let _lock = ConfigLock::acquire(&path, Duration::ZERO).expect("lock should be acquired");

        let started = Instant::now();
        let error = transact_document_with_timeout(
            &path,
            ConfigFileKind::Global,
            Duration::from_millis(30),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect_err("contending transaction should time out");

        assert!(
            error.code() == "config_lock_timeout"
                && error.to_string().contains("is busy")
                && started.elapsed() < Duration::from_secs(1),
            "unexpected contention error: {error}"
        );
        assert_eq!(
            error.details_json().expect("details should be present")["path"],
            path.display().to_string()
        );
    }

    #[test]
    #[cfg(unix)]
    fn lock_identity_resolves_directory_symlink_aliases() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let directory = temporary.path().join("config");
        let alias = temporary.path().join("alias");
        std::fs::create_dir(&directory).expect("config directory should be created");
        symlink(&directory, &alias).expect("directory symlink should be created");
        let direct = directory.join("config.toml");
        let through_alias = alias.join("config.toml");

        assert_eq!(
            resolve_config_path(&direct).expect("direct identity should resolve"),
            resolve_config_path(&through_alias).expect("alias identity should resolve")
        );
    }

    #[test]
    #[cfg(unix)]
    fn transaction_updates_file_symlink_target_without_replacing_link() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let target = temporary.path().join("managed.toml");
        let link = temporary.path().join("config.toml");
        std::fs::write(&target, "value = 1\n").expect("target config should be written");
        symlink(&target, &link).expect("config symlink should be created");

        let changed = transact_document(
            &link,
            ConfigFileKind::Project,
            |document| {
                document["value"] = toml_edit::value(2);
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("symlinked config transaction should succeed");

        assert!(changed);
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("config link metadata should be readable")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(target).expect("target config should remain readable"),
            "value = 2\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn changed_symlink_identity_is_rejected() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let first_target = temporary.path().join("first.toml");
        let second_target = temporary.path().join("second.toml");
        let link = temporary.path().join("config.toml");
        std::fs::write(&first_target, "value = 1\n").expect("first config should be written");
        std::fs::write(&second_target, "value = 2\n").expect("second config should be written");
        symlink(&first_target, &link).expect("config symlink should be created");
        let expected = resolve_config_path(&link).expect("initial identity should resolve");
        std::fs::remove_file(&link).expect("initial symlink should be removed");
        symlink(&second_target, &link).expect("config symlink should be retargeted");

        let error = ensure_config_path_identity(&link, &expected)
            .expect_err("changed config identity should fail");

        assert_eq!(error.code(), "config_file_invalid");
        assert!(error.to_string().contains("changed while acquiring"));
    }

    #[test]
    #[cfg(unix)]
    fn dangling_config_symlink_reports_logical_path() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let link = temporary.path().join("config.toml");
        symlink(temporary.path().join("missing.toml"), &link)
            .expect("dangling config symlink should be created");

        let error = resolve_config_path(&link).expect_err("dangling config symlink should fail");

        assert_eq!(error.code(), "config_file_invalid");
        assert_eq!(
            error.details_json().expect("details should be present")["path"],
            link.display().to_string()
        );
    }

    #[test]
    #[cfg(unix)]
    fn peer_transaction_rejects_layers_resolving_to_same_file() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let global = temporary.path().join("global.toml");
        let project = temporary.path().join("project.toml");
        std::fs::write(&global, "value = 1\n").expect("global config should be written");
        symlink(&global, &project).expect("project config symlink should be created");

        let error = transact_document_with_peer(
            &project,
            ConfigFileKind::Project,
            &global,
            |document, _| {
                document["value"] = toml_edit::value(2);
                Ok(())
            },
            |_, _| Ok(()),
        )
        .expect_err("aliased config layers should fail");

        assert_eq!(error.code(), "config_file_invalid");
        assert!(error.to_string().contains("resolve to the same file"));
        assert_eq!(
            std::fs::read_to_string(global).expect("global config should remain readable"),
            "value = 1\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn transaction_rejects_hardlinked_config_without_changing_aliases() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let alias = temporary.path().join("alias.toml");
        std::fs::write(&path, "value = 1\n").expect("config should be written");
        std::fs::hard_link(&path, &alias).expect("hard link should be created");

        let error = transact_document(
            &path,
            ConfigFileKind::Project,
            |document| {
                document["value"] = toml_edit::value(2);
                Ok(())
            },
            |_| Ok(()),
        )
        .expect_err("hardlinked config transaction should fail");

        assert_eq!(error.code(), "config_file_invalid");
        assert!(error.to_string().contains("multiple hard links"));
        assert_eq!(
            std::fs::read_to_string(path).expect("config should remain readable"),
            "value = 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(alias).expect("alias should remain readable"),
            "value = 1\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn lock_pair_acquires_canonical_alias_once() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let alias = temporary.path().join("alias.toml");
        std::fs::write(&path, "value = true\n").expect("config should be written");
        symlink(&path, &alias).expect("config symlink should be created");

        let (_lock, duplicate) = ConfigLock::acquire_pair(&path, &alias, Duration::ZERO)
            .expect("canonical aliases should share one lock");

        assert!(duplicate.is_none());
    }

    #[test]
    fn atomic_write_removes_temporary_file_after_rename_failure() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("destination");
        std::fs::create_dir(&path).expect("destination directory should be created");

        atomic_replace(&path, b"value", ConfigFileKind::Project)
            .expect_err("replacing a directory should fail");

        let entries = std::fs::read_dir(temporary.path())
            .expect("tempdir should be readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("directory entries should be readable");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn atomic_write_does_not_remove_unowned_temporary_file() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let temporary_path = temporary.path().join("existing.tmp");
        std::fs::write(&temporary_path, "owned elsewhere")
            .expect("colliding temporary file should be written");

        atomic_replace_at(
            &path,
            b"value = 1\n",
            ConfigFileKind::Project,
            &temporary_path,
        )
        .expect_err("temporary file collision should fail");

        assert_eq!(
            std::fs::read_to_string(temporary_path)
                .expect("unowned temporary file should remain readable"),
            "owned elsewhere"
        );
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_preserves_existing_project_permissions() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("thruput.toml");
        std::fs::write(&path, "value = \"old\"\n").expect("fixture should be written");
        std::fs::set_permissions(&path, Permissions::from_mode(0o640))
            .expect("fixture permissions should be set");

        atomic_replace(&path, b"value = \"new\"\n", ConfigFileKind::Project)
            .expect("document should be replaced");

        assert_eq!(
            std::fs::metadata(path)
                .expect("document metadata should be readable")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_private_normalizes_existing_permissions() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "value = \"old\"\n").expect("fixture should be written");
        std::fs::set_permissions(&path, Permissions::from_mode(0o644))
            .expect("fixture permissions should be set");

        atomic_replace(&path, b"value = \"new\"\n", ConfigFileKind::Global)
            .expect("document should be replaced");

        assert_eq!(
            std::fs::metadata(path)
                .expect("document metadata should be readable")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_private_write_normalizes_existing_permissions() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let contents = b"value = \"same\"\n";
        std::fs::write(&path, contents).expect("fixture should be written");
        std::fs::set_permissions(&path, Permissions::from_mode(0o644))
            .expect("fixture permissions should be set");

        transact_document(&path, ConfigFileKind::Global, |_| Ok(()), |_| Ok(()))
            .expect("unchanged write should succeed");

        assert_eq!(
            std::fs::metadata(path)
                .expect("document metadata should be readable")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_transaction_does_not_replace_document() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "value = 1\n").expect("fixture should be written");
        let inode = std::fs::metadata(&path)
            .expect("document metadata should be readable")
            .ino();

        let changed = transact_document(&path, ConfigFileKind::Global, |_| Ok(()), |_| Ok(()))
            .expect("transaction should succeed");

        assert!(!changed);
        assert_eq!(
            std::fs::metadata(path)
                .expect("document metadata should be readable")
                .ino(),
            inode
        );
    }

    #[test]
    fn validation_failure_leaves_document_unchanged() {
        let temporary = tempfile::TempDir::new().expect("tempdir should be created");
        let path = temporary.path().join("config.toml");
        let contents = "value = 1\n";
        std::fs::write(&path, contents).expect("fixture should be written");

        transact_document(
            &path,
            ConfigFileKind::Project,
            |document| {
                document["value"] = toml_edit::value(2);
                Ok(())
            },
            |_| Err(CliError::Other("invalid document".into())),
        )
        .expect_err("validation should fail");

        assert_eq!(
            std::fs::read_to_string(path).expect("document should remain readable"),
            contents
        );
    }
}
