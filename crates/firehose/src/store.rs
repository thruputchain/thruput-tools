//! Immutable height-named XDR files. Atomic publication makes each block and
//! its replay cursor one durable record; the directory is the height index.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use groundhog_indexer::{
    EventIndexerPulledBlock, IndexerInitializedProgress, IndexerOutboxOrigin, IndexerProgress,
};

use crate::{Error, block_height, decode, encode, invalid, verify::verify};

/// Decimal height width shared by record writers and directory discovery.
pub(crate) const RECORD_HEIGHT_WIDTH: usize = 20;

/// The directory is the height index, so the writer and every archive reader
/// must agree on this name.
pub(crate) fn record_path(directory: &Path, height: i64) -> PathBuf {
    directory.join(format!("{height:0RECORD_HEIGHT_WIDTH$}.xdr"))
}

/// An exclusively locked, verified local archive.
#[derive(Debug)]
pub struct Store {
    directory: PathBuf,
    _lock: File,
    progress: Option<IndexerInitializedProgress>,
    /// Total bytes in durable block and origin records (excludes filesystem allocation).
    pub bytes: u64,
    /// Allocated record bytes, including filesystem block rounding on Unix.
    pub allocated_bytes: u64,
    /// Number of immutable blocks on disk.
    pub blocks: u64,
}

impl Store {
    /// Verify all records and reconstruct progress before accepting new blocks.
    pub fn open(directory: &Path) -> Result<Self, Error> {
        fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("LOCK"))?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        let mut store = Self {
            directory: directory.to_owned(),
            _lock: lock,
            progress: None,
            bytes: 0,
            allocated_bytes: 0,
            blocks: 0,
        };
        let origin_path = directory.join("origin.xdr");
        if origin_path.exists() {
            let bytes = fs::read(&origin_path)?;
            let origin = decode(&bytes)?;
            store.adopt_memory(origin)?;
            store.bytes += bytes.len() as u64;
            store.allocated_bytes += allocated_bytes(&origin_path)?;
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| invalid("non-UTF8 archive filename"))?;
            if matches!(name, "LOCK" | "pending" | "origin.xdr") {
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "xdr") {
                return Err(invalid("unexpected archive entry"));
            }
            paths.push(path);
        }
        paths.sort();
        for path in paths {
            let bytes = fs::read(&path)?;
            let pulled: EventIndexerPulledBlock = decode(&bytes)?;
            if path != record_path(directory, block_height(&pulled)) {
                return Err(invalid("height filename mismatch"));
            }
            verify(&pulled)?;
            store.check_next(&pulled)?;
            store.advance(&pulled);
            store.bytes += bytes.len() as u64;
            store.allocated_bytes += allocated_bytes(&path)?;
            store.blocks += 1;
        }
        Ok(store)
    }

    /// Cursor encoded in durable records; parked state is connection-local.
    pub fn cursor(&self, parked: bool) -> IndexerProgress {
        match self.progress.clone() {
            Some(initialized) if parked => IndexerProgress::IndexerProgressCaughtUp { initialized },
            Some(initialized) => IndexerProgress::IndexerProgressInitialized { initialized },
            None => IndexerProgress::Default { state: 0 },
        }
    }

    /// Highest contiguous height, or -1 before adopting an origin.
    pub fn height(&self) -> i64 {
        self.progress.as_ref().map_or(-1, |p| *p.contiguous_height)
    }

    fn publish(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        let pending = self.directory.join("pending");
        // A crash after hard_link but before unlink can leave pending pointing
        // at an already committed inode. Never truncate that inode on restart.
        match fs::remove_file(&pending) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        // Link cannot overwrite an immutable record, even if the caller is wrong.
        fs::hard_link(&pending, path)?;
        File::open(&self.directory)?.sync_all()?;
        fs::remove_file(pending)?;
        Ok(())
    }

    fn adopt_memory(&mut self, origin: IndexerOutboxOrigin) -> Result<(), Error> {
        if *origin.first_height < 0 {
            return Err(invalid("negative epoch first height"));
        }
        if let Some(previous) = &self.progress {
            if previous.origin == origin {
                return Ok(());
            }
            if origin.epoch_sequence <= previous.origin.epoch_sequence
                || previous.contiguous_height.checked_add(1) != Some(origin.first_height.clone())
                || origin.first_prev_hash != previous.last_consensus_hash
            {
                return Err(invalid("epoch does not continue stored cursor"));
            }
        }
        self.progress = Some(IndexerInitializedProgress {
            contiguous_height: (*origin.first_height - 1).into(),
            last_consensus_hash: origin.first_prev_hash.clone(),
            origin,
        });
        Ok(())
    }

    /// Adopt a caught-up epoch boundary, preserving it even on an empty chain.
    pub fn adopt(&mut self, origin: &IndexerOutboxOrigin) -> Result<(), Error> {
        let was_empty = self.progress.is_none();
        let old = self.progress.clone();
        self.adopt_memory(origin.clone())?;
        if was_empty {
            let path = self.directory.join("origin.xdr");
            let bytes = encode(origin)?;
            if let Err(error) = self.publish(&path, &bytes) {
                self.progress = old;
                return Err(error);
            }
            self.bytes += bytes.len() as u64;
            self.allocated_bytes += allocated_bytes(&path)?;
        }
        Ok(())
    }

    fn check_next(&self, pulled: &EventIndexerPulledBlock) -> Result<(), Error> {
        let value = &pulled.block.payload.consensus_value;
        let origin = &pulled.origin;
        if *origin.first_height < 0 {
            return Err(invalid("negative epoch first height"));
        }
        if let Some(previous) = &self.progress {
            if previous.contiguous_height.checked_add(1)
                != Some(value.proposal_header.block_height.clone())
                || value.prev_hash != previous.last_consensus_hash
            {
                return Err(invalid("block gap or previous hash mismatch"));
            }
            if previous.origin != *origin
                && (origin.epoch_sequence <= previous.origin.epoch_sequence
                    || origin.first_height != value.proposal_header.block_height
                    || origin.first_prev_hash != value.prev_hash)
            {
                return Err(invalid("invalid epoch transition"));
            }
        } else if value.proposal_header.block_height != origin.first_height
            || value.prev_hash != origin.first_prev_hash
        {
            return Err(invalid("first block does not match origin"));
        }
        Ok(())
    }

    fn advance(&mut self, pulled: &EventIndexerPulledBlock) {
        self.progress = Some(IndexerInitializedProgress {
            origin: pulled.origin.clone(),
            contiguous_height: pulled
                .block
                .payload
                .consensus_value
                .proposal_header
                .block_height
                .clone(),
            last_consensus_hash: pulled.block.identity.consensus_hash.clone(),
        });
    }

    /// Verify and durably publish a block. An identical replay returns false.
    pub fn append(&mut self, pulled: &EventIndexerPulledBlock) -> Result<bool, Error> {
        verify(pulled)?;
        let bytes = encode(pulled)?;
        let path = record_path(&self.directory, block_height(pulled));
        if path.exists() {
            if fs::read(path)? == bytes {
                return Ok(false);
            }
            return Err(invalid("immutable height already has different content"));
        }
        self.check_next(pulled)?;
        self.publish(&path, &bytes)?;
        self.advance(pulled);
        self.bytes += bytes.len() as u64;
        self.allocated_bytes += allocated_bytes(&path)?;
        self.blocks += 1;
        Ok(true)
    }
}

fn allocated_bytes(path: &Path) -> std::io::Result<u64> {
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(metadata.blocks() * 512)
    }
    #[cfg(not(unix))]
    {
        Ok(metadata.len())
    }
}
