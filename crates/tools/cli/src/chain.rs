//! Offline backup and restore for persistent local-chain state.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use groundhog_build_info::BuildVersion;
use schemars::JsonSchema;
use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::result::CommandResult;
use crate::{CliError, Print};

const ARCHIVE_SCHEMA_VERSION: u32 = 2;
const LEGACY_ARCHIVE_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const ARCHIVE_KIND: &str = "groundhog-chain-dump";
// Files are compressed independently in bounded chunks so restore never has to
// hold an entire database file in memory.
const CHUNK_SIZE: usize = 1024 * 1024;
const COMPRESSION_LEVEL: i32 = 3;
const DATA_DIR_METADATA: &str = "hog-devnode-data.json";
const LOCK_FILE: &str = ".hog-devnode.lock";
const NODE_PID_FILE: &str = "hog-devnode.pid";
// Runtime state is deliberately excluded: endpoints, PIDs, and other process
// metadata must be recreated when the restored node starts.
const PERSISTENT_DIRS: [&str; 3] = ["ramdb", "events", "indexer"];

/// Stable machine-readable result for `groundhog chain dump`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainDumpReport {
    pub schema_version: u32,
    pub archive_path: PathBuf,
    pub data_dir: PathBuf,
    pub file_count: usize,
    pub uncompressed_bytes: String,
    pub archive_bytes: String,
    pub archive_sha256: String,
}

/// Stable machine-readable result for `groundhog chain restore`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainRestoreReport {
    pub schema_version: u32,
    pub archive_path: PathBuf,
    pub data_dir: PathBuf,
    pub file_count: usize,
    pub uncompressed_bytes: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_warning: Option<String>,
}

#[derive(Debug, Default)]
struct ArchiveStats {
    file_count: usize,
    uncompressed_bytes: u64,
    // Tracks both duplicate entries and the required persistent-state entries.
    paths: HashSet<String>,
}

/// Create a compressed JSON archive of a stopped persistent local node.
pub fn dump(
    data_dir: &Path,
    output: &Path,
    overwrite: bool,
    command: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    require_unix()?;
    let data_dir = require_existing_data_dir(data_dir)?;
    let output = absolute_path(output)?;
    if output.starts_with(&data_dir) {
        return Err(CliError::ChainArchiveInsideDataDir(
            output.display().to_string(),
        ));
    }
    // Holding the node's locks gives every archived file a single stopped-node
    // snapshot rather than a mixture of database states.
    let _lock = DataDirLock::acquire(&data_dir)?;

    if output.exists() && !overwrite {
        return Err(CliError::ChainDumpExists(output.display().to_string()));
    }
    let files = collect_archive_files(&data_dir)?;
    let output_parent = output
        .parent()
        .ok_or_else(|| archive_io(format!("output path '{}' has no parent", output.display())))?;
    fs::create_dir_all(output_parent)
        .map_err(|error| archive_io(format!("create '{}': {error}", output_parent.display())))?;
    ensure_outside_data_dir(&data_dir, &output)?;

    let task = print.start_task(format!("Dumping chain state from {}", data_dir.display()));
    // Build and fsync beside the destination, then rename into place. This
    // avoids exposing a partial archive and keeps the rename on one filesystem.
    let mut temp = tempfile::Builder::new()
        .prefix(".groundhog-chain-dump-")
        .tempfile_in(output_parent)
        .map_err(|error| archive_io(format!("create temporary archive: {error}")))?;
    let total_bytes = write_archive(
        temp.as_file_mut(),
        &files,
        groundhog_build_info::build_version!(),
    )?;
    temp.as_file().sync_all()?;

    if overwrite {
        temp.persist(&output).map_err(|error| {
            archive_io(format!(
                "install archive '{}': {}",
                output.display(),
                error.error
            ))
        })?;
    } else {
        temp.persist_noclobber(&output).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                CliError::ChainDumpExists(output.display().to_string())
            } else {
                archive_io(format!(
                    "install archive '{}': {}",
                    output.display(),
                    error.error
                ))
            }
        })?;
    }
    sync_directory(output_parent)?;
    let archive_bytes = fs::metadata(&output)?.len();
    let archive_sha256 = hash_file(&output)?;
    task.success("Chain dump complete");

    let report = ChainDumpReport {
        schema_version: REPORT_SCHEMA_VERSION,
        archive_path: output,
        data_dir,
        file_count: files.len(),
        uncompressed_bytes: total_bytes.to_string(),
        archive_bytes: archive_bytes.to_string(),
        archive_sha256,
    };
    if print.is_json() {
        print.json(&CommandResult::new(command, &report))?;
    } else {
        print.detail("Archive", report.archive_path.display());
        print.detail("Files", report.file_count);
        print.detail("Uncompressed bytes", &report.uncompressed_bytes);
        print.detail("SHA-256", &report.archive_sha256);
    }
    Ok(())
}

