use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::CliError;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `data` to `path` atomically by writing to a temporary file in the same
/// directory, then renaming it into place.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = unique_temp_path(path);
    std::fs::write(&tmp, data)?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

pub(crate) fn atomic_write_private(path: &Path, data: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = unique_temp_path(path);
    let result = (|| -> Result<(), CliError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);

        let mut file = options.open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn unique_temp_path(path: &Path) -> std::path::PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("tmp");
    path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        counter
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_private_does_not_use_shared_temp_path() {
        let tmp = tempfile::TempDir::new().expect("tempdir should be created");
        let path = tmp.path().join("config.toml");

        atomic_write_private(&path, b"default_key = \"alice\"\n").unwrap();
        atomic_write_private(&path, b"default_key = \"bob\"\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "default_key = \"bob\"\n"
        );
        assert!(!path.with_extension("toml.tmp").exists());
    }
}
