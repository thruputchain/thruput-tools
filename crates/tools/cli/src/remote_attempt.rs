//! On-disk records of in-flight remote faucet requests.
//!
//! A hosted faucet deduplicates requests by client nonce, so a command that
//! is retried after a crash or a lost response must present the nonce it
//! used the first time. Each command keeps that state in a JSON file under
//! the cache directory, named by a digest of the request's identity, and
//! holds an exclusive lock beside it while the request is in flight.

use std::fs::{File, OpenOptions};
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
        options.mode(0o600);
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
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
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
    std::fs::write(path, contents)?;
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
