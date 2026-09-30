use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::SimError;
use crate::target::{OwnedProcess, TargetOwnership};
use crate::util::command_on_path;

pub const SWEEP_REPORT_SCHEMA_VERSION: u32 = 1;
const OWNERSHIP_FILE: &str = "target-ownership.json";
const SWEEP_CLEANUP_GRACE: Duration = Duration::from_secs(10);
const POST_KILL_DRAIN: Duration = Duration::from_millis(500);

/// Result of scanning and optionally reclaiming simulator-owned local targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepReport {
    pub schema_version: u32,
    pub root: String,
    pub entries: Vec<SweepEntry>,
}

impl SweepReport {
    /// Return whether any validated process failed to terminate.
    pub fn has_failures(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.state == SweepEntryState::Failed)
    }
}

/// Per-run sweeper classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepEntry {
    pub run_dir: String,
    pub state: SweepEntryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SweepEntry {
    fn reclaimed(run_dir: &Path, process: &OwnedProcess) -> Self {
        Self {
            run_dir: run_dir.display().to_string(),
            state: SweepEntryState::Reclaimed,
            pid: Some(process.pid),
            port: Some(process.port),
            reason: None,
            error: None,
        }
    }

    fn skipped(run_dir: &Path, reason: impl Into<String>) -> Self {
        Self {
            run_dir: run_dir.display().to_string(),
            state: SweepEntryState::Skipped,
            pid: None,
            port: None,
            reason: Some(reason.into()),
            error: None,
        }
    }

    fn dry_run(run_dir: &Path, process: &OwnedProcess) -> Self {
        Self {
            run_dir: run_dir.display().to_string(),
            state: SweepEntryState::Skipped,
            pid: Some(process.pid),
            port: Some(process.port),
            reason: Some("dry run: would reclaim".into()),
            error: None,
        }
    }

    fn failed(run_dir: &Path, process: &OwnedProcess, error: impl Into<String>) -> Self {
        Self {
            run_dir: run_dir.display().to_string(),
            state: SweepEntryState::Failed,
            pid: Some(process.pid),
            port: Some(process.port),
            reason: None,
            error: Some(error.into()),
        }
    }
}

/// Sweeper entry state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SweepEntryState {
    Reclaimed,
    Skipped,
    Failed,
}

/// Live process identity read from the operating system during validation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveProcess {
    start_time: String,
    pgid: u32,
    command: String,
}

/// Validation result for one recorded process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessValidation {
    Valid,
    Skipped(String),
}