// Streams the archive envelope and file records to the destination.
fn write_archive(
    file: &mut File,
    files: &[(String, PathBuf)],
    build_version: BuildVersion,
) -> Result<u64, CliError> {
    let mut writer = BufWriter::new(file);
    // Write the envelope directly instead of materializing its base64 payloads
    // in a serde_json::Value; large chain dumps remain memory-bounded.
    write!(
        writer,
        "{{\"schemaVersion\":{ARCHIVE_SCHEMA_VERSION},\"kind\":\"{ARCHIVE_KIND}\",\"producerVersion\":"
    )?;
    serde_json::to_writer(&mut writer, build_version.crate_version())
        .map_err(|error| archive_io(format!("encode producer version: {error}")))?;
    if let Some(platform_version) = build_version.platform_version() {
        writer.write_all(b",\"producerPlatformVersion\":")?;
        serde_json::to_writer(&mut writer, platform_version)
            .map_err(|error| archive_io(format!("encode producer platform version: {error}")))?;
    }
    write!(
        writer,
        ",\"compression\":\"zstd\",\"contentEncoding\":\"base64\",\"chunkSizeBytes\":{CHUNK_SIZE},\"files\":["
    )?;

    let mut total_bytes = 0u64;
    for (index, (relative, source)) in files.iter().enumerate() {
        if index != 0 {
            writer.write_all(b",")?;
        }
        let metadata = fs::metadata(source)
            .map_err(|error| archive_io(format!("inspect '{}': {error}", source.display())))?;
        let size = metadata.len();
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| archive_io("archive uncompressed byte count overflowed".to_owned()))?;
        let sha256 = hash_file(source)?;
        writer.write_all(b"{\"path\":")?;
        serde_json::to_writer(&mut writer, relative)
            .map_err(|error| archive_io(format!("encode archive path: {error}")))?;
        write!(writer, ",\"sizeBytes\":\"{size}\",\"chunks\":[")?;
        write_file_chunks(source, &mut writer)?;
        write!(writer, "],\"sha256\":\"{sha256}\"}}")?;
    }
    writer.write_all(b"]}")?;
    writer.flush()?;
    Ok(total_bytes)
}

/// Restore a compressed JSON archive into a persistent local-node directory.
pub fn restore(
    data_dir: &Path,
    input: &Path,
    force: bool,
    command: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    require_unix()?;
    let input = fs::canonicalize(input)
        .map_err(|error| archive_io(format!("open archive '{}': {error}", input.display())))?;
    let data_dir = prepare_restore_data_dir(data_dir)?;
    ensure_outside_data_dir(&data_dir, &input)?;
    // Keep the destination locked through validation and the directory swap so
    // a node cannot start against state that is about to be replaced.
    let _old_lock = DataDirLock::acquire(&data_dir)?;
    if data_dir_is_populated(&data_dir)? && !force {
        return Err(CliError::ChainRestoreRequiresForce(
            data_dir.display().to_string(),
        ));
    }

    let parent = data_dir.parent().ok_or_else(|| {
        archive_io(format!(
            "data directory '{}' has no parent",
            data_dir.display()
        ))
    })?;
    // Restore into a sibling directory first. No unverified archive content is
    // installed into the live data directory.
    let staging = tempfile::Builder::new()
        .prefix(".groundhog-chain-restore-")
        .tempdir_in(parent)
        .map_err(|error| archive_io(format!("create restore staging directory: {error}")))?;
    set_directory_permissions(staging.path())?;
    for name in PERSISTENT_DIRS {
        let path = staging.path().join(name);
        fs::create_dir(&path)?;
        set_directory_permissions(&path)?;
    }
    let new_lock = DataDirLock::acquire(staging.path())?;

    let task = print.start_task(format!("Restoring chain state into {}", data_dir.display()));
    let archive = File::open(&input)?;
    let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(archive));
    let stats = ArchiveSeed {
        root: staging.path(),
    }
    .deserialize(&mut deserializer)
    .map_err(|error| CliError::ChainArchiveInvalid {
        path: input.display().to_string(),
        message: error.to_string(),
    })?;
    deserializer
        .end()
        .map_err(|error| CliError::ChainArchiveInvalid {
            path: input.display().to_string(),
            message: error.to_string(),
        })?;
    validate_required_entries(&stats).map_err(|message| CliError::ChainArchiveInvalid {
        path: input.display().to_string(),
        message,
    })?;
    // Flush file contents during decoding and directory entries here before the
    // staged replacement below.
    sync_tree_directories(staging.path())?;

    let backup = unused_backup_path(parent, data_dir.file_name())?;
    // The two-step swap preserves the old directory for rollback if installing
    // the staged restore fails.
    fs::rename(&data_dir, &backup)
        .map_err(|error| archive_io(format!("move existing data directory aside: {error}")))?;
    let staging_path = staging.keep();
    if let Err(error) = fs::rename(&staging_path, &data_dir) {
        let rollback = fs::rename(&backup, &data_dir);
        let staging_cleanup = fs::remove_dir_all(&staging_path);
        return Err(archive_io(format!(
            "install restored data: {error}; rollback result: {rollback:?}; staging cleanup: {staging_cleanup:?}"
        )));
    }
    sync_directory(parent)?;
    drop(new_lock);

    let cleanup_warning = match fs::remove_dir_all(&backup) {
        Ok(()) => {
            sync_directory(parent)?;
            None
        }
        Err(error) => {
            let warning = format!(
                "restored state is active, but old state could not be removed from '{}': {error}",
                backup.display()
            );
            print.warn(&warning);
            Some(warning)
        }
    };
    task.success("Chain restore complete");

    let report = ChainRestoreReport {
        schema_version: REPORT_SCHEMA_VERSION,
        archive_path: input,
        data_dir,
        file_count: stats.file_count,
        uncompressed_bytes: stats.uncompressed_bytes.to_string(),
        cleanup_warning,
    };
    if print.is_json() {
        print.json(&CommandResult::new(command, &report))?;
    } else {
        print.detail("Data directory", report.data_dir.display());
        print.detail("Files", report.file_count);
        print.detail("Restored bytes", &report.uncompressed_bytes);
    }
    Ok(())
}

