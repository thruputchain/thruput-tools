//! Read-only directory discovery and verified publication to the subscriber hub.
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::{Error, metrics::Metrics, store::record_path, subscription::Hub};

/// A reader that never creates files or takes the writer's lock.
#[derive(Debug)]
pub struct Tailer {
    directory: PathBuf,
    tip: i64,
    modified: Option<SystemTime>,
    awaiting_origin: bool,
}

impl Tailer {
    /// Discover the maximum fixed-width record name with one directory enumeration.
    /// Record contents are not read. An absent or empty directory has tip -1.
    pub fn new(directory: PathBuf) -> Result<Self, Error> {
        let tip = discover_height(&directory, false)?.unwrap_or(-1);
        let modified = if tip < 0 {
            None
        } else {
            Some(fs::metadata(record_path(&directory, tip))?.modified()?)
        };
        Ok(Self {
            directory,
            tip,
            modified,
            awaiting_origin: tip < 0,
        })
    }

    /// Highest discovered or verified and published height.
    pub fn tip(&self) -> i64 {
        self.tip
    }

    /// Publish consecutive complete records, stopping at the first missing height.
    pub async fn poll(&mut self, hub: &Hub) -> Result<(), Error> {
        if self.awaiting_origin {
            let Some(minimum) = discover_height(&self.directory, true)? else {
                return Ok(());
            };
            self.tip = minimum - 1;
            self.awaiting_origin = false;
        }
        loop {
            let height = self
                .tip
                .checked_add(1)
                .ok_or_else(|| crate::invalid("archive height overflow"))?;
            let path = record_path(&self.directory, height);
            let metadata = match tokio::fs::metadata(&path).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let bytes = tokio::fs::read(path).await?;
            let block = crate::decode(&bytes)?;
            crate::verify::verify(&block)?;
            if crate::block_height(&block) != height {
                return Err(crate::invalid("height filename mismatch"));
            }
            let modified = metadata.modified()?;
            hub.publish(crate::subscription::record_from_bytes(&block, bytes));
            self.tip = height;
            self.modified = Some(modified);
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
            metrics.set_healthy(true);
        }
    }

    /// Tail until cancelled; verification and I/O failures leave subscribers connected.
    pub async fn run(mut self, hub: Hub, metrics: Metrics, poll: Duration, stale: Duration) {
        let mut ticker = tokio::time::interval(poll);
        loop {
            ticker.tick().await;
            let result = self.poll(&hub).await;
            self.update_health(&metrics, stale);
            if let Err(error) = result {
                metrics.set_healthy(false);
                tracing::error!(%error, "archive tail failed; retrying without advancing tip");
            }
        }
    }
}

fn discover_height(directory: &Path, minimum: bool) -> Result<Option<i64>, Error> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut selected = String::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.len() == 24
            && name.as_bytes()[..20].iter().all(u8::is_ascii_digit)
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "xdr")
            && (selected.is_empty()
                || if minimum {
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
        selected[..20]
            .parse()
            .map(Some)
            .map_err(|_| crate::invalid("invalid archive height"))
    }
}