/// Why a run directory's ownership record could not be loaded.
#[derive(Debug, thiserror::Error)]
enum OwnershipReadError {
    #[error("{0}")]
    Read(#[from] io::Error),
    #[error("not a valid ownership record: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Why the sweeper could not inspect or reclaim a recorded process.
#[derive(Debug, thiserror::Error)]
enum SweepOpError {
    /// `ps` could not be run.
    #[error("failed to run `ps`: {0}")]
    Ps(#[source] io::Error),
    /// `ps` ran but its output did not carry what validation compares.
    #[error("ps output did not include {field}")]
    PsOutputMissing { field: &'static str },
    /// A recorded or reported process group is not a group this can signal.
    #[error("invalid process group `{value}`")]
    ProcessGroup { value: String },
    /// Signalling the process group failed for a reason other than "already gone".
    #[error("failed to signal process group: {0}")]
    Signal(#[source] io::Error),
    /// The group outlived `SIGKILL` plus the drain window.
    #[error("process group {pgid} still alive after SIGKILL")]
    SurvivedKill { pgid: u32 },
    /// Process reclamation has no implementation on this platform.
    #[cfg(not(unix))]
    #[error("sweep is only supported on Unix targets")]
    UnsupportedPlatform,
}

trait SweepOps {
    fn ps_available(&self) -> bool;
    fn inspect_process(&self, pid: u32) -> Result<Option<LiveProcess>, SweepOpError>;
    fn terminate_process_group(&self, process: &OwnedProcess) -> Result<(), SweepOpError>;
}

struct RealSweepOps;

/// Scan `root` for simulator-owned local targets and optionally reclaim them.
pub fn sweep_runs(root: &Path, dry_run: bool) -> Result<SweepReport, SimError> {
    sweep_runs_with_ops(root, dry_run, &RealSweepOps)
}

fn sweep_runs_with_ops(
    root: &Path,
    dry_run: bool,
    ops: &dyn SweepOps,
) -> Result<SweepReport, SimError> {
    let mut report = SweepReport {
        schema_version: SWEEP_REPORT_SCHEMA_VERSION,
        root: root.display().to_string(),
        entries: Vec::new(),
    };
    if !root.exists() {
        return Ok(report);
    }

    let mut run_dirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.entries.push(SweepEntry::skipped(
                    root,
                    format!("directory entry unreadable: {error}"),
                ));
                continue;
            }
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => run_dirs.push(path),
            Ok(_) => {}
            Err(error) => report.entries.push(SweepEntry::skipped(
                &path,
                format!("directory entry unreadable: {error}"),
            )),
        }
    }
    run_dirs.sort();

    let ps_available = ops.ps_available();
    for run_dir in run_dirs {
        let ownership_path = run_dir.join(OWNERSHIP_FILE);
        if !ownership_path.exists() {
            continue;
        }
        let ownership = match read_ownership(&ownership_path) {
            Ok(ownership) => ownership,
            Err(error) => {
                report.entries.push(SweepEntry::skipped(
                    &run_dir,
                    format!("ownership unreadable: {error}"),
                ));
                continue;
            }
        };
        let Some(process) = reclaimable_process(&ownership) else {
            report.entries.push(SweepEntry::skipped(
                &run_dir,
                "not a reclaimable simulator-owned local target",
            ));
            continue;
        };
        if !ps_available {
            report
                .entries
                .push(SweepEntry::skipped(&run_dir, "`ps` not available"));
            continue;
        }
        match validate_process(process, ops) {
            Ok(ProcessValidation::Valid) if dry_run => {
                report.entries.push(SweepEntry::dry_run(&run_dir, process));
            }
            Ok(ProcessValidation::Valid) => match ops.terminate_process_group(process) {
                Ok(()) => report
                    .entries
                    .push(SweepEntry::reclaimed(&run_dir, process)),
                Err(error) => {
                    report
                        .entries
                        .push(SweepEntry::failed(&run_dir, process, error.to_string()));
                }
            },
            Ok(ProcessValidation::Skipped(reason)) => {
                report.entries.push(SweepEntry::skipped(&run_dir, reason));
            }
            Err(error) => {
                report.entries.push(SweepEntry::skipped(
                    &run_dir,
                    format!("process validation failed: {error}"),
                ));
            }
        }
    }

    Ok(report)
}

fn read_ownership(path: &Path) -> Result<TargetOwnership, OwnershipReadError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn reclaimable_process(ownership: &TargetOwnership) -> Option<&OwnedProcess> {
    if ownership.started_by_sim && ownership.cleanup_allowed {
        ownership.process.as_ref()
    } else {
        None
    }
}

fn validate_recorded_process(
    recorded: &OwnedProcess,
    live: Option<&LiveProcess>,
) -> ProcessValidation {
    let Some(live) = live else {
        return ProcessValidation::Skipped("pid not alive".into());
    };
    if live.start_time != recorded.start_time {
        return ProcessValidation::Skipped("pid start time mismatch".into());
    }
    if live.pgid != recorded.pgid {
        return ProcessValidation::Skipped("process group mismatch".into());
    }
    if !live.command.contains(&recorded.command) {
        return ProcessValidation::Skipped("command mismatch".into());
    }
    ProcessValidation::Valid
}

fn validate_process(
    recorded: &OwnedProcess,
    ops: &dyn SweepOps,
) -> Result<ProcessValidation, SweepOpError> {
    let live = ops.inspect_process(recorded.pid)?;
    Ok(validate_recorded_process(recorded, live.as_ref()))
}

impl SweepOps for RealSweepOps {
    fn ps_available(&self) -> bool {
        command_on_path("ps")
    }

    fn inspect_process(&self, process_id: u32) -> Result<Option<LiveProcess>, SweepOpError> {
        read_ps_snapshot(process_id)
    }

    fn terminate_process_group(&self, process: &OwnedProcess) -> Result<(), SweepOpError> {
        terminate_process_group(process, SWEEP_CLEANUP_GRACE)
    }
}

fn read_ps_snapshot(pid: u32) -> Result<Option<LiveProcess>, SweepOpError> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=,pgid=,command="])
        .output()
        .map_err(SweepOpError::Ps)?;
    if !output.status.success() {
        // A non-zero `ps` exit is treated as "gone". That may hide a `ps`
        // malfunction as a skip, but it keeps sweep firmly in the no-wrong-kill
        // direction.
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&output.stdout);
    parse_ps_snapshot_line(line.lines().next().unwrap_or_default()).map(Some)
}

fn parse_ps_snapshot_line(line: &str) -> Result<LiveProcess, SweepOpError> {
    let mut parts = line.split_whitespace();
    let mut start_tokens = Vec::with_capacity(5);
    for _ in 0..5 {
        let Some(token) = parts.next() else {
            return Err(SweepOpError::PsOutputMissing {
                field: "a full lstart token",
            });
        };
        start_tokens.push(token);
    }
    let process_group = parts.next().ok_or(SweepOpError::PsOutputMissing {
        field: "a process group",
    })?;
    let pgid = process_group
        .parse::<u32>()
        .map_err(|_| SweepOpError::ProcessGroup {
            value: process_group.to_owned(),
        })?;
    let command = parts.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return Err(SweepOpError::PsOutputMissing { field: "a command" });
    }
    Ok(LiveProcess {
        start_time: start_tokens.join(" "),
        pgid,
        command,
    })
}