// Rejects platforms that cannot provide the locking and permission guarantees used here.
fn require_unix() -> Result<(), CliError> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(CliError::UnsupportedOs(std::env::consts::OS.to_owned()))
    }
}

// Wraps archive-specific context in the CLI's stable error category.
fn archive_io(message: String) -> CliError {
    CliError::ChainArchiveIo(message)
}

// Resolves a relative path against the process working directory without requiring it to exist.
fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

// Validates that a data directory exists and is not itself a symlink.
fn require_existing_data_dir(path: &Path) -> Result<PathBuf, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        archive_io(format!(
            "inspect data directory '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(archive_io(format!(
            "data directory '{}' must be a real directory",
            path.display()
        )));
    }
    fs::canonicalize(path).map_err(|error| {
        archive_io(format!(
            "resolve data directory '{}': {error}",
            path.display()
        ))
    })
}

// Creates a missing restore destination and returns its canonical path.
fn prepare_restore_data_dir(path: &Path) -> Result<PathBuf, CliError> {
    let absolute = absolute_path(path)?;
    if !absolute.exists() {
        fs::create_dir_all(&absolute)?;
        set_directory_permissions(&absolute)?;
    }
    require_existing_data_dir(&absolute)
}

// Ensures the archive cannot be read from or written into the managed state tree.
fn ensure_outside_data_dir(data_dir: &Path, archive: &Path) -> Result<(), CliError> {
    // Canonicalize the existing portion of the path so `..` and symlinked
    // parents cannot disguise an archive located inside the data directory.
    let candidate = if archive.exists() {
        fs::canonicalize(archive)?
    } else {
        let parent = archive.parent().ok_or_else(|| {
            archive_io(format!(
                "archive path '{}' has no parent",
                archive.display()
            ))
        })?;
        fs::canonicalize(parent)?.join(
            archive
                .file_name()
                .ok_or_else(|| archive_io("archive path has no file name".to_owned()))?,
        )
    };
    if candidate.starts_with(data_dir) {
        return Err(CliError::ChainArchiveInsideDataDir(
            candidate.display().to_string(),
        ));
    }
    Ok(())
}

