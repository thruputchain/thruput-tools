//! Read the installed Thruput skill and manage it through the maintained installer.

use crate::{
    CliError, Print,
    result::{CommandError, CommandResult},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const SKILL_DIRECTORY: &str = ".agents/skills/thruput";
const SKILLS_PACKAGE: &str = "skills@1.7.0";
const REPOSITORY: &str = "thruputchain/agent-skills";

/// Errors reading or installing the Thruput skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("unknown skill topic `{0}`; run `thruput skill list`")]
    Topic(String),
    #[error(
        "Thruput skill files are not installed; run `thruput skill install` in this directory or `thruput skill install --global`"
    )]
    MissingInstallation,
    #[error(
        "cannot read installed skill file {path}: {source}; check the installation or run `thruput skill install` again"
    )]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("skill reference must be a Markdown file inside the installed skill: {0}")]
    InvalidReference(PathBuf),
    #[error("{0} is not on PATH; install Node.js 22.20 or newer with npm, and Git")]
    MissingTool(&'static str),
    #[error("Node.js 22.20 or newer is required; installed version is {0}")]
    NodeVersion(String),
    #[error(
        "Thruput is not installed from thruputchain/agent-skills in this scope; run `thruput skill install` with the same scope"
    )]
    NotInstalled,
    #[error("cannot read the skills installation report: {0}")]
    InstallerReport(serde_json::Error),
    #[error(
        "skills did not confirm a global Thruput refresh; check repository access and retry `thruput skill update --global`"
    )]
    UpdateUnconfirmed,
    #[error("skill installation and updates require network access; remove --offline")]
    Offline,
    #[error("unattended skill setup requires --yes and, for installation, --agent <name>")]
    Unattended,
    #[error("invalid agent ID `{0}`; use a skills agent name such as opencode or claude-code")]
    InvalidAgent(String),
    #[error("skill setup was cancelled")]
    Cancelled,
    #[error(
        "skills exited with status {0:?}; check its diagnostic output and repository authentication"
    )]
    Failed(Option<i32>),
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ReadReport {
    schema_version: u32,
    scope: Option<&'static str>,
    path: Option<PathBuf>,
    topics: Vec<String>,
    content: Option<String>,
}

