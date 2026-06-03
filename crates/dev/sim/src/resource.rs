use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::result::ResourceSamplePoint;

/// Boxed future returned by a target resource sampler.
pub type ResourceSamplerFuture<'a> = Pin<Box<dyn Future<Output = ResourceSample> + Send + 'a>>;

/// Samples coarse resource usage for the currently owned target process.
pub trait TargetResourceSampler {
    /// Return a best-effort resource sample for the scheduled simulation time.
    fn sample(&self, scheduled_time_ms: u64) -> ResourceSamplerFuture<'_>;
}

/// Resource sampling result for one scheduled sampler tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceSample {
    /// The sampler observed at least one target process.
    Observed(ResourceSamplePoint),
    /// Sampling was not supported or no target process could be read.
    Unavailable {
        /// Scheduled simulation time for this sampler tick.
        scheduled_time_ms: u64,
        /// Wall-clock observation timestamp in Unix milliseconds.
        observed_at_unix_ms: String,
        /// Human-readable reason sampling was unavailable.
        reason: String,
    },
}

impl ResourceSample {
    /// Build an unavailable sample using the current wall-clock timestamp.
    pub fn unavailable(scheduled_time_ms: u64, reason: impl Into<String>) -> Self {
        Self::Unavailable {
            scheduled_time_ms,
            observed_at_unix_ms: observed_at_unix_ms(),
            reason: reason.into(),
        }
    }
}

/// Sample a process group owned by a local target.
///
/// On Linux this walks `/proc` and aggregates readable processes in `pgid`.
/// Other platforms return `ResourceSample::Unavailable`.
#[cfg(target_os = "linux")]
pub fn sample_process_group(root_pid: u32, pgid: u32, scheduled_time_ms: u64) -> ResourceSample {
    linux::sample_process_group(root_pid, pgid, scheduled_time_ms)
}

/// Sample a process group owned by a local target.
///
/// On Linux this walks `/proc`; other platforms return `ResourceSample::Unavailable`.
#[cfg(not(target_os = "linux"))]
pub fn sample_process_group(_root_pid: u32, _pgid: u32, scheduled_time_ms: u64) -> ResourceSample {
    ResourceSample::unavailable(
        scheduled_time_ms,
        "target resource sampling is unavailable on this platform",
    )
}