// Collects the persistent state files in deterministic archive-path order.
fn collect_archive_files(data_dir: &Path) -> Result<Vec<(String, PathBuf)>, CliError> {
    let metadata = data_dir.join(DATA_DIR_METADATA);
    require_regular_file(&metadata)?;
    let mut files = vec![(DATA_DIR_METADATA.to_owned(), metadata)];
    for directory in PERSISTENT_DIRS {
        let root = data_dir.join(directory);
        let metadata = fs::symlink_metadata(&root).map_err(|error| {
            archive_io(format!(
                "inspect required directory '{}': {error}",
                root.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(archive_io(format!(
                "required path '{}' must be a real directory",
                root.display()
            )));
        }
        collect_directory_files(data_dir, &root, &mut files)?;
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

// Recursively collects regular files while rejecting links and special files.
fn collect_directory_files(
    data_dir: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), CliError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        // Never follow links while walking state: that could capture files
        // outside the data directory or make the snapshot self-referential.
        if metadata.file_type().is_symlink() {
            return Err(archive_io(format!(
                "symlink '{}' is not allowed in a chain dump",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_directory_files(data_dir, &path, files)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(data_dir).map_err(|error| {
                archive_io(format!("make '{}' relative: {error}", path.display()))
            })?;
            files.push((portable_relative_path(relative)?, path));
        } else {
            return Err(archive_io(format!(
                "special file '{}' is not allowed in a chain dump",
                path.display()
            )));
        }
    }
    Ok(())
}

// Converts a native relative path into the archive's portable slash-separated form.
fn portable_relative_path(path: &Path) -> Result<String, CliError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(archive_io(format!(
                "unsafe archive path '{}'",
                path.display()
            )));
        };
        parts.push(part.to_str().ok_or_else(|| {
            archive_io(format!("archive path '{}' is not UTF-8", path.display()))
        })?);
    }
    // Archive paths always use `/`, independent of the host path separator.
    Ok(parts.join("/"))
}

// Requires a path to name a real regular file rather than a link or special file.
fn require_regular_file(path: &Path) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        archive_io(format!(
            "inspect required file '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(archive_io(format!(
            "required path '{}' must be a regular file",
            path.display()
        )));
    }
    Ok(())
}

// Computes a file's lowercase SHA-256 digest without loading the whole file.
fn hash_file(path: &Path) -> Result<String, CliError> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

// Compresses and base64-encodes one file as a bounded JSON chunk sequence.
fn write_file_chunks(path: &Path, writer: &mut impl Write) -> Result<(), CliError> {
    let mut file = BufReader::new(File::open(path)?);
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut first = true;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let compressed = zstd::bulk::compress(&buffer[..count], COMPRESSION_LEVEL)
            .map_err(|error| archive_io(format!("compress '{}': {error}", path.display())))?;
        let encoded = BASE64.encode(compressed);
        if !first {
            writer.write_all(b",")?;
        }
        first = false;
        serde_json::to_writer(&mut *writer, &encoded)
            .map_err(|error| archive_io(format!("encode archive chunk: {error}")))?;
    }
    Ok(())
}

struct ArchiveSeed<'a> {
    root: &'a Path,
}

// The seed/visitor hierarchy decodes directly from the input stream and writes
// each validated file into staging without retaining the full archive in RAM.
impl<'de> DeserializeSeed<'de> for ArchiveSeed<'_> {
    type Value = ArchiveStats;

    // Starts streaming deserialization of the top-level archive object.
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ArchiveVisitor { root: self.root })
    }
}

struct ArchiveVisitor<'a> {
    root: &'a Path,
}

impl<'de> Visitor<'de> for ArchiveVisitor<'_> {
    type Value = ArchiveStats;

    // Describes the top-level shape expected by serde diagnostics.
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a canonical Groundhog chain archive")
    }

    // Validates archive metadata before delegating the file stream.
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema = None;
        let mut kind = None;
        let mut compression = None;
        let mut encoding = None;
        let mut chunk_size = None;
        let mut stats = ArchiveStats::default();
        let mut saw_files = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "schemaVersion" => schema = Some(map.next_value::<u32>()?),
                "kind" => kind = Some(map.next_value::<String>()?),
                "producerVersion" | "producerPlatformVersion" => {
                    map.next_value::<String>()?;
                }
                "compression" => compression = Some(map.next_value::<String>()?),
                "contentEncoding" => encoding = Some(map.next_value::<String>()?),
                "chunkSizeBytes" => chunk_size = Some(map.next_value::<usize>()?),
                "files" => {
                    // The writer emits format metadata before `files`; enforcing
                    // that order lets chunks be decoded only after their codec
                    // and size contract have been validated.
                    if !matches!(
                        schema,
                        Some(LEGACY_ARCHIVE_SCHEMA_VERSION | ARCHIVE_SCHEMA_VERSION)
                    ) || kind.as_deref() != Some(ARCHIVE_KIND)
                        || compression.as_deref() != Some("zstd")
                        || encoding.as_deref() != Some("base64")
                        || chunk_size != Some(CHUNK_SIZE)
                    {
                        return Err(A::Error::custom(
                            "archive metadata must precede files and match the supported format",
                        ));
                    }
                    stats = map.next_value_seed(FilesSeed {
                        root: self.root,
                        stats,
                    })?;
                    saw_files = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !saw_files {
            return Err(A::Error::custom("archive is missing files"));
        }
        Ok(stats)
    }
}

