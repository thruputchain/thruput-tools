//! Immutable height-named XDR files. Atomic publication makes each block and
//! its replay cursor one durable record; the directory is the height index.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use thruput_indexer::{
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

/// Bounds applied by the periodic archive pruner.
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// Maximum record age, measured from filesystem modification time.
    pub duration: Duration,
    /// Maximum allocated bytes, including filesystem block rounding.
    pub bytes: u64,
    /// Minimum number of blocks preserved regardless of either budget.
    pub min_blocks: u64,
}

/// An exclusively locked, verified local archive.
#[derive(Debug)]
pub struct Store {
    directory: PathBuf,
    _lock: File,
    progress: Option<IndexerInitializedProgress>,
    floor: Option<i64>,
    /// Number of blocks fully verified during this open.
    pub verified_blocks: u64,
    /// Total bytes in durable block, origin and floor records (excludes filesystem allocation).
    pub bytes: u64,
    /// Allocated record bytes, including filesystem block rounding on Unix.
    pub allocated_bytes: u64,
    /// Number of immutable blocks on disk.
    pub blocks: u64,
}

impl Store {
    /// Reconstruct progress, trusting a consumed clean-shutdown watermark when present.
    pub fn open(directory: &Path) -> Result<Self, Error> {
        Self::recover(directory, true)
    }

    /// Fully verify retained records even when a clean-shutdown watermark exists.
    pub fn open_verified(directory: &Path) -> Result<Self, Error> {
        Self::recover(directory, false)
    }

