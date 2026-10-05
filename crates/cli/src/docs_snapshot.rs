//! The selected developer-site artifact, materialized in the user's cache.

use crate::{
    CliError,
    docs::{DocsError, Snapshot, read_snapshot, regular_files, validate_markdown_manifest},
};
use flate2::read::GzDecoder;
use fs2::FileExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const ARCHIVE: &[u8] = include_bytes!("../assets/docs/snapshot.tar.gz");
const LOCK: &str = include_str!("../assets/docs/snapshot-lock.json");
const MAX_FILES: usize = 4096;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TAR_BYTES: u64 = MAX_BYTES + (MAX_FILES as u64 * 1024) + 10240;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pin {
    #[serde(flatten)]
    snapshot: Snapshot,
    archive: String,
    archive_bytes: usize,
    sha256: String,
}

type Files = Vec<(PathBuf, Vec<u8>)>;

fn invalid(message: &str) -> DocsError {
    DocsError::Bundle(message.into())
}

fn decode(archive: &[u8], pin: &Pin) -> Result<Files, CliError> {
    if pin.archive != "snapshot.tar.gz"
        || pin.snapshot.source_dirty
        || pin.archive_bytes != archive.len()
        || pin.sha256 != hex::encode(Sha256::digest(archive))
    {
        return Err(invalid("bundled archive differs from its clean-source pin").into());
    }
    // Bound expansion before tar parses metadata. The small snapshot is decoded
    // on each invocation so cache validation compares against trusted bytes,
    // rather than trusting a mutable completion marker beside cached files.
    let mut expanded = Vec::new();
    GzDecoder::new(archive)
        .take(MAX_TAR_BYTES + 1)
        .read_to_end(&mut expanded)?;
    if expanded.len() as u64 > MAX_TAR_BYTES {
        return Err(invalid("snapshot exceeds expanded-size limit").into());
    }
    let mut tar = tar::Archive::new(expanded.as_slice());
    let mut files = Vec::new();
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    for entry in tar.entries()?.raw(true) {
        let mut entry = entry?;
        let name = entry
            .path()?
            .to_str()
            .ok_or_else(|| invalid("non-UTF-8 archive path"))?
            .to_owned();
        if !entry.header().entry_type().is_file()
            || name
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.'))
            || name.contains(['\\', ':'])
            || name.chars().any(char::is_control)
            || !paths.insert(name.clone())
            || paths.len() > MAX_FILES
        {
            return Err(invalid("unsupported or duplicate archive entry").into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid("snapshot size overflow"))?;
        if total > MAX_BYTES {
            return Err(invalid("snapshot exceeds content-size limit").into());
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        files.push((PathBuf::from(name), data));
    }
    let manifest = files
        .iter()
        .find(|(path, _)| path == Path::new("snapshot.json"))
        .ok_or_else(|| invalid("archive has no snapshot manifest"))?;
    let snapshot: Snapshot = serde_json::from_slice(&manifest.1)
        .map_err(|_| invalid("archive snapshot manifest is invalid"))?;
    if snapshot != pin.snapshot
        || snapshot.schema_version != 1
        || snapshot.source_revision.len() != 40
        || !snapshot
            .source_revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || snapshot.agent_guide_cli_version.is_empty()
        || !paths.contains("index.html")
        || !paths.contains("start/index.html")
        || !paths.contains("agent-guide.md")
        || !paths.contains("markdown/index.md")
        || !paths.contains("markdown/full.md")
        || !paths.contains("llms.txt")
        || !paths.contains("llms-full.txt")
    {
        return Err(invalid("archive identity or entry points differ from its pin").into());
    }
    let markdown = files
        .iter()
        .find(|(path, _)| path == Path::new("markdown/manifest.json"))
        .ok_or_else(|| invalid("archive has no Markdown manifest"))?;
    validate_markdown_manifest(&markdown.1, &snapshot)
        .map_err(|error| invalid(&error.to_string()))?;
    Ok(files)
}

fn matches_cache(root: &Path, files: &Files) -> bool {
    if !fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        return false;
    }
    let Ok(actual) = regular_files(root) else {
        return false;
    };
    actual.len() == files.len()
        && files.iter().all(|(path, expected)| {
            let path = root.join(path);
            fs::metadata(&path).is_ok_and(|metadata| metadata.len() == expected.len() as u64)
                && fs::read(path).is_ok_and(|data| data == *expected)
        })
}

fn install(cache: &Path, pin: &Pin, files: &Files) -> Result<PathBuf, CliError> {
    fs::create_dir_all(cache)?;
    let root = cache.join(&pin.sha256);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join(format!("{}.lock", pin.sha256)))?;
    lock.lock_exclusive()?;
    if matches_cache(&root, files) {
        return Ok(root);
    }
    let stage = tempfile::Builder::new()
        .prefix(".docs-")
        .tempdir_in(cache)?;
    for (path, bytes) in files {
        let target = stage.path().join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, bytes)?;
    }
    read_snapshot(stage.path())?;
    // Only this content-addressed cache entry is replaceable. Never follow a
    // symlink when removing a damaged entry, and never touch --dir snapshots.
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&root)?,
        Ok(_) => fs::remove_file(&root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::rename(stage.path(), &root)?;
    Ok(root)
}