struct FilesSeed<'a> {
    root: &'a Path,
    stats: ArchiveStats,
}

impl<'de> DeserializeSeed<'de> for FilesSeed<'_> {
    type Value = ArchiveStats;

    // Starts deserializing file records while carrying aggregate statistics forward.
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(FilesVisitor {
            root: self.root,
            stats: self.stats,
        })
    }
}

struct FilesVisitor<'a> {
    root: &'a Path,
    stats: ArchiveStats,
}

impl<'de> Visitor<'de> for FilesVisitor<'_> {
    type Value = ArchiveStats;

    // Describes the file-list shape expected by serde diagnostics.
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an array of archived files")
    }

    // Restores each file record and returns the accumulated archive statistics.
    fn visit_seq<A>(mut self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(()) = sequence.next_element_seed(FileSeed {
            root: self.root,
            stats: &mut self.stats,
        })? {}
        Ok(self.stats)
    }
}

struct FileSeed<'a> {
    root: &'a Path,
    stats: &'a mut ArchiveStats,
}

impl<'de> DeserializeSeed<'de> for FileSeed<'_> {
    type Value = ();

    // Starts deserializing one file record into the shared restore state.
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(FileVisitor {
            root: self.root,
            stats: self.stats,
        })
    }
}

struct FileVisitor<'a> {
    root: &'a Path,
    stats: &'a mut ArchiveStats,
}

impl<'de> Visitor<'de> for FileVisitor<'_> {
    type Value = ();

    // Describes the per-file object shape expected by serde diagnostics.
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a canonical archived file")
    }

    // Writes one file into staging and verifies its declared size and digest.
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut path = None;
        let mut size = None;
        let mut restored = None;
        let mut sha256 = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "path" => path = Some(map.next_value::<String>()?),
                "sizeBytes" => {
                    let value = map.next_value::<String>()?;
                    size = Some(value.parse::<u64>().map_err(A::Error::custom)?);
                }
                "chunks" => {
                    // Field ordering is part of the canonical streaming format:
                    // the target and declared limit must be known before bytes
                    // are accepted from the archive.
                    let relative = path
                        .as_deref()
                        .ok_or_else(|| A::Error::custom("file path must precede chunks"))?;
                    let expected_size =
                        size.ok_or_else(|| A::Error::custom("file sizeBytes must precede chunks"))?;
                    validate_archive_path(relative).map_err(A::Error::custom)?;
                    if !self.stats.paths.insert(relative.to_owned()) {
                        return Err(A::Error::custom(format!(
                            "duplicate archive path '{relative}'"
                        )));
                    }
                    let target = self.root.join(relative);
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(A::Error::custom)?;
                        set_directory_permissions(parent).map_err(A::Error::custom)?;
                    }
                    let file = create_restore_file(&target).map_err(A::Error::custom)?;
                    restored = Some(map.next_value_seed(ChunksSeed {
                        file,
                        expected_size,
                    })?);
                }
                "sha256" => sha256 = Some(map.next_value::<String>()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let relative = path.ok_or_else(|| A::Error::custom("file is missing path"))?;
        let expected_size = size.ok_or_else(|| A::Error::custom("file is missing sizeBytes"))?;
        let restored = restored.ok_or_else(|| A::Error::custom("file is missing chunks"))?;
        let expected_sha = sha256.ok_or_else(|| A::Error::custom("file is missing sha256"))?;
        if restored.bytes != expected_size {
            return Err(A::Error::custom(format!(
                "file '{relative}' restored {} bytes, expected {expected_size}",
                restored.bytes
            )));
        }
        let actual_sha = hex::encode(restored.hasher.finalize());
        if actual_sha != expected_sha {
            return Err(A::Error::custom(format!(
                "file '{relative}' SHA-256 mismatch"
            )));
        }
        restored.file.sync_all().map_err(A::Error::custom)?;
        self.stats.file_count += 1;
        self.stats.uncompressed_bytes =
            self.stats
                .uncompressed_bytes
                .checked_add(restored.bytes)
                .ok_or_else(|| A::Error::custom("archive byte count overflowed"))?;
        Ok(())
    }
}

struct RestoredFile {
    file: File,
    hasher: Sha256,
    bytes: u64,
}

struct ChunksSeed {
    file: File,
    expected_size: u64,
}

impl<'de> DeserializeSeed<'de> for ChunksSeed {
    type Value = RestoredFile;

    // Starts decoding a file's chunk sequence into its newly created destination.
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ChunksVisitor {
            restored: RestoredFile {
                file: self.file,
                hasher: Sha256::new(),
                bytes: 0,
            },
            expected_size: self.expected_size,
        })
    }
}