/// Print an installed skill file unchanged, or list its reference names.
/// Reads the current directory's installation before the user's global installation;
/// `global` selects only the latter. Reading never invokes the installer or the network.
pub fn show(
    topic: Option<&str>,
    global: bool,
    list: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let mut report = ReadReport {
        schema_version: 1,
        scope: None,
        path: None,
        topics: Vec::new(),
        content: None,
    };
    let result = (|| -> Result<(), CliError> {
        let (root, scope) = installed_directory(global)?;
        report.scope = Some(scope);
        report.path = Some(root.clone());
        if list {
            report.topics = reference_names(&root)?;
        } else {
            let relative = match topic {
                None | Some("SKILL.md") => PathBuf::from("SKILL.md"),
                Some(topic) => {
                    let name = topic.strip_prefix("references/").unwrap_or(topic);
                    let name = name.strip_suffix(".md").unwrap_or(name);
                    if !valid_reference_name(name) {
                        return Err(SkillError::Topic(topic.into()).into());
                    }
                    PathBuf::from(format!("references/{name}.md"))
                }
            };
            let path = match installed_file(&root, &relative) {
                Err(CliError::Skill(SkillError::Read { source, .. }))
                    if topic.is_some() && source.kind() == io::ErrorKind::NotFound =>
                {
                    return Err(SkillError::Topic(topic.unwrap_or_default().into()).into());
                }
                result => result?,
            };
            report.content =
                Some(
                    fs::read_to_string(&path).map_err(|source| SkillError::Read {
                        path: path.clone(),
                        source,
                    })?,
                );
            report.path = Some(path);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            if print.is_json() {
                print.json(&CommandResult::new(cmd, report))?;
            } else if let Some(content) = &report.content {
                print.raw(content)?;
            } else {
                print.raw(&(report.topics.join("\n") + "\n"))?;
            }
            Ok(())
        }
        Err(error) => reported(error, report, cmd, print),
    }
}

fn installed_directory(global: bool) -> Result<(PathBuf, &'static str), CliError> {
    let mut candidates = Vec::new();
    if !global {
        candidates.push((std::env::current_dir()?.join(SKILL_DIRECTORY), "project"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        candidates.push((PathBuf::from(home).join(SKILL_DIRECTORY), "global"));
    }
    for (path, scope) in candidates {
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(SkillError::Read { path, source }.into()),
            Ok(_) => {}
        }
        let root = path
            .canonicalize()
            .map_err(|source| SkillError::Read { path, source })?;
        installed_file(&root, Path::new("SKILL.md"))?;
        return Ok((root, scope));
    }
    Err(SkillError::MissingInstallation.into())
}

fn installed_file(root: &Path, relative: &Path) -> Result<PathBuf, CliError> {
    let path = root.join(relative);
    let resolved = path.canonicalize().map_err(|source| SkillError::Read {
        path: path.clone(),
        source,
    })?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(SkillError::InvalidReference(path).into());
    }
    Ok(resolved)
}

fn valid_reference_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn reference_names(root: &Path) -> Result<Vec<String>, CliError> {
    let path = root.join("references");
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(SkillError::Read { path, source }.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SkillError::Read {
            path: path.clone(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".md")) else {
            continue;
        };
        if valid_reference_name(name) {
            installed_file(root, &Path::new("references").join(entry.file_name()))?;
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Installation scope and explicit consent passed to the maintained skills CLI.
#[derive(Debug, Default)]
pub struct InstallOptions {
    /// Use user-wide installation instead of the current project.
    pub global: bool,
    /// Installer agent IDs; empty permits interactive selection.
    pub agents: Vec<String>,
    /// Explicit consent to execute npm and modify installed skill files.
    pub yes: bool,
    /// Reject network-dependent setup before invoking npm.
    pub offline: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct InstallReport<'a> {
    schema_version: u32,
    source: &'static str,
    installer: &'static str,
    scope: &'static str,
    agents: &'a [String],
}

fn reported<T: Serialize>(
    error: CliError,
    report: T,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if print.is_json() {
        print.json(&CommandResult::failure_with_error(
            cmd,
            report,
            CommandError::from_cli_error(&error),
        ))?;
        Err(CliError::CommandReportedFailure)
    } else {
        Err(error)
    }
}

/// Install or update only Thruput, using the latest maintained source.
pub fn install(
    update: bool,
    options: &InstallOptions,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let report = InstallReport {
        schema_version: 1,
        source: REPOSITORY,
        installer: SKILLS_PACKAGE,
        scope: if installer_selects_scope(update, options, print) {
            "interactive"
        } else if options.global {
            "global"
        } else {
            "project"
        },
        agents: &options.agents,
    };
    match run_installer(update, options, print) {
        Ok(()) => {
            print.success(if update {
                "Thruput skill update complete."
            } else {
                "Thruput skill is installed."
            });
            print.json(&CommandResult::new(cmd, report))
        }
        Err(error) => reported(error, report, cmd, print),
    }
}

fn installer_selects_scope(update: bool, options: &InstallOptions, print: &Print) -> bool {
    !update && !options.global && !options.yes && print.can_prompt()
}

fn run_installer(update: bool, options: &InstallOptions, print: &Print) -> Result<(), CliError> {
    if options.offline {
        return Err(SkillError::Offline.into());
    }
    if !print.can_prompt() && (!options.yes || (!update && options.agents.is_empty())) {
        return Err(SkillError::Unattended.into());
    }
    for agent in &options.agents {
        if agent != "*"
            && (agent.is_empty()
                || agent.starts_with('-')
                || !agent
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
        {
            return Err(SkillError::InvalidAgent(agent.clone()).into());
        }
    }
    for tool in ["node", "npm", "npx", "git"] {
        which::which(tool).map_err(|_| SkillError::MissingTool(tool))?;
    }
    let version = Command::new("node")
        .arg("--version")
        .stdin(Stdio::null())
        .output()?;
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    let mut parts = version
        .trim_start_matches('v')
        .split('.')
        .map(str::parse::<u32>);
    if !matches!((parts.next(), parts.next()), (Some(Ok(major)), Some(Ok(minor))) if major > 22 || (major == 22 && minor >= 20))
    {
        return Err(SkillError::NodeVersion(version).into());
    }
    if !print.confirm(
        options.yes,
        "skill setup",
        "Run npx skills to install or update Thruput guidance?",
    )? {
        return Err(SkillError::Cancelled.into());
    }
    if update {
        require_installed(options.global)?;
        if options.global {
            return refresh_global();
        }
    }
    let mut child = Command::new("npx");
    child.args(["--yes", SKILLS_PACKAGE]);
    if update {
        child.args(["update", "thruput", "--project"]);
    } else {
        child.args(["add", REPOSITORY, "--skill", "thruput"]);
        if options.global {
            child.arg("--global");
        }
        for agent in installer_agents(&options.agents) {
            child.arg("--agent").arg(agent);
        }
        if !print.can_prompt() {
            child.arg("--json");
        }
    }
    if options.yes {
        child.arg("--yes");
    }
    let status = print.run_setup_process(&mut child)?;
    if !status.success() {
        return Err(SkillError::Failed(status.code()).into());
    }
    // The upstream interactive installer exits zero on cancellation too. Verify
    // that the selected scope actually contains our skill before reporting it.
    if !update {
        match require_installed(options.global) {
            Err(CliError::Skill(SkillError::NotInstalled))
                if installer_selects_scope(update, options, print) =>
            {
                // Without an explicit scope, the upstream UI lets the user
                // select Global instead of its Project default.
                require_installed(true)?;
            }
            result => result?,
        }
    }
    Ok(())
}

/// Agents passed to the installer, always including the canonical `.agents/skills` target.
///
/// Inside a coding agent, skills@1.7.0 installs in copy mode and writes only the selected
/// agents' directories, such as `~/.claude/skills`. `show`, `list` and `update` read
/// [`SKILL_DIRECTORY`], so also select the `universal` agent, whose directory is that
/// canonical location. With it selected the installer links the other agents to the
/// canonical copy instead of copying.
fn installer_agents(agents: &[String]) -> Vec<&str> {
    let mut selected: Vec<&str> = agents.iter().map(String::as_str).collect();
    if !selected.is_empty()
        && !selected
            .iter()
            .any(|agent| matches!(*agent, "*" | "universal"))
    {
        selected.push("universal");
    }
    selected
}

fn refresh_global() -> Result<(), CliError> {
    #[derive(Deserialize)]
    struct RefreshReport {
        name: Option<String>,
        status: String,
        source: Option<String>,
        scope: Option<String>,
    }

    // skills@1.7.0 update can exit zero after a failed global source check.
    // Its add --json path fetches the source and reports each installation,
    // including refreshes whose content has not changed. Consent is checked above.
    let mut command = Command::new("npx");
    command.args([
        "--yes",
        SKILLS_PACKAGE,
        "add",
        REPOSITORY,
        "--skill",
        "thruput",
        "--global",
        "--yes",
        "--json",
    ]);
    let output = Print::run_setup_process_report(&mut command)?;
    if !output.status.success() {
        return Err(SkillError::Failed(output.status.code()).into());
    }
    let reports: Vec<RefreshReport> =
        serde_json::from_slice(&output.stdout).map_err(SkillError::InstallerReport)?;
    if !matches!(reports.as_slice(), [report]
        if report.name.as_deref() == Some("thruput")
            && report.status == "installed"
            && report.source.as_deref() == Some(REPOSITORY)
            && report.scope.as_deref() == Some("global"))
    {
        return Err(SkillError::UpdateUnconfirmed.into());
    }
    require_installed(true)
}

fn require_installed(global: bool) -> Result<(), CliError> {
    #[derive(Deserialize)]
    struct Installed {
        name: String,
        source: Option<String>,
    }
    let mut command = Command::new("npx");
    command.args(["--yes", SKILLS_PACKAGE, "list", "--json"]);
    if global {
        command.arg("--global");
    }
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env("CI", "1")
        .output()?;
    if !output.status.success() {
        return Err(SkillError::Failed(output.status.code()).into());
    }
    let installed: Vec<Installed> =
        serde_json::from_slice(&output.stdout).map_err(SkillError::InstallerReport)?;
    if !installed
        .iter()
        .any(|skill| skill.name == "thruput" && skill.source.as_deref() == Some(REPOSITORY))
    {
        return Err(SkillError::NotInstalled.into());
    }
    Ok(())
}
