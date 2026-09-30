//! Read-only directory discovery and verified publication to the subscriber hub.
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use crate::{
    Error,
    metrics::Metrics,
    store::{RECORD_HEIGHT_WIDTH, record_path},
    subscription::Hub,
};

/// A reader that never creates files or takes the writer's lock.
#[derive(Debug)]
pub struct Tailer {
    directory: PathBuf,
    tip: i64,
    modified: Option<SystemTime>,
    awaiting_origin: bool,
    started: Instant,
    discovery_failed: bool,
}

impl Tailer {
    /// Discover names once and verify only the maximum record. Store failures are retried.
    pub fn new(directory: PathBuf) -> Result<Self, Error> {
        let mut tailer = Self {
            directory,
            tip: -1,
            modified: None,
            awaiting_origin: true,
            started: Instant::now(),
            discovery_failed: false,
        };
        let result = (|| {
            if let Some(height) = discover_height(&tailer.directory, Extreme::Maximum)? {
                let path = record_path(&tailer.directory, height);
                verify_record(&fs::read(&path)?, height)?;
                tailer.modified = Some(fs::metadata(path)?.modified()?);
                tailer.tip = height;
                tailer.awaiting_origin = false;
            }
            Ok::<_, Error>(())
        })();
        if let Err(error) = result {
            tailer.discovery_failed = true;
            tracing::error!(%error, "archive tip discovery failed; retrying without adopting tip");
        }
        Ok(tailer)
    }

    /// Highest discovered or verified and published height.
    pub fn tip(&self) -> i64 {
        self.tip
    }

    /// Publish consecutive complete records, stopping at the first missing height.
    pub async fn poll(&mut self, hub: &Hub) -> Result<(), Error> {
        self.poll_with_health(hub, None).await
    }

    async fn poll_with_health(
        &mut self,
        hub: &Hub,
        health: Option<(&Metrics, Duration)>,
    ) -> Result<(), Error> {
        let mut height = if self.awaiting_origin {
            let extreme = if self.discovery_failed {
                Extreme::Maximum
            } else {
                Extreme::Minimum
            };
            let Some(height) = discover_height(&self.directory, extreme)? else {
                self.discovery_failed = false;
                return Ok(());
            };
            height
        } else {
            self.tip
                .checked_add(1)
                .ok_or_else(|| crate::invalid("archive height overflow"))?
        };
        loop {
            let path = record_path(&self.directory, height);
            let metadata = match tokio::fs::metadata(&path).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let bytes = tokio::fs::read(path).await?;
            let block = verify_record(&bytes, height)?;
            let modified = metadata.modified()?;
            hub.publish(crate::subscription::record_from_bytes(&block, bytes));
            self.tip = height;
            self.modified = Some(modified);
            self.awaiting_origin = false;
            self.discovery_failed = false;
            if let Some((metrics, stale)) = health {
                self.update_health(metrics, stale);
            }
            height = height
                .checked_add(1)
                .ok_or_else(|| crate::invalid("archive height overflow"))?;
        }
    }

    /// Refresh health from record age, independently of writer availability.
    pub fn update_health(&self, metrics: &Metrics, stale: Duration) {
        metrics.height.set(self.tip);
        if let Some(modified) = self.modified {
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default();
            metrics.tip_age.set(age.as_secs_f64());
            metrics.set_healthy(age <= stale);
        } else {
            metrics.tip_age.set(f64::NAN);
            metrics.set_healthy(!self.discovery_failed && self.started.elapsed() <= stale);
        }
    }

    /// Tail until cancelled; verification and I/O failures leave subscribers connected.
    pub async fn run(mut self, hub: Hub, metrics: Metrics, interval: Duration, stale: Duration) {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let result = self.poll_with_health(&hub, Some((&metrics, stale))).await;
            self.update_health(&metrics, stale);
            if let Err(error) = result {
                metrics.set_healthy(false);
                tracing::error!(%error, "archive tail failed; retrying without advancing tip");
            }
        }
    }
}

fn verify_record(
    bytes: &[u8],
    height: i64,
) -> Result<groundhog_indexer::EventIndexerPulledBlock, Error> {
    let block = crate::decode(bytes)?;
    crate::verify::verify(&block)?;
    if crate::block_height(&block) != height {
        return Err(crate::invalid("height filename mismatch"));
    }
    Ok(block)
}

#[derive(Clone, Copy)]
enum Extreme {
    Minimum,
    Maximum,
}

fn discover_height(directory: &Path, extreme: Extreme) -> Result<Option<i64>, Error> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut selected = String::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.len() == RECORD_HEIGHT_WIDTH + ".xdr".len()
            && name.as_bytes()[..RECORD_HEIGHT_WIDTH]
                .iter()
                .all(u8::is_ascii_digit)
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "xdr")
            && (selected.is_empty()
                || if matches!(extreme, Extreme::Minimum) {
                    name < selected.as_str()
                } else {
                    name > selected.as_str()
                })
        {
            name.clone_into(&mut selected);
        }
    }
    if selected.is_empty() {
        Ok(None)
    } else {
        selected[..RECORD_HEIGHT_WIDTH]
            .parse()
            .map(Some)
            .map_err(|_| crate::invalid("invalid archive height"))
    }
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_records;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::Config;

    #[test]
    fn empty_archive_grace_expires_from_tailer_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut tailer = Tailer::new(dir.path().to_owned()).unwrap();
        let metrics = Metrics::new().unwrap();
        let grace = Duration::from_mins(1);
        tailer.update_health(&metrics, grace);
        assert!(metrics.is_healthy());
        tailer.started -= grace + Duration::from_secs(1);
        tailer.update_health(&metrics, grace);
        assert!(!metrics.is_healthy());
        assert!(metrics.tip_age.get().is_nan());
    }

    #[tokio::test]
    async fn publication_refreshes_health_before_the_drain_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let mut tailer = Tailer::new(dir.path().to_owned()).unwrap();
        let metrics = Metrics::new().unwrap();
        let config = Config {
            ws_listen: "127.0.0.1:0".parse().unwrap(),
            allow_remote: false,
            max_subscribers: 64,
            subscriber_queue: 8,
            subscriber_queue_bytes: 2 * 1024 * 1024,
            max_record_bytes: 4 * 1024 * 1024,
            live_timeout_ms: 5000,
            catchup_timeout_ms: 10000,
            catchup_progress_ms: 30000,
        };
        let hub = Hub::new(dir.path().to_owned(), -1, config);
        let block = test_records::fixture();
        fs::write(record_path(dir.path(), 1), crate::encode(&block).unwrap()).unwrap();
        let mut invalid = test_records::next(&block);
        invalid.block.identity.consensus_hash = block.block.identity.consensus_hash;
        fs::write(record_path(dir.path(), 2), crate::encode(&invalid).unwrap()).unwrap();
        assert!(
            tailer
                .poll_with_health(&hub, Some((&metrics, Duration::from_mins(1))))
                .await
                .is_err()
        );
        // The valid publication refreshed all three before the subsequent read failed.
        assert_eq!(metrics.height.get(), 1);
        assert!(metrics.tip_age.get() < 60.);
        assert!(metrics.is_healthy());
    }
}