#[cfg(unix)]
fn terminate_process_group(process: &OwnedProcess, grace: Duration) -> Result<(), SweepOpError> {
    signal_process_group(process.pgid, libc::SIGTERM)?;
    let deadline = Instant::now() + grace;
    loop {
        if !process_group_exists(process.pgid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    signal_process_group(process.pgid, libc::SIGKILL)?;
    let deadline = Instant::now() + POST_KILL_DRAIN;
    loop {
        if !process_group_exists(process.pgid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SweepOpError::SurvivedKill { pgid: process.pgid });
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_process: &OwnedProcess, _grace: Duration) -> Result<(), SweepOpError> {
    Err(SweepOpError::UnsupportedPlatform)
}

#[cfg(unix)]
fn signal_process_group(pgid: u32, signal: libc::c_int) -> Result<(), SweepOpError> {
    let group = i32::try_from(pgid)
        .ok()
        .filter(|group| *group > 0)
        .ok_or_else(|| SweepOpError::ProcessGroup {
            value: pgid.to_string(),
        })?;
    let rc = unsafe { libc::kill(-group, signal) };
    if rc == 0 {
        Ok(())
    } else {
        let source = io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(SweepOpError::Signal(source))
        }
    }
}

#[cfg(unix)]
fn process_group_exists(pgid: u32) -> bool {
    let Some(pgid) = i32::try_from(pgid)
        .ok()
        .filter(|process_group| *process_group > 0)
    else {
        return false;
    };
    let rc = unsafe { libc::kill(-pgid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::target::TargetProviderKind;

    #[test]
    fn validate_recorded_process_requires_start_time_pgid_and_command() {
        let recorded = owned_process();
        let live = LiveProcess {
            start_time: recorded.start_time.clone(),
            pgid: recorded.pgid,
            command: "/tmp/groundhog node --json".into(),
        };

        assert_eq!(
            validate_recorded_process(&recorded, Some(&live)),
            ProcessValidation::Valid
        );
        assert_eq!(
            validate_recorded_process(&recorded, None),
            ProcessValidation::Skipped("pid not alive".into())
        );
        assert_eq!(
            validate_recorded_process(
                &recorded,
                Some(&LiveProcess {
                    start_time: "different".into(),
                    ..live.clone()
                })
            ),
            ProcessValidation::Skipped("pid start time mismatch".into())
        );
        assert_eq!(
            validate_recorded_process(
                &recorded,
                Some(&LiveProcess {
                    pgid: 999,
                    ..live.clone()
                })
            ),
            ProcessValidation::Skipped("process group mismatch".into())
        );
        assert_eq!(
            validate_recorded_process(
                &recorded,
                Some(&LiveProcess {
                    command: "/bin/sleep 100".into(),
                    ..live
                })
            ),
            ProcessValidation::Skipped("command mismatch".into())
        );
    }

    #[test]
    fn parses_single_ps_snapshot_with_lstart_pgid_and_command() {
        let snapshot =
            parse_ps_snapshot_line("Wed May  7 12:00:00 2026  5678 /tmp/groundhog node --json")
                .expect("ps snapshot should parse");

        assert_eq!(snapshot.start_time, "Wed May 7 12:00:00 2026");
        assert_eq!(snapshot.pgid, 5678);
        assert_eq!(snapshot.command, "/tmp/groundhog node --json");
    }

    #[test]
    fn sweep_report_serializes_flat_json_shape() {
        let report = SweepReport {
            schema_version: SWEEP_REPORT_SCHEMA_VERSION,
            root: "hogsim-runs".into(),
            entries: vec![
                SweepEntry::reclaimed(Path::new("hogsim-runs/a"), &owned_process()),
                SweepEntry::skipped(Path::new("hogsim-runs/b"), "pid not alive"),
            ],
        };

        let value = serde_json::to_value(report).expect("report should serialize");

        assert_eq!(value["schemaVersion"], json!(1));
        assert_eq!(value["root"], json!("hogsim-runs"));
        assert_eq!(value["entries"][0]["runDir"], json!("hogsim-runs/a"));
        assert_eq!(value["entries"][0]["state"], json!("reclaimed"));
        assert_eq!(value["entries"][0]["pid"], json!(1234));
        assert_eq!(value["entries"][0]["port"], json!(9545));
        assert_eq!(value["entries"][1]["state"], json!("skipped"));
        assert_eq!(value["entries"][1]["reason"], json!("pid not alive"));
        assert!(value["entries"][1].get("error").is_none());
    }

    #[test]
    fn scan_classifies_mixed_run_directories() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let root = temp.path();
        let existing = root.join("existing");
        let invalid = root.join("invalid");
        let dead = root.join("dead");
        let live = root.join("live");
        let non_run = root.join("non-run");
        fs::create_dir_all(&existing).expect("existing dir should create");
        fs::create_dir_all(&invalid).expect("invalid dir should create");
        fs::create_dir_all(&dead).expect("dead dir should create");
        fs::create_dir_all(&live).expect("live dir should create");
        fs::create_dir_all(&non_run).expect("non-run dir should create");
        fs::write(
            existing.join(OWNERSHIP_FILE),
            serde_json::to_vec(&TargetOwnership::existing()).expect("ownership should serialize"),
        )
        .expect("existing ownership should write");
        fs::write(invalid.join(OWNERSHIP_FILE), b"{not-json")
            .expect("invalid ownership should write");
        write_ownership(&dead, owned_process_with_pid(1234));
        write_ownership(&live, owned_process_with_pid(5678));

        let ops = FakeSweepOps {
            ps_available: true,
            processes: BTreeMap::from([(
                5678,
                LiveProcess {
                    start_time: "Wed May 27 12:00:00 2026".into(),
                    pgid: 5678,
                    command: "/tmp/groundhog node --json".into(),
                },
            )]),
            terminated: RefCell::new(Vec::new()),
            survives_kill: None,
        };

        let report = sweep_runs_with_ops(root, false, &ops).expect("sweep should scan");
        let states: Vec<_> = report
            .entries
            .iter()
            .map(|entry| (entry.run_dir.as_str(), entry.state, entry.reason.as_deref()))
            .collect();

        assert_eq!(report.schema_version, SWEEP_REPORT_SCHEMA_VERSION);
        assert_eq!(states.len(), 4);
        assert!(
            report
                .entries
                .iter()
                .all(|entry| !entry.run_dir.ends_with("non-run"))
        );
        assert!(states.iter().any(|(_, state, reason)| {
            *state == SweepEntryState::Skipped
                && *reason == Some("not a reclaimable simulator-owned local target")
        }));
        assert!(states.iter().any(|(_, state, reason)| {
            *state == SweepEntryState::Skipped
                && reason.is_some_and(|reason| reason.starts_with("ownership unreadable:"))
        }));
        assert!(states.iter().any(|(_, state, reason)| {
            *state == SweepEntryState::Skipped && *reason == Some("pid not alive")
        }));
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.state == SweepEntryState::Reclaimed
                    && entry.pid == Some(5678)
                    && entry.port == Some(9545))
        );
        assert_eq!(&*ops.terminated.borrow(), &[5678]);
    }

    #[test]
    fn sweep_skips_candidates_when_ps_is_missing() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir should create");
        write_ownership(&run_dir, owned_process());
        let ops = FakeSweepOps {
            ps_available: false,
            processes: BTreeMap::new(),
            terminated: RefCell::new(Vec::new()),
            survives_kill: None,
        };

        let report = sweep_runs_with_ops(temp.path(), false, &ops).expect("sweep should classify");

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].state, SweepEntryState::Skipped);
        assert_eq!(
            report.entries[0].reason.as_deref(),
            Some("`ps` not available")
        );
        assert!(ops.terminated.borrow().is_empty());
    }

    #[test]
    fn failed_termination_is_reported_per_entry() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir should create");
        let process = owned_process();
        write_ownership(&run_dir, process.clone());
        let ops = FakeSweepOps {
            ps_available: true,
            processes: BTreeMap::from([(
                process.pid,
                LiveProcess {
                    start_time: process.start_time.clone(),
                    pgid: process.pgid,
                    command: "/tmp/groundhog node --json".into(),
                },
            )]),
            terminated: RefCell::new(Vec::new()),
            survives_kill: Some(process.pgid),
        };

        let report = sweep_runs_with_ops(temp.path(), false, &ops).expect("sweep should classify");

        assert!(report.has_failures());
        assert_eq!(report.entries[0].state, SweepEntryState::Failed);
        assert_eq!(
            report.entries[0].error.as_deref(),
            Some(format!("process group {} still alive after SIGKILL", process.pgid).as_str())
        );
    }

    fn write_ownership(run_dir: &Path, process: OwnedProcess) {
        let ownership = TargetOwnership {
            provider: TargetProviderKind::Local,
            started_by_sim: true,
            cleanup_allowed: true,
            process: Some(process),
        };
        fs::write(
            run_dir.join(OWNERSHIP_FILE),
            serde_json::to_vec(&ownership).expect("ownership should serialize"),
        )
        .expect("ownership should write");
    }

    fn owned_process() -> OwnedProcess {
        owned_process_with_pid(1234)
    }

    fn owned_process_with_pid(pid: u32) -> OwnedProcess {
        OwnedProcess {
            pid,
            pgid: pid,
            port: 9545,
            start_time: "Wed May 27 12:00:00 2026".into(),
            command: "groundhog node".into(),
        }
    }

    struct FakeSweepOps {
        ps_available: bool,
        processes: BTreeMap<u32, LiveProcess>,
        terminated: RefCell<Vec<u32>>,
        /// Process group the fake reports as surviving `SIGKILL`.
        survives_kill: Option<u32>,
    }

    impl SweepOps for FakeSweepOps {
        fn ps_available(&self) -> bool {
            self.ps_available
        }

        fn inspect_process(&self, pid: u32) -> Result<Option<LiveProcess>, SweepOpError> {
            Ok(self.processes.get(&pid).cloned())
        }

        fn terminate_process_group(&self, process: &OwnedProcess) -> Result<(), SweepOpError> {
            self.terminated.borrow_mut().push(process.pid);
            match self.survives_kill {
                Some(pgid) => Err(SweepOpError::SurvivedKill { pgid }),
                None => Ok(()),
            }
        }
    }
}
