//! On-disk records of in-flight remote faucet requests.
//!
//! A retry must reuse the original client nonce, so each command records it
//! in a JSON file under the cache directory with a lock beside it.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

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

/// How long to retry a contended attempt lock before reporting another
/// command. A child spawned concurrently by another thread briefly co-owns
/// this process's descriptors (and their `flock`s) between fork and exec, so
/// an instantly contended lock does not yet prove another command holds it.
const LOCK_RETRY_WINDOW: Duration = Duration::from_millis(500);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

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
    let deadline = Instant::now() + LOCK_RETRY_WINDOW;
    loop {
        let Err(error) = file.try_lock_exclusive() else {
            return Ok(file);
        };
        let contended = error.kind() == std::io::ErrorKind::WouldBlock
            || error.raw_os_error() == fs2::lock_contended_error().raw_os_error();
        if !contended {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Err(CliError::Other(format!(
                "another {command} command is already running for this {scope} ({})",
                lock_path.display()
            )));
        }
        thread::sleep(LOCK_RETRY_INTERVAL);
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io;

    /// A child spawned from another thread and held between fork and exec,
    /// where it co-owns every descriptor this process had open at the fork.
    struct PausedChild {
        release: io::PipeWriter,
        spawner: thread::JoinHandle<std::process::ExitStatus>,
    }

    impl PausedChild {
        fn spawn() -> Self {
            use std::os::{fd::AsRawFd, unix::process::CommandExt};

            let (mut forked, forked_writer) = io::pipe().unwrap();
            let (release_reader, release) = io::pipe().unwrap();
            let (forked_fd, release_fd) = (forked_writer.as_raw_fd(), release_reader.as_raw_fd());
            let spawner = thread::spawn(move || {
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
        fn release_after(mut self, delay: Duration) -> thread::JoinHandle<()> {
            thread::spawn(move || {
                thread::sleep(delay);
                self.release.write_all(&[0]).unwrap();
                assert!(self.spawner.join().unwrap().success());
            })
        }
    }

    // A child spawned by another thread keeps the released lock's descriptor
    // until it execs, so the next command must wait out that window rather
    // than report another command.
    #[test]
    fn lock_handoff_waits_out_a_concurrent_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let attempt = dir.path().join("attempt.json");
        let first = lock_attempt(&attempt, "airdrop", "recipient").unwrap();
        let child = PausedChild::spawn();
        drop(first);
        let released = child.release_after(Duration::from_millis(50));
        lock_attempt(&attempt, "airdrop", "recipient").unwrap();
        released.join().unwrap();
    }

    #[test]
    fn lock_held_by_another_owner_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let attempt = dir.path().join("attempt.json");
        let _held = lock_attempt(&attempt, "airdrop", "recipient").unwrap();
        let error = lock_attempt(&attempt, "airdrop", "recipient").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("another airdrop command is already running for this recipient"),
            "{error}"
        );
    }
}
