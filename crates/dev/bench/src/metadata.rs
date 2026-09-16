//! Separate observed binary identity from runner-supplied target configuration.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use groundhog_json_rpc_types::version::VersionInfo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BenchError,
    cli::Cli,
    corpus::{read_bounded, rpc},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "camelCase")]
pub(super) enum Observation<T> {
    Observed { value: T },
    Unavailable { reason: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct HostMetadata {
    pub(super) os: String,
    pub(super) arch: String,
    pub(super) cpu_model: Option<String>,
    pub(super) logical_cpus: Option<usize>,
    pub(super) os_release: Option<String>,
}

impl HostMetadata {
    pub(super) fn local() -> Self {
        #[cfg(target_os = "macos")]
        let cpu_model = command_output("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"]);
        #[cfg(target_os = "linux")]
        let cpu_model = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines().find_map(|line| {
                    line.strip_prefix("model name")
                        .and_then(|rest| rest.split_once(':'))
                        .map(|(_, value)| value.trim().to_owned())
                })
            });
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let cpu_model = None;
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cpu_model,
            logical_cpus: std::thread::available_parallelism().ok().map(usize::from),
            os_release: command_output("/usr/bin/uname", &["-r"]),
        }
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ComponentMetadata {
    pub(super) artifact_sha256: Option<String>,
    pub(super) commit_hash: Option<String>,
    pub(super) git_dirty: Option<bool>,
    pub(super) build_profile: Option<String>,
    pub(super) features: Option<Vec<String>>,
    pub(super) worker_threads: Option<usize>,
    pub(super) effective_config: Option<BTreeMap<String, serde_json::Value>>,
    pub(super) host: Option<HostMetadata>,
    pub(super) logging: Option<LoggingPolicy>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LoggingPolicy {
    pub(super) filter: String,
    pub(super) sink: String,
    pub(super) span_events: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TargetInfo {
    pub(super) schema_version: u32,
    pub(super) topology: String,
    pub(super) gateway: ComponentMetadata,
    pub(super) backend: ComponentMetadata,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TargetMetadata {
    pub(super) gateway_version: Observation<VersionInfo>,
    pub(super) runner_supplied: Option<TargetInfo>,
}

pub(super) async fn collect(
    cli: &Cli,
    client: &reqwest::Client,
) -> Result<TargetMetadata, BenchError> {
    let gateway_version = match rpc(client, cli, "version", serde_json::json!([])).await {
        Ok(value) => match serde_json::from_value::<VersionInfo>(value) {
            Ok(value) => Observation::Observed { value },
            Err(error) => Observation::Unavailable {
                reason: format!("invalid version response: {error}"),
            },
        },
        Err(error) => Observation::Unavailable {
            reason: error.to_string(),
        },
    };
    let runner_supplied = if let Some(path) = &cli.target_info {
        let info: TargetInfo = serde_json::from_slice(&read_bounded(path, 1024 * 1024).await?)
            .map_err(|error| {
                BenchError::InvalidConfiguration(format!("invalid target info: {error}"))
            })?;
        if info.schema_version != 1 || info.topology.trim().is_empty() {
            return Err(BenchError::InvalidConfiguration(
                "target info needs schemaVersion 1 and a topology description".into(),
            ));
        }
        if let Observation::Observed { value } = &gateway_version
            && let (Some(actual), Some(expected)) = (&value.commit_hash, &info.gateway.commit_hash)
            && actual != expected
        {
            return Err(BenchError::InvalidConfiguration(
                "runner gateway commit differs from observed version".into(),
            ));
        }
        Some(info)
    } else {
        None
    };
    Ok(TargetMetadata {
        gateway_version,
        runner_supplied,
    })
}

pub(super) fn binary_digest(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("0x{}", hex::encode(hash.finalize())))
}