    #[allow(clippy::too_many_lines)]
    fn recover(directory: &Path, trust_watermark: bool) -> Result<Self, Error> {
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
            floor: None,
            verified_blocks: 0,
            bytes: 0,
            allocated_bytes: 0,
            blocks: 0,
        };
        let watermark_path = directory.join("verified.xdr");
        let watermark: Option<IndexerInitializedProgress> = if watermark_path.exists() {
            let progress = decode(&fs::read(&watermark_path)?)?;
            // Invalidate before accepting writes: a killed run must fully verify next time.
            fs::remove_file(&watermark_path)?;
            File::open(directory)?.sync_all()?;
            trust_watermark.then_some(progress)
        } else {
            None
        };
        let floor_path = directory.join("floor.xdr");
        if floor_path.exists() {
            let bytes = fs::read(&floor_path)?;
            let progress: IndexerInitializedProgress = decode(&bytes)?;
            store.bytes += bytes.len() as u64;
            store.allocated_bytes += allocated_bytes(&floor_path)?;
            if *progress.contiguous_height < 0 || *progress.origin.first_height < 0 {
                return Err(invalid("invalid archive floor"));
            }
            store.floor = Some(
                (*progress.contiguous_height)
                    .checked_add(1)
                    .ok_or_else(|| invalid("archive floor overflow"))?,
            );
            store.progress = Some(progress);
        }
        let origin_path = directory.join("origin.xdr");
        if origin_path.exists() {
            let bytes = fs::read(&origin_path)?;
            let origin = decode(&bytes)?;
            if store.progress.is_none() {
                store.adopt_memory(origin)?;
            }
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
            if matches!(
                name,
                "LOCK" | "pending" | "origin.xdr" | "floor.xdr" | "metadata.xdr"
            ) {
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "xdr") {
                return Err(invalid("unexpected archive entry"));
            }
            paths.push(path);
        }
        paths.sort();
        let mut watermark_seen = false;
        for path in paths {
            if store
                .floor
                .is_some_and(|floor| path < record_path(directory, floor))
            {
                fs::remove_file(path)?;
                continue;
            }
            let bytes = fs::read(&path)?;
            let pulled: EventIndexerPulledBlock = decode(&bytes)?;
            if path != record_path(directory, block_height(&pulled)) {
                return Err(invalid("height filename mismatch"));
            }
            if watermark
                .as_ref()
                .is_none_or(|p| block_height(&pulled) > *p.contiguous_height)
            {
                verify(&pulled)?;
                store.verified_blocks += 1;
            }
            store.check_next(&pulled)?;
            store.advance(&pulled);
            if watermark
                .as_ref()
                .is_some_and(|p| *p.contiguous_height == store.height())
            {
                if store.progress.as_ref() != watermark.as_ref() {
                    return Err(invalid("watermark progress mismatch"));
                }
                watermark_seen = true;
            }
            store.bytes += bytes.len() as u64;
            store.allocated_bytes += allocated_bytes(&path)?;
            store.blocks += 1;
        }
        if watermark.is_some() && !watermark_seen {
            return Err(invalid("watermark does not match retained records"));
        }
        File::open(directory)?.sync_all()?;
        Ok(store)
    }

    /// First retained height, if pruning has established a durable floor.
    pub fn floor_height(&self) -> Option<i64> {
        self.floor
    }

    fn publish_metadata(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let staging = self.directory.join("metadata.xdr");
        if staging.exists() {
            fs::remove_file(&staging)?;
        }
        self.publish(&staging, bytes)?;
        fs::rename(staging, self.directory.join(name))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    /// Publish verified tip progress only after ingestion has stopped cleanly.
    pub fn clean_shutdown(mut self) -> Result<(), Error> {
        if self.blocks > 0 {
            // CaughtUp can adopt an epoch with no new block. Checkpoint only the
            // progress encoded by the tip record, which recovery reconstructs.
            let tip = decode(&fs::read(record_path(&self.directory, self.height()))?)?;
            self.advance(&tip);
            if let Some(progress) = &self.progress {
                self.publish_metadata("verified.xdr", &encode(progress)?)?;
            }
        }
        Ok(())
    }

    /// Publish a floor before unlinking older records, without changing the upstream cursor.
    pub fn prune(&mut self, retention: Retention) -> Result<(), Error> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit()))
            {
                records.push(path);
            }
        }
        records.sort();
        let max_remove = records
            .len()
            .saturating_sub(usize::try_from(retention.min_blocks.max(1)).unwrap_or(usize::MAX));
        let mut remaining = self.allocated_bytes;
        let mut remove = 0;
        for path in records.iter().take(max_remove) {
            let age = SystemTime::now()
                .duration_since(fs::metadata(path)?.modified()?)
                .unwrap_or_default();
            if remaining <= retention.bytes && age <= retention.duration {
                break;
            }
            remaining = remaining.saturating_sub(allocated_bytes(path)?);
            remove += 1;
        }
        if remove == 0 {
            return Ok(());
        }
        let first: EventIndexerPulledBlock = decode(&fs::read(&records[remove])?)?;
        let height = block_height(&first);
        let floor = IndexerInitializedProgress {
            origin: first.origin.clone(),
            contiguous_height: (height - 1).into(),
            last_consensus_hash: first.block.payload.consensus_value.prev_hash.clone(),
        };
        let floor_path = self.directory.join("floor.xdr");
        let (old_bytes, old_allocated) = if floor_path.exists() {
            (
                fs::metadata(&floor_path)?.len(),
                allocated_bytes(&floor_path)?,
            )
        } else {
            (0, 0)
        };
        self.publish_metadata("floor.xdr", &encode(&floor)?)?;
        self.bytes = self.bytes - old_bytes + fs::metadata(&floor_path)?.len();
        self.allocated_bytes = self.allocated_bytes - old_allocated + allocated_bytes(&floor_path)?;
        self.floor = Some(height);
        for path in records.iter().take(remove) {
            let bytes = fs::metadata(path)?.len();
            let allocated = allocated_bytes(path)?;
            fs::remove_file(path)?;
            self.bytes -= bytes;
            self.allocated_bytes -= allocated;
            self.blocks -= 1;
        }
        File::open(&self.directory)?.sync_all()?;
        // The first floor adds an allocation unit. Account for its actual
        // allocation before deciding whether the byte budget is satisfied.
        if self.allocated_bytes > retention.bytes && self.blocks > retention.min_blocks.max(1) {
            self.prune(retention)?;
        }
        Ok(())
    }

    /// Cursor encoded in durable records; parked state is connection-local.
    pub fn cursor(&self, parked: bool) -> IndexerProgress {
        match self.progress.clone() {
            Some(initialized) if parked => IndexerProgress::IndexerProgressCaughtUp { initialized },
            Some(initialized) => IndexerProgress::IndexerProgressInitialized { initialized },
            None => IndexerProgress::Default {
                state: thruput_indexer::IndexerProgressState(0),
            },
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
        Self::check_progress(self.progress.as_ref(), pulled)
    }

    fn check_progress(
        progress: Option<&IndexerInitializedProgress>,
        pulled: &EventIndexerPulledBlock,
    ) -> Result<(), Error> {
        let value = &pulled.block.payload.consensus_value;
        let origin = &pulled.origin;
        if *origin.first_height < 0 {
            return Err(invalid("negative epoch first height"));
        }
        if let Some(previous) = progress {
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
        self.progress = Some(Self::block_progress(pulled));
    }

    fn block_progress(pulled: &EventIndexerPulledBlock) -> IndexerInitializedProgress {
        IndexerInitializedProgress {
            origin: pulled.origin.clone(),
            contiguous_height: pulled
                .block
                .payload
                .consensus_value
                .proposal_header
                .block_height
                .clone(),
            last_consensus_hash: pulled.block.identity.consensus_hash.clone(),
        }
    }

    /// Verify and advance a connection-local bootstrap cursor without writing records.
    pub fn skip_bootstrap(
        progress: &mut Option<IndexerInitializedProgress>,
        pulled: &EventIndexerPulledBlock,
    ) -> Result<(), Error> {
        verify(pulled)?;
        Self::check_progress(progress.as_ref(), pulled)?;
        *progress = Some(Self::block_progress(pulled));
        Ok(())
    }

    /// Durably establish a fresh archive at a verified, caught-up tip.
    pub fn establish_tip(&mut self, progress: IndexerInitializedProgress) -> Result<(), Error> {
        if self.progress.is_some() || self.blocks != 0 {
            return Err(invalid("tip bootstrap requires an empty archive"));
        }
        if *progress.origin.first_height < 0
            || *progress.contiguous_height < *progress.origin.first_height
        {
            return Err(invalid("invalid bootstrap tip"));
        }
        let floor = (*progress.contiguous_height)
            .checked_add(1)
            .ok_or_else(|| invalid("archive floor overflow"))?;
        let bytes = encode(&progress)?;
        self.publish_metadata("floor.xdr", &bytes)?;
        self.bytes += bytes.len() as u64;
        self.allocated_bytes += allocated_bytes(&self.directory.join("floor.xdr"))?;
        self.floor = Some(floor);
        self.progress = Some(progress);
        Ok(())
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
