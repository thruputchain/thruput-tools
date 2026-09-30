//! Best-effort server observations. Actual sample offsets expose boundary skew.
use crate::metadata::Observation;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

type Metrics = BTreeMap<String, f64>;

pub(super) struct Config {
    pub metrics_url: Option<String>,
    pub gateway_pid: Option<u32>,
    pub interval_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerObservations {
    interval_ms: u64,
    scope: &'static str,
    reset_detection: &'static str,
    samples: Vec<Sample>,
    counter_deltas: Observation<Metrics>,
    gauge_peaks: Observation<Metrics>,
    cpu_seconds: Observation<f64>,
    rss_peak_bytes: Observation<u64>,
    internal_connection_replacements: Observation<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    started_offset_ms: f64,
    completed_offset_ms: f64,
    metrics: Observation<Metrics>,
    process: Observation<Process>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Process {
    start_ticks: u64,
    cpu_seconds: f64,
    rss_bytes: u64,
}

fn unavailable<T>(reason: impl Into<String>) -> Observation<T> {
    Observation::Unavailable {
        reason: reason.into(),
    }
}

pub(super) async fn collect(config: Config, start: Instant, end: Instant) -> ServerObservations {
    let client = reqwest::Client::new();
    let interval = Duration::from_millis(config.interval_ms);
    let mut samples = Vec::new();
    let mut next = start;
    loop {
        tokio::time::sleep_until(next.into()).await;
        let started = Instant::now();
        let metrics = match &config.metrics_url {
            Some(url) => {
                match tokio::time::timeout(Duration::from_secs(1), scrape(&client, url)).await {
                    Ok(Ok(value)) => Observation::Observed { value },
                    Ok(Err(error)) => unavailable(error),
                    Err(_) => unavailable("metrics scrape timed out"),
                }
            }
            None => unavailable("--metrics-url not supplied"),
        };
        let process = match config.gateway_pid {
            Some(pid) => match process_sample(pid) {
                Ok(value) => Observation::Observed { value },
                Err(error) => unavailable(error),
            },
            None => unavailable("--gateway-pid not supplied"),
        };
        samples.push(Sample {
            started_offset_ms: started.duration_since(start).as_secs_f64() * 1000.0,
            completed_offset_ms: start.elapsed().as_secs_f64() * 1000.0,
            metrics,
            process,
        });
        if started >= end {
            break;
        }
        next = (next + interval).max(Instant::now()).min(end);
    }
    let (mut counter_deltas, gauge_peaks) = metric_summary(&samples);
    let (cpu_seconds, rss_peak_bytes) = process_summary(&samples);
    if config.gateway_pid.is_some() && matches!(cpu_seconds, Observation::Unavailable { .. }) {
        counter_deltas =
            unavailable("gateway process continuity could not be verified; see process samples");
    }
    ServerObservations {
        interval_ms: config.interval_ms,
        reset_detection: "counter decreases/disappearance and supplied local PID continuity; metrics-only restarts that catch up between samples cannot be detected",
        scope: "gateway-only; samples bracket measured window; offsets include scrape latency; warmup and drain excluded except boundary skew",
        samples,
        counter_deltas,
        gauge_peaks,
        cpu_seconds,
        rss_peak_bytes,
        internal_connection_replacements: unavailable(
            "rpc_driver internal replacements have no gateway metric hook",
        ),
    }
}

fn is_counter(key: &str) -> bool {
    key.starts_with("groundhog_gateway_backend_requests_total{")
        || key.starts_with("groundhog_gateway_backend_reconnects_total{")
}
fn is_gauge(key: &str) -> bool {
    key.starts_with("groundhog_gateway_rpc_in_flight{")
        || key.starts_with("groundhog_gateway_internal_batch_in_flight{")
}

async fn scrape(client: &reqwest::Client, url: &str) -> Result<Metrics, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err("metrics response exceeds 4 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_metrics(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
}

fn parse_metrics(text: &str) -> Result<Metrics, String> {
    let mut values = Metrics::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let Some((key, raw)) = line.rsplit_once(' ') else {
            continue;
        };
        if !is_counter(key) && !is_gauge(key) {
            continue;
        }
        let value: f64 = raw.parse().map_err(|_| "invalid metric value")?;
        if !value.is_finite() || value < 0.0 {
            return Err("invalid negative/nonfinite metric".into());
        }
        if values.insert(key.to_owned(), value).is_some() {
            return Err("duplicate metric series".into());
        }
    }
    if values.is_empty() {
        return Err("no supported gateway series in scrape".into());
    }
    Ok(values)
}

fn metric_summary(samples: &[Sample]) -> (Observation<Metrics>, Observation<Metrics>) {
    let maps: Result<Vec<_>, _> = samples
        .iter()
        .map(|s| match &s.metrics {
            Observation::Observed { value } => Ok(value),
            Observation::Unavailable { reason } => Err(reason.clone()),
        })
        .collect();
    let maps = match maps {
        Ok(maps) => maps,
        Err(reason) => return (unavailable(reason.clone()), unavailable(reason)),
    };
    let mut deltas = Metrics::new();
    let mut peaks = Metrics::new();
    for map in &maps {
        for (key, value) in *map {
            if is_gauge(key) {
                peaks
                    .entry(key.clone())
                    .and_modify(|v| *v = v.max(*value))
                    .or_insert(*value);
            }
        }
    }
    let gauges = if peaks.is_empty() {
        unavailable("no in-flight gauge series observed")
    } else {
        Observation::Observed { value: peaks }
    };
    for pair in maps.windows(2) {
        for (key, before) in pair[0].iter().filter(|(k, _)| is_counter(k)) {
            if pair[1].get(key).is_none_or(|after| after < before) {
                return (unavailable("counter reset or series disappeared"), gauges);
            }
        }
        // Lazily registered label series start at zero when first observed.
        for (key, after) in pair[1].iter().filter(|(k, _)| is_counter(k)) {
            *deltas.entry(key.clone()).or_default() +=
                after - pair[0].get(key).copied().unwrap_or(0.0);
        }
    }
    let counters = if deltas.is_empty() {
        unavailable("no backend counter series observed")
    } else {
        Observation::Observed { value: deltas }
    };
    (counters, gauges)
}

fn process_summary(samples: &[Sample]) -> (Observation<f64>, Observation<u64>) {
    let mut first: Option<&Process> = None;
    let mut last = 0.0;
    let mut peak = 0;
    for sample in samples {
        let value = match &sample.process {
            Observation::Observed { value } => value,
            Observation::Unavailable { reason } => {
                return (unavailable(reason.clone()), unavailable(reason.clone()));
            }
        };
        if let Some(first) = first {
            if value.start_ticks != first.start_ticks || value.cpu_seconds < last {
                return (
                    unavailable("gateway process reset"),
                    unavailable("gateway process reset"),
                );
            }
        } else {
            first = Some(value);
        }
        last = value.cpu_seconds;
        peak = peak.max(value.rss_bytes);
    }
    match first {
        Some(first) => (
            Observation::Observed {
                value: last - first.cpu_seconds,
            },
            Observation::Observed { value: peak },
        ),
        None => (
            unavailable("no process samples"),
            unavailable("no process samples"),
        ),
    }
}

#[cfg(not(target_os = "linux"))]
fn process_sample(_pid: u32) -> Result<Process, String> {
    Err("gateway process sampling requires Linux /proc".into())
}

#[cfg(target_os = "linux")]
fn process_sample(pid: u32) -> Result<Process, String> {
    use std::sync::OnceLock;
    static UNITS: OnceLock<Result<(f64, u64), String>> = OnceLock::new();
    let (ticks, page) = UNITS
        .get_or_init(|| {
            fn unit(name: &str) -> Result<u64, String> {
                let output = std::process::Command::new("getconf")
                    .arg(name)
                    .output()
                    .map_err(|e| e.to_string())?;
                if !output.status.success() {
                    return Err(format!("getconf {name} failed"));
                }
                let value = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?;
                if value == 0 {
                    return Err("invalid process units".into());
                }
                Ok(value)
            }
            Ok((unit("CLK_TCK")? as f64, unit("PAGESIZE")?))
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    parse_stat(&text, *ticks, *page)
}

#[cfg(any(target_os = "linux", test))]
fn parse_stat(text: &str, ticks: f64, page: u64) -> Result<Process, String> {
    let (_, fields) = text.rsplit_once(')').ok_or("malformed proc stat")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let number = |index: usize| -> Result<u64, String> {
        fields
            .get(index)
            .ok_or("short proc stat")?
            .parse()
            .map_err(|_| "invalid proc stat number".into())
    };
    Ok(Process {
        start_ticks: number(19)?,
        cpu_seconds: (number(11)? as f64 + number(12)? as f64) / ticks,
        rss_bytes: number(21)?.checked_mul(page).ok_or("RSS overflow")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stat_handles_parentheses_in_command() {
        let mut fields = vec!["0"; 22];
        fields[0] = "S";
        fields[11] = "100";
        fields[12] = "50";
        fields[19] = "123";
        fields[21] = "42";
        let value = parse_stat(&format!("3 (a ) b) {}", fields.join(" ")), 100.0, 4096).unwrap();
        assert_eq!(value.cpu_seconds, 1.5);
        assert_eq!(value.rss_bytes, 42 * 4096);
        assert_eq!(value.start_ticks, 123);
    }
    #[test]
    fn counters_are_deltas_but_gauges_are_peaks() {
        let counter = "groundhog_gateway_backend_requests_total{backend=\"indexer\"}";
        let gauge = "groundhog_gateway_rpc_in_flight{method=\"receipt\"}";
        let samples: Vec<_> = [(10, 40), (25, 70), (30, 2)]
            .into_iter()
            .map(|(n, g)| Sample {
                started_offset_ms: 0.0,
                completed_offset_ms: 0.0,
                metrics: Observation::Observed {
                    value: parse_metrics(&format!("{counter} {n}\n{gauge} {g}")).unwrap(),
                },
                process: unavailable("test"),
            })
            .collect();
        let (Observation::Observed { value: deltas }, Observation::Observed { value: peaks }) =
            metric_summary(&samples)
        else {
            panic!("observed samples")
        };
        assert_eq!(deltas[counter], 20.0);
        assert_eq!(peaks[gauge], 70.0);
    }

    #[test]
    fn reset_is_not_a_negative_delta() {
        let key = "groundhog_gateway_backend_requests_total{backend=\"scheduler\"}";
        let sample = |n| Sample {
            started_offset_ms: 0.0,
            completed_offset_ms: 0.0,
            metrics: Observation::Observed {
                value: parse_metrics(&format!("{key} {n}")).unwrap(),
            },
            process: unavailable("test"),
        };
        let (counter, gauge) = metric_summary(&[sample(9), sample(2)]);
        assert!(matches!(counter, Observation::Unavailable { .. }));
        assert!(matches!(gauge, Observation::Unavailable { .. }));
    }
}