fn observed_at_unix_ms() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
        .to_string()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::io;
    use std::path::Path;

    use super::{ResourceSample, observed_at_unix_ms};
    use crate::result::{ResourceSamplePoint, ResourceSampleScope};

    pub fn sample_process_group(
        root_pid: u32,
        process_group_id: u32,
        scheduled_time_ms: u64,
    ) -> ResourceSample {
        let page_size = page_size_bytes();
        let ticks_per_second = ticks_per_second();
        let mut aggregate = ResourceAggregate::default();

        let proc_entries = match fs::read_dir("/proc") {
            Ok(entries) => entries,
            Err(error) => {
                return ResourceSample::unavailable(
                    scheduled_time_ms,
                    format!("failed to read /proc: {error}"),
                );
            }
        };

        for entry in proc_entries.flatten() {
            let file_name = entry.file_name();
            let Some(process_id) = file_name.to_str().and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let proc_path = entry.path();
            let Some(process) = read_process(&proc_path, process_id, page_size, ticks_per_second)
            else {
                continue;
            };
            if process.pgid != process_group_id {
                continue;
            }
            aggregate.add(process);
        }

        if aggregate.process_count == 0 {
            return ResourceSample::unavailable(
                scheduled_time_ms,
                format!("no readable processes found for process group {process_group_id}"),
            );
        }

        ResourceSample::Observed(ResourceSamplePoint {
            simulation_time_ms: scheduled_time_ms,
            observed_at_unix_ms: observed_at_unix_ms(),
            scope: ResourceSampleScope::ProcessGroup,
            root_pid,
            pgid: Some(process_group_id),
            process_count: aggregate.process_count,
            rss_bytes: aggregate.rss_bytes.finish(),
            cpu_user_ms: aggregate.cpu_user_ms.finish(),
            cpu_system_ms: aggregate.cpu_system_ms.finish(),
            threads: aggregate.threads.finish(),
            open_fds: aggregate.open_fds.finish(),
        })
    }

    fn read_process(
        proc_path: &Path,
        process_id: u32,
        page_size: u64,
        ticks_per_second: u64,
    ) -> Option<ProcessSample> {
        let stat = fs::read_to_string(proc_path.join("stat")).ok()?;
        let stat = parse_stat(&stat, page_size, ticks_per_second).ok()?;
        if stat.pid != process_id {
            return None;
        }
        let status = fs::read_to_string(proc_path.join("status"))
            .ok()
            .and_then(|status| parse_status(&status).ok());
        let open_fds = count_fds(&proc_path.join("fd")).ok();

        Some(ProcessSample {
            pgid: stat.pgid,
            rss_bytes: stat.rss_bytes.or_else(|| status.as_ref()?.rss_bytes),
            cpu_user_ms: stat.cpu_user_ms,
            cpu_system_ms: stat.cpu_system_ms,
            threads: stat.threads.or_else(|| status.as_ref()?.threads),
            open_fds,
        })
    }

    fn count_fds(path: &Path) -> io::Result<u64> {
        let mut count = 0_u64;
        for entry in fs::read_dir(path)? {
            let _ = entry?;
            count += 1;
        }
        Ok(count)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ProcStat {
        pid: u32,
        pgid: u32,
        rss_bytes: Option<u64>,
        cpu_user_ms: Option<u64>,
        cpu_system_ms: Option<u64>,
        threads: Option<u64>,
    }

    fn parse_stat(value: &str, page_size: u64, ticks_per_second: u64) -> Result<ProcStat, String> {
        let open = value
            .find('(')
            .ok_or_else(|| "stat missing comm open paren".to_string())?;
        let close = value
            .rfind(')')
            .ok_or_else(|| "stat missing comm close paren".to_string())?;
        let pid = value[..open]
            .trim()
            .parse::<u32>()
            .map_err(|error| format!("invalid stat pid: {error}"))?;
        let fields: Vec<_> = value[close + 1..].split_whitespace().collect();
        let process_group_id = parse_field::<u32>(&fields, 2, "pgrp")?;
        let utime_ticks = parse_field::<u64>(&fields, 11, "utime").ok();
        let stime_ticks = parse_field::<u64>(&fields, 12, "stime").ok();
        let threads = parse_field::<u64>(&fields, 17, "num_threads").ok();
        let rss_pages = parse_field::<i64>(&fields, 21, "rss").ok();
        let rss_bytes = rss_pages
            .and_then(|pages| u64::try_from(pages).ok())
            .and_then(|pages| pages.checked_mul(page_size));
        Ok(ProcStat {
            pid,
            pgid: process_group_id,
            rss_bytes,
            cpu_user_ms: utime_ticks.and_then(|ticks| ticks_to_ms(ticks, ticks_per_second)),
            cpu_system_ms: stime_ticks.and_then(|ticks| ticks_to_ms(ticks, ticks_per_second)),
            threads,
        })
    }

    fn parse_field<T: std::str::FromStr>(
        fields: &[&str],
        index: usize,
        name: &str,
    ) -> Result<T, String>
    where
        T::Err: std::fmt::Display,
    {
        fields
            .get(index)
            .ok_or_else(|| format!("stat missing {name}"))?
            .parse::<T>()
            .map_err(|error| format!("invalid stat {name}: {error}"))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ProcStatus {
        rss_bytes: Option<u64>,
        threads: Option<u64>,
    }

    fn parse_status(value: &str) -> Result<ProcStatus, String> {
        let mut rss_bytes = None;
        let mut threads = None;
        for line in value.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb = rest
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| "VmRSS missing value".to_string())?
                    .parse::<u64>()
                    .map_err(|error| format!("invalid VmRSS: {error}"))?;
                rss_bytes = kb.checked_mul(1024);
            } else if let Some(rest) = line.strip_prefix("Threads:") {
                threads = Some(
                    rest.trim()
                        .parse::<u64>()
                        .map_err(|error| format!("invalid Threads: {error}"))?,
                );
            }
        }
        Ok(ProcStatus { rss_bytes, threads })
    }

    fn ticks_to_ms(ticks: u64, ticks_per_second: u64) -> Option<u64> {
        ticks.checked_mul(1_000)?.checked_div(ticks_per_second)
    }

    fn page_size_bytes() -> u64 {
        // SAFETY: sysconf is thread-safe for _SC_PAGESIZE and has no Rust-side
        // aliasing requirements.
        let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(4096)
    }

    fn ticks_per_second() -> u64 {
        // SAFETY: sysconf is thread-safe for _SC_CLK_TCK and has no Rust-side
        // aliasing requirements.
        let value = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(100)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ProcessSample {
        pgid: u32,
        rss_bytes: Option<u64>,
        cpu_user_ms: Option<u64>,
        cpu_system_ms: Option<u64>,
        threads: Option<u64>,
        open_fds: Option<u64>,
    }

    #[derive(Debug, Default)]
    struct ResourceAggregate {
        process_count: u32,
        rss_bytes: FieldSum,
        cpu_user_ms: FieldSum,
        cpu_system_ms: FieldSum,
        threads: FieldSum,
        open_fds: FieldSum,
    }

    impl ResourceAggregate {
        fn add(&mut self, process: ProcessSample) {
            self.process_count += 1;
            self.rss_bytes.add(process.rss_bytes);
            self.cpu_user_ms.add(process.cpu_user_ms);
            self.cpu_system_ms.add(process.cpu_system_ms);
            self.threads.add(process.threads);
            self.open_fds.add(process.open_fds);
        }
    }

    #[derive(Debug, Default)]
    struct FieldSum {
        value: u64,
        observed: bool,
    }

    impl FieldSum {
        fn add(&mut self, value: Option<u64>) {
            if let Some(value) = value {
                self.observed = true;
                self.value = self.value.saturating_add(value);
            }
        }

        fn finish(self) -> Option<u64> {
            if self.observed {
                Some(self.value)
            } else {
                None
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_proc_stat_with_spaces_in_comm() {
            let stat = "123 (hogd dev worker) S 1 456 456 0 -1 4194560 10 0 0 0 20 5 0 0 20 0 7 0 100 4096 25";
            let parsed = parse_stat(stat, 4096, 100).expect("stat should parse");

            assert_eq!(parsed.pid, 123);
            assert_eq!(parsed.pgid, 456);
            assert_eq!(parsed.cpu_user_ms, Some(200));
            assert_eq!(parsed.cpu_system_ms, Some(50));
            assert_eq!(parsed.threads, Some(7));
            assert_eq!(parsed.rss_bytes, Some(25 * 4096));
        }

        #[test]
        fn parses_proc_status_fallback_fields() {
            let status = "Name:\thogd-dev\nVmRSS:\t  2048 kB\nThreads:\t12\n";
            let parsed = parse_status(status).expect("status should parse");

            assert_eq!(parsed.rss_bytes, Some(2_097_152));
            assert_eq!(parsed.threads, Some(12));
        }

        #[test]
        fn aggregate_sums_available_process_fields() {
            let mut aggregate = ResourceAggregate::default();
            aggregate.add(ProcessSample {
                pgid: 1,
                rss_bytes: Some(10),
                cpu_user_ms: Some(1),
                cpu_system_ms: Some(2),
                threads: Some(3),
                open_fds: Some(4),
            });
            aggregate.add(ProcessSample {
                pgid: 1,
                rss_bytes: Some(20),
                cpu_user_ms: Some(2),
                cpu_system_ms: Some(3),
                threads: Some(4),
                open_fds: None,
            });

            assert_eq!(aggregate.rss_bytes.finish(), Some(30));
            assert_eq!(aggregate.cpu_user_ms.finish(), Some(3));
            assert_eq!(aggregate.open_fds.finish(), Some(4));
        }

        #[test]
        fn aggregate_uses_none_when_no_process_has_field() {
            let mut aggregate = ResourceAggregate::default();
            aggregate.add(ProcessSample {
                pgid: 1,
                rss_bytes: None,
                cpu_user_ms: None,
                cpu_system_ms: None,
                threads: None,
                open_fds: None,
            });

            assert_eq!(aggregate.rss_bytes.finish(), None);
            assert_eq!(aggregate.cpu_user_ms.finish(), None);
            assert_eq!(aggregate.open_fds.finish(), None);
        }
    }
}
