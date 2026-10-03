//! On-disk records of in-flight remote faucet requests.
//!
//! A retry must reuse the original client nonce, so each command records it
//! in a JSON file under the cache directory with a lock beside it.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{CliError, config::cache_dir};

/// The attempt file for the request whose identity is `parts`, under the
/// cache subdirectory `dir`.
pub(crate) fn attempt_path(dir: &str, parts: &[&str]) -> Result<PathBuf, CliError> {
    let hash: [u8; 32] = Sha256::digest(parts.join("\0")).into();
    Ok(cache_dir()?
        .join(dir)
        .join(format!("{}.json", hex::encode(hash))))
}

/// Take the exclusive lock beside `attempt_path` for the life of the
/// returned file.
///
/// `command` and `scope` only phrase the error when another process holds
/// the lock, for example `airdrop` and `profile, recipient, and network`.
pub(crate) fn lock_attempt(
    attempt_path: &Path,
    command: &str,
    scope: &str,
) -> Result<File, CliError> {
    let parent = attempt_path
        .parent()
        .ok_or_else(|| CliError::Other(format!("{command} attempt path has no parent")))?;
    std::fs::create_dir_all(parent)?;
    let lock_path = attempt_path.with_extension("lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&lock_path)?;
    file.try_lock_exclusive().map_err(|error| {
        let contended = fs2::lock_contended_error();
        if error.kind() == std::io::ErrorKind::WouldBlock
            || error.raw_os_error() == contended.raw_os_error()
        {
            CliError::Other(format!(
                "another {command} command is already running for this {scope} ({})",
                lock_path.display()
            ))
        } else {
            error.into()
        }
    })?;
    Ok(file)
}

/// Read the attempt at `path`, or `None` when no attempt was recorded.
pub(crate) fn read_attempt<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, CliError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(CliError::LocalFundingAttempt(
            "attempt must be a regular file".into(),
        ));
    }
    let mut contents = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut contents)?;
    if contents.len() > 64 * 1024 {
        return Err(CliError::LocalFundingAttempt(
            "attempt exceeds 64 KiB".into(),
        ));
    }
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| CliError::Other(format!("failed to parse `{}`: {error}", path.display())))
}

/// Record `attempt` at `path`, creating its directory as needed.
pub(crate) fn write_attempt<T: Serialize>(path: &Path, attempt: &T) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents =
        serde_json::to_vec_pretty(attempt).map_err(|error| CliError::Other(error.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| CliError::LocalFundingAttempt("attempt path has no parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| CliError::Io(error.error))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Delete the attempt at `path`; an attempt that is already gone is fine.
pub(crate) fn remove_attempt(path: &Path) -> Result<(), CliError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