struct ChunksVisitor {
    restored: RestoredFile,
    expected_size: u64,
}

impl<'de> Visitor<'de> for ChunksVisitor {
    type Value = RestoredFile;

    // Describes the encoded chunk-list shape expected by serde diagnostics.
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an array of zstd+base64 chunks")
    }

    // Decodes chunks into the target while enforcing the file's declared size.
    fn visit_seq<A>(mut self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(encoded) = sequence.next_element::<String>()? {
            // Base64 plus zstd overhead should stay well below twice the raw
            // chunk size; reject oversized input before allocating its decode.
            if encoded.len() > (CHUNK_SIZE * 2) {
                return Err(A::Error::custom("encoded archive chunk is too large"));
            }
            let compressed = BASE64.decode(encoded).map_err(A::Error::custom)?;
            let raw = zstd::bulk::decompress(&compressed, CHUNK_SIZE).map_err(A::Error::custom)?;
            if raw.is_empty() {
                return Err(A::Error::custom("archive chunks must not be empty"));
            }
            self.restored.bytes = self
                .restored
                .bytes
                .checked_add(u64::try_from(raw.len()).map_err(A::Error::custom)?)
                .ok_or_else(|| A::Error::custom("restored file size overflowed"))?;
            if self.restored.bytes > self.expected_size {
                return Err(A::Error::custom("restored file exceeds its declared size"));
            }
            self.restored
                .file
                .write_all(&raw)
                .map_err(A::Error::custom)?;
            self.restored.hasher.update(&raw);
        }
        Ok(self.restored)
    }
}

// Restricts an archive entry to the known persistent-state namespace.
fn validate_archive_path(path: &str) -> Result<(), String> {
    // Accept only portable relative paths. This blocks absolute paths,
    // traversal components, and Windows separators before joining with root.
    if path.is_empty() || path.len() > 4096 || path.contains('\\') {
        return Err(format!("unsafe archive path '{path}'"));
    }
    let parsed = Path::new(path);
    if parsed
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe archive path '{path}'"));
    }
    if path == DATA_DIR_METADATA {
        return Ok(());
    }
    if PERSISTENT_DIRS
        .iter()
        .any(|prefix| path.starts_with(&format!("{prefix}/")))
    {
        return Ok(());
    }
    Err(format!("archive path '{path}' is outside persistent state"))
}

// Confirms that the minimum state needed to start a node was restored.
fn validate_required_entries(stats: &ArchiveStats) -> Result<(), String> {
    if !stats.paths.contains(DATA_DIR_METADATA) {
        return Err(format!("archive is missing {DATA_DIR_METADATA}"));
    }
    if !stats.paths.iter().any(|path| path.starts_with("ramdb/")) {
        return Err("archive contains no RAMDB files".to_owned());
    }
    Ok(())
}

#[cfg(unix)]
// Creates a restore target exclusively with owner-only permissions.
fn create_restore_file(path: &Path) -> Result<File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
// Provides a compile-time fallback for platforms rejected by `require_unix`.
fn create_restore_file(path: &Path) -> Result<File, std::io::Error> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

// Distinguishes durable chain contents from harmless process-management files.
fn data_dir_is_populated(data_dir: &Path) -> Result<bool, CliError> {
    for entry in fs::read_dir(data_dir)? {
        let name = entry?.file_name();
        // Lock files and ephemeral runtime metadata may be created by command
        // setup; they do not represent chain state that requires `--force`.
        if name != LOCK_FILE && name != NODE_PID_FILE && name != "runtime" {
            return Ok(true);
        }
    }
    Ok(false)
}

