//! Embedded, release-matched instructions for coding agents.

use crate::{
    CliError, Print,
    result::{CommandError, CommandResult},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
};

const SOURCE: &str = include_str!("../assets/groundhog/knowledge/skill-source.json");
const SKILLS_PACKAGE: &str = "skills@1.7.0";
const REPOSITORY: &str = "groundhogchain/agent-skills";

/// Errors reading or installing the Groundhog skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("unknown skill topic `{0}`; run `groundhog skill list`")]
    Topic(String),
    #[error("invalid bundled skill: {0}; reinstall the CLI")]
    Bundle(serde_json::Error),
    #[error("{0} is not on PATH; install Node.js 22.20 or newer with npm, and Git")]
    MissingTool(&'static str),
    #[error("Node.js 22.20 or newer is required; installed version is {0}")]
    NodeVersion(String),
    #[error(
        "Groundhog is not installed from groundhogchain/agent-skills in this scope; run `groundhog skill install` with the same scope"
    )]
    NotInstalled,
    #[error("cannot read the skills installation report: {0}")]
    InstallerReport(serde_json::Error),
    #[error(
        "skills did not confirm a global Groundhog refresh; check repository access and retry `groundhog skill update --global`"
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

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Provenance {
    repository: String,
    revision: Option<String>,
    dirty: bool,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Bundle {
    #[serde(flatten)]
    provenance: Provenance,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ReadReport {
    schema_version: u32,
    cli_version: &'static str,
    provenance: Provenance,
    topics: Vec<String>,
    content: Option<String>,
}

/// Print the bundled entry, a topic, all guidance, or the topic index.
pub fn show(
    topic: Option<&str>,
    all: bool,
    list: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let bundle: Bundle = serde_json::from_str(SOURCE).map_err(SkillError::Bundle)?;
    let topics: Vec<_> = bundle
        .files
        .keys()
        .filter_map(|path| {
            path.strip_prefix("references/")?
                .strip_suffix(".md")
                .map(str::to_owned)
        })
        .collect();
    let result = if list {
        Ok(None)
    } else if all {
        Ok(Some(render()))
    } else {
        let path = topic.map_or_else(
            || "SKILL.md".into(),
            |topic| format!("references/{topic}.md"),
        );
        bundle
            .files
            .get(&path)
            .map(|content| Some(with_provenance(content, &bundle.provenance)))
            .ok_or_else(|| CliError::from(SkillError::Topic(topic.unwrap_or("entry").into())))
    };
    let mut report = ReadReport {
        schema_version: 1,
        cli_version: env!("CARGO_PKG_VERSION"),
        provenance: bundle.provenance,
        topics,
        content: None,
    };
    match result {
        Ok(content) => {
            report.content = content;
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

fn with_provenance(content: &str, source: &Provenance) -> String {
    let identity = format!(
        "<!-- groundhog-cli-version: {} -->\n<!-- skill-source: {}; revision: {}; sha256: {}; dirty: {} -->\n\n",
        env!("CARGO_PKG_VERSION"),
        source.repository,
        source.revision.as_deref().unwrap_or("uncommitted"),
        source.sha256,
        source.dirty
    );
    if let Some((frontmatter, body)) = content
        .strip_prefix("---\n")
        .and_then(|text| text.split_once("\n---\n"))
    {
        format!("---\n{frontmatter}\n---\n\n{identity}{}", body.trim_start())
    } else {
        format!("{identity}{content}")
    }
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

/// Install or update only Groundhog, using the latest maintained source.
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
                "Groundhog skill update complete."
            } else {
                "Groundhog skill is installed."
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
        "Run npx skills to install or update Groundhog guidance?",
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
        child.args(["update", "groundhog", "--project"]);
    } else {
        child.args(["add", REPOSITORY, "--skill", "groundhog"]);
        if options.global {
            child.arg("--global");
        }
        for agent in &options.agents {
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
        "groundhog",
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
        if report.name.as_deref() == Some("groundhog")
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
        .any(|skill| skill.name == "groundhog" && skill.source.as_deref() == Some(REPOSITORY))
    {
        return Err(SkillError::NotInstalled.into());
    }
    Ok(())
}

const CLI_VERSION_PLACEHOLDER: &str = "{{GROUNDHOG_CLI_VERSION}}";
const SKILL_TEMPLATE: &str = include_str!("../assets/groundhog/knowledge/generated/SKILL.md");

/// Render the skill bundled with this CLI build.
#[must_use]
pub fn render() -> String {
    SKILL_TEMPLATE.replace(CLI_VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_skill_has_valid_identity_and_bounded_size() {
        let skill = render();

        assert!(skill.starts_with("---\nname: groundhog\n"));
        assert!(skill.contains(&format!(
            "<!-- groundhog-cli-version: {} -->",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!skill.contains(CLI_VERSION_PLACEHOLDER));
        assert!(skill.ends_with('\n'));
        assert!(!skill.ends_with("\n\n"));
        assert!(
            skill.len() <= 32 * 1024,
            "complete skill is {} bytes",
            skill.len()
        );
        let bundle: Bundle = serde_json::from_str(SOURCE).unwrap();
        assert!(bundle.files["SKILL.md"].len() < 5 * 1024);
    }

    #[test]
    fn rendered_skill_preserves_high_risk_guidance() {
        let skill = render();

        assert!(skill.contains("`Balance` is the practical starting point"));
        assert!(skill.contains("Do not use raw `NNInt` for a balance"));
        assert!(skill.contains("`Balance` is the ordinary type for balances"));
        assert!(skill.contains("`NNInt` is the lower-level integer type for counters"));
        assert!(skill.contains("`RCQ<T>` is the SDK's retro-causal queue"));
        assert!(skill.contains("self.value.try_get().none_if_missing().unwrap_or(0)"));
        assert!(skill.contains("A generated TypeScript binding describes a contract"));
        assert!(
            skill.contains("Generated event descriptors such as `my_token.v1.events.Transfer`")
        );
        assert!(skill.contains("Inspect\n   `node_modules`, Cargo registry source"));
        assert!(skill.contains("Never print, log, store, or paste private signing material"));
    }
}