pub(crate) fn bundled() -> Result<PathBuf, CliError> {
    let pin: Pin =
        serde_json::from_str(LOCK).map_err(|_| invalid("invalid bundled snapshot pin"))?;
    let files = decode(ARCHIVE, &pin)?;
    let cache = crate::config::cache_dir()?.join("docs");
    install(&cache, &pin, &files).map_err(|error| match error {
        CliError::Io(source) => DocsError::Cache {
            path: cache,
            source,
        }
        .into(),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_snapshot_is_pinned_and_cache_recovers_from_damage() {
        let pin: Pin = serde_json::from_str(LOCK).unwrap();
        let files = decode(ARCHIVE, &pin).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let first = install(cache.path(), &pin, &files).unwrap();
        let modified = fs::metadata(first.join("index.html"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(install(cache.path(), &pin, &files).unwrap(), first);
        assert_eq!(
            fs::metadata(first.join("index.html"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        fs::write(first.join("index.html"), "broken").unwrap();
        install(cache.path(), &pin, &files).unwrap();
        assert!(matches_cache(&first, &files));
        fs::write(first.join("extra.txt"), "not part of the snapshot").unwrap();
        install(cache.path(), &pin, &files).unwrap();
        assert!(!first.join("extra.txt").exists());
    }

    #[test]
    fn rejects_modified_archive() {
        let pin = serde_json::from_str(LOCK).unwrap();
        let mut bytes = ARCHIVE.to_vec();
        bytes[10] ^= 1;
        assert!(decode(&bytes, &pin).is_err());
    }

    #[test]
    fn rejects_markdown_from_another_revision_even_when_archive_checksum_matches() {
        let mut pin: Pin = serde_json::from_str(LOCK).unwrap();
        let snapshot = serde_json::to_string(&pin.snapshot).unwrap();
        let mut markdown_snapshot = pin.snapshot.clone();
        markdown_snapshot.source_revision = "b".repeat(40);
        let markdown = serde_json::to_string(&serde_json::json!({
            "schemaVersion": 1, "snapshot": markdown_snapshot,
        }))
        .unwrap();
        let mut tar = tar::Builder::new(Vec::new());
        for (name, content) in [
            ("index.html", ""),
            ("start/index.html", ""),
            ("agent-guide.md", ""),
            ("markdown/index.md", ""),
            ("markdown/full.md", ""),
            ("llms.txt", ""),
            ("llms-full.txt", ""),
            ("snapshot.json", snapshot.as_str()),
            ("markdown/manifest.json", markdown.as_str()),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gzip, &tar.into_inner().unwrap()).unwrap();
        let bytes = gzip.finish().unwrap();
        pin.archive_bytes = bytes.len();
        pin.sha256 = hex::encode(Sha256::digest(&bytes));
        assert!(
            decode(&bytes, &pin)
                .unwrap_err()
                .to_string()
                .contains("Markdown export identity")
        );
    }

    fn entry_archive(name: &str, kind: tar::EntryType) -> (Vec<u8>, Pin) {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_ustar();
        // Construct raw paths so the test is not relying on Builder's checks.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(0);
        header.set_mode(0o644);
        header.set_entry_type(kind);
        header.set_cksum();
        tar.append(&header, std::io::empty()).unwrap();
        let expanded = tar.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gzip, &expanded).unwrap();
        let bytes = gzip.finish().unwrap();
        let mut pin: Pin = serde_json::from_str(LOCK).unwrap();
        pin.archive_bytes = bytes.len();
        pin.sha256 = hex::encode(Sha256::digest(&bytes));
        (bytes, pin)
    }

    #[test]
    fn rejects_archive_paths_and_links_before_extraction() {
        for name in [
            "../escape",
            "/absolute",
            ".env",
            "a//b",
            "a\\b",
            "C:/escape",
        ] {
            let (bytes, pin) = entry_archive(name, tar::EntryType::Regular);
            let error = decode(&bytes, &pin).unwrap_err().to_string();
            assert!(error.contains("unsupported"), "{name}: {error}");
        }
        let (bytes, pin) = entry_archive("link", tar::EntryType::Symlink);
        assert!(
            decode(&bytes, &pin)
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_symlink_cache_does_not_touch_its_target() {
        let pin: Pin = serde_json::from_str(LOCK).unwrap();
        let files = decode(ARCHIVE, &pin).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("keep.txt");
        fs::write(&sentinel, "keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), cache.path().join(&pin.sha256)).unwrap();
        let root = install(cache.path(), &pin, &files).unwrap();
        assert!(matches_cache(&root, &files));
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
    }

    #[test]
    fn concurrent_installers_share_one_complete_snapshot() {
        let cache = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..3 {
                let path = cache.path();
                scope.spawn(move || {
                    let pin = serde_json::from_str(LOCK).unwrap();
                    let files = decode(ARCHIVE, &pin).unwrap();
                    let root = install(path, &pin, &files).unwrap();
                    assert!(matches_cache(&root, &files));
                });
            }
        });
    }
}