// Finds a collision-free sibling name for the data directory being replaced.
fn unused_backup_path(
    parent: &Path,
    file_name: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, CliError> {
    let name = file_name
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("chain-data");
    for sequence in 0..1000u32 {
        let candidate = parent.join(format!(
            ".{name}.groundhog-replaced-{}-{sequence}",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(archive_io(
        "could not allocate replacement backup path".to_owned(),
    ))
}

// Flushes every directory in a restored tree from the leaves upward.
fn sync_tree_directories(root: &Path) -> Result<(), CliError> {
    // Recurses depth-first so child directory entries are durable before their parents.
    fn visit(path: &Path) -> Result<(), CliError> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(&entry.path())?;
            }
        }
        // Sync children before their parent so every restored directory entry
        // reaches stable storage before the top-level rename is persisted.
        sync_directory(path)
    }

    visit(root)
}

// Flushes changes to a directory entry to stable storage.
fn sync_directory(path: &Path) -> Result<(), CliError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
// Applies owner-only access to restored directories.
fn set_directory_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
// Leaves permissions unchanged on platforms rejected by `require_unix`.
fn set_directory_permissions(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

struct DataDirLock {
    _files: Vec<File>,
}

impl DataDirLock {
    #[cfg(unix)]
    // Acquires all lock identities used by supported local-node tooling.
    fn acquire(data_dir: &Path) -> Result<Self, CliError> {
        // The supervisor and older tooling use these two lock identities. Both
        // must be held to exclude every supported node startup path.
        let files = vec![
            acquire_lock_file(data_dir, LOCK_FILE)?,
            acquire_lock_file(data_dir, NODE_PID_FILE)?,
        ];
        Ok(Self { _files: files })
    }

    #[cfg(not(unix))]
    // Reports the unsupported platform instead of providing weaker locking.
    fn acquire(_data_dir: &Path) -> Result<Self, CliError> {
        Err(CliError::UnsupportedOs(std::env::consts::OS.to_owned()))
    }
}

#[cfg(unix)]
// Opens and non-blockingly locks one owner-only lock file without following symlinks.
fn acquire_lock_file(data_dir: &Path, name: &str) -> Result<File, CliError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let path = data_dir.join(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        // O_NOFOLLOW closes the final-component symlink race after the earlier
        // directory validation; O_CLOEXEC keeps locks out of child processes.
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| archive_io(format!("open lock '{}': {error}", path.display())))?;
    // SAFETY: flock only inspects the valid file descriptor and does not retain pointers.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            return Err(CliError::ChainDataDirLocked(data_dir.display().to_string()));
        }
        return Err(archive_io(format!("lock '{}': {error}", path.display())));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Builds the smallest representative persistent data directory for archive tests.
    fn fixture() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join(DATA_DIR_METADATA),
            b"{\"schemaVersion\":1}",
        )
        .unwrap();
        for directory in PERSISTENT_DIRS {
            fs::create_dir(temp.path().join(directory)).unwrap();
        }
        fs::write(temp.path().join("ramdb/0.log"), vec![7u8; CHUNK_SIZE + 17]).unwrap();
        fs::write(temp.path().join("events/event.bin"), b"events").unwrap();
        fs::write(temp.path().join("indexer/CURRENT"), b"MANIFEST-1\n").unwrap();
        temp
    }

    #[test]
    // Verifies that every persistent file survives a dump-and-restore cycle unchanged.
    fn dump_and_restore_round_trip() {
        let source = fixture();
        let parent = TempDir::new().unwrap();
        let archive = parent.path().join("chain.json");
        dump(
            source.path(),
            &archive,
            false,
            "chain.dump",
            &Print::quiet(),
        )
        .unwrap();

        let restored = parent.path().join("restored");
        restore(&restored, &archive, false, "chain.restore", &Print::quiet()).unwrap();
        for relative in [
            DATA_DIR_METADATA,
            "ramdb/0.log",
            "events/event.bin",
            "indexer/CURRENT",
        ] {
            assert_eq!(
                fs::read(source.path().join(relative)).unwrap(),
                fs::read(restored.join(relative)).unwrap()
            );
        }
        assert!(!restored.join("runtime").exists());
    }

    #[test]
    fn archive_envelope_reports_crate_and_platform_versions() {
        let source = fixture();
        let files = collect_archive_files(source.path()).unwrap();
        let archive = tempfile::NamedTempFile::new().unwrap();
        write_archive(
            &mut archive.as_file().try_clone().unwrap(),
            &files,
            BuildVersion::new("0.9.6", Some("0.1.23")),
        )
        .unwrap();

        let envelope: serde_json::Value =
            serde_json::from_slice(&fs::read(archive.path()).unwrap()).unwrap();
        assert_eq!(envelope["schemaVersion"], 2);
        assert_eq!(envelope["producerVersion"], "0.9.6");
        assert_eq!(envelope["producerPlatformVersion"], "0.1.23");
    }

    #[test]
    fn restore_accepts_legacy_v1_archive() {
        let source = fixture();
        let files = collect_archive_files(source.path()).unwrap();
        let parent = TempDir::new().unwrap();
        let archive = parent.path().join("chain-v1.json");
        let mut output = File::create(&archive).unwrap();
        write_archive(&mut output, &files, BuildVersion::new("0.9.5", None)).unwrap();
        drop(output);

        let encoded = fs::read_to_string(&archive).unwrap().replacen(
            &format!("\"schemaVersion\":{ARCHIVE_SCHEMA_VERSION}"),
            &format!("\"schemaVersion\":{LEGACY_ARCHIVE_SCHEMA_VERSION}"),
            1,
        );
        fs::write(&archive, encoded).unwrap();

        let restored = parent.path().join("restored");
        restore(&restored, &archive, false, "chain.restore", &Print::quiet()).unwrap();
        assert_eq!(
            fs::read(restored.join("ramdb/0.log")).unwrap(),
            fs::read(source.path().join("ramdb/0.log")).unwrap()
        );
    }

    #[test]
    // Verifies that traversal, absolute, foreign-root, and backslash paths are rejected.
    fn archive_path_validation_rejects_traversal_and_unknown_roots() {
        for path in [
            "../ramdb/0.log",
            "/ramdb/0.log",
            "other/file",
            "ramdb\\0.log",
        ] {
            assert!(validate_archive_path(path).is_err(), "accepted {path}");
        }
        assert!(validate_archive_path("ramdb/0.log").is_ok());
    }

    #[test]
    // Verifies that restore detects content whose digest no longer matches the archive.
    fn restore_rejects_tampered_file_hash() {
        let source = fixture();
        let parent = TempDir::new().unwrap();
        let archive = parent.path().join("chain.json");
        dump(
            source.path(),
            &archive,
            false,
            "chain.dump",
            &Print::quiet(),
        )
        .unwrap();
        let mut encoded = fs::read_to_string(&archive).unwrap();
        let marker = "\"sha256\":\"";
        let hash_start = encoded.find(marker).unwrap() + marker.len();
        let replacement = if &encoded[hash_start..=hash_start] == "f" {
            "e"
        } else {
            "f"
        };
        encoded.replace_range(hash_start..=hash_start, replacement);
        fs::write(&archive, encoded).unwrap();

        let error = restore(
            &parent.path().join("restored"),
            &archive,
            false,
            "chain.restore",
            &Print::quiet(),
        )
        .unwrap_err();
        assert!(matches!(error, CliError::ChainArchiveInvalid { .. }));
    }

    #[cfg(unix)]
    #[test]
    // Verifies that dumping never follows a symlink from persistent state.
    fn dump_rejects_symlinks_in_persistent_state() {
        use std::os::unix::fs::symlink;

        let source = fixture();
        symlink(
            source.path().join("ramdb/0.log"),
            source.path().join("events/link"),
        )
        .unwrap();
        let parent = TempDir::new().unwrap();
        let error = dump(
            source.path(),
            &parent.path().join("chain.json"),
            false,
            "chain.dump",
            &Print::quiet(),
        )
        .unwrap_err();
        assert!(matches!(error, CliError::ChainArchiveIo(_)));
    }

    #[cfg(unix)]
    #[test]
    // Verifies that two archive operations cannot hold the data-directory lock together.
    fn dump_rejects_concurrent_archive_operation() {
        let source = fixture();
        let _lock = DataDirLock::acquire(source.path()).unwrap();
        let parent = TempDir::new().unwrap();
        let error = dump(
            source.path(),
            &parent.path().join("chain.json"),
            false,
            "chain.dump",
            &Print::quiet(),
        )
        .unwrap_err();
        assert!(matches!(error, CliError::ChainDataDirLocked(_)));
    }

    #[cfg(unix)]
    #[test]
    // Verifies that a node-held PID lock prevents a dump from starting.
    fn dump_rejects_running_node_pid_lock() {
        let source = fixture();
        let _node_lock = acquire_lock_file(source.path(), NODE_PID_FILE).unwrap();
        let parent = TempDir::new().unwrap();
        let archive = parent.path().join("chain.json");

        let error = dump(
            source.path(),
            &archive,
            false,
            "chain.dump",
            &Print::quiet(),
        )
        .unwrap_err();

        assert!(matches!(error, CliError::ChainDataDirLocked(_)));
        assert!(!archive.exists());
    }

    #[cfg(unix)]
    #[test]
    // Verifies that a node-held PID lock prevents restore from replacing live state.
    fn restore_rejects_running_node_pid_lock() {
        let source = fixture();
        let parent = TempDir::new().unwrap();
        let archive = parent.path().join("chain.json");
        dump(
            source.path(),
            &archive,
            false,
            "chain.dump",
            &Print::quiet(),
        )
        .unwrap();

        let destination = fixture();
        let original = fs::read(destination.path().join("ramdb/0.log")).unwrap();
        let _node_lock = acquire_lock_file(destination.path(), NODE_PID_FILE).unwrap();

        let error = restore(
            destination.path(),
            &archive,
            true,
            "chain.restore",
            &Print::quiet(),
        )
        .unwrap_err();

        assert!(matches!(error, CliError::ChainDataDirLocked(_)));
        assert_eq!(
            fs::read(destination.path().join("ramdb/0.log")).unwrap(),
            original
        );
    }
}
