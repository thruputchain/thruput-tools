//! `thruput init` — interactive first-run wizard.
//!
//! Walks a new user through environment setup, key generation, and optional
//! project scaffolding. When everything is already configured, prints a short
//! summary and exits. Automated setup emits its own versioned JSON result.

use std::path::{Path, PathBuf};

use crate::init_json::{INIT_JSON_SCHEMA_VERSION, InitReport, InitRequirement};
use crate::print::{StepState, Wizard};
use crate::result::{CommandError, CommandResult};
use crate::{CliError, Print};

const INIT_STEPS: usize = 4;
const DEFAULT_LOCAL_RPC_URL: &str = "http://127.0.0.1:9545";

/// Explicit choices for setup; output flags never authorize key generation.
#[derive(Debug, Default)]
pub struct InitOptions {
    /// Authorize initial key creation without an interactive confirmation.
    pub generate_key: bool,
}

/// Initialize global profiles and report whether required setup is complete.
pub fn run(
    project_start: &Path,
    options: &InitOptions,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    // Validate existing state before writing configuration.
    crate::checks::collect_report(project_start, None)?;
    crate::config::initialize_global_profiles()?;
    let report = crate::checks::collect_report(project_start, None)?;
    let mut ui = InitUi::new(print);
    let setup = setup_report(&report, false);
    if setup.complete {
        return finish_setup(project_start, &report, setup, None, true, cmd, &ui);
    }

    // Do not create keys or projects while basic prerequisites need repair.
    if !setup.cargo_available || !setup.profiles_configured {
        return finish_setup(project_start, &report, setup, None, false, cmd, &ui);
    }

    ui.begin_step(InitStep::Environment)?;
    handle_environment_step(&mut ui);
    ui.finish_step(InitStep::Environment, StepState::Done);
    ui.begin_step(InitStep::Profiles)?;
    handle_profile_step(project_start, &mut ui)?;
    ui.finish_step(InitStep::Profiles, StepState::Done);
    ui.begin_step(InitStep::SigningKey)?;
    let key_generated = handle_key_step(&report, options, &mut ui)?;
    ui.begin_step(InitStep::Project)?;
    let project_context = handle_scaffold_step(
        report.project.config_path.is_some(),
        project_start,
        &report,
        &mut ui,
    )?;
    let final_report = crate::checks::collect_report(project_start, None)?;
    let setup = setup_report(&final_report, key_generated);
    finish_setup(
        project_start,
        &final_report,
        setup,
        project_context.as_ref(),
        false,
        cmd,
        &ui,
    )
}

fn setup_report(report: &crate::checks::StatusReport, key_generated: bool) -> InitReport {
    let profiles_configured = report.project.profile_name.is_some()
        && !report.project.findings.iter().any(|finding| {
            matches!(
                finding.code,
                "profile_not_found" | "profile_config_invalid" | "no_active_profile"
            )
        });
    let mut missing = Vec::new();
    if !report.environment.cargo {
        missing.push(InitRequirement::Cargo);
    }
    if !profiles_configured {
        missing.push(InitRequirement::Profile);
    }
    if usable_default_key_for_init(report).is_none() {
        missing.push(InitRequirement::SigningKey);
    }
    InitReport {
        schema_version: INIT_JSON_SCHEMA_VERSION,
        complete: missing.is_empty(),
        cargo_available: report.environment.cargo,
        profiles_configured,
        selected_key: report.effective_default_key.clone(),
        key_generated,
        missing,
    }
}

fn finish_setup(
    project_start: &Path,
    report: &crate::checks::StatusReport,
    setup: InitReport,
    project: Option<&ProjectContext>,
    already_ready: bool,
    cmd: &'static str,
    ui: &InitUi<'_>,
) -> Result<(), CliError> {
    let complete = setup.complete;
    let hints = setup.missing.iter().map(|requirement| match requirement {
        InitRequirement::Cargo => "Install Rust and put cargo on PATH.",
        InitRequirement::Profile => "Repair the active profile with thruput config, or select a valid profile.",
        InitRequirement::SigningKey if can_generate_initial_key(report) =>
            "Run thruput init --non-interactive --generate-key to create the initial signing key.",
        InitRequirement::SigningKey =>
            "Select a valid stored key with thruput keys use <name>; repair any profile default_key override with thruput config. Restore or replace corrupt key files explicitly.",
    }).collect::<Vec<_>>().join(" ");
    if ui.print.is_json() {
        let envelope = if complete {
            CommandResult::new(cmd, setup)
        } else {
            CommandResult::failure_with_error(
                cmd,
                setup,
                CommandError::new(
                    "init_incomplete",
                    "Setup incomplete",
                    serde_json::Value::Null,
                )
                .with_hint(&hints),
            )
        };
        ui.print.json(&envelope)?;
    } else if complete {
        let next_steps = if already_ready {
            NextSteps::Status
        } else if let Some(project) = project {
            NextSteps::BuildProject(project)
        } else {
            NextSteps::NewProject
        };
        ui.completion_review(
            if already_ready {
                "Thruput Ready"
            } else {
                "Setup Complete"
            },
            &summary_items(project_start, usable_default_key_for_init(report), project)?,
            next_steps,
        )?;
    } else {
        ui.finish_view()?;
        if ui.wizard.is_active() {
            ui.print.outcome_findings(
                crate::print::Outcome::Partial,
                "Setup Incomplete",
                &[("Next step".into(), hints)],
                &ui.warnings,
            );
        } else {
            ui.print.group(
                crate::print::Outcome::Partial,
                "Setup Incomplete",
                |print| {
                    print.status("Setup Incomplete");
                    print.status(&hints);
                },
            );
        }
    }
    if complete {
        Ok(())
    } else {
        Err(CliError::CommandReportedFailure)
    }
}

fn handle_environment_step(ui: &mut InitUi<'_>) {
    ui.success("Cargo available");
}

fn handle_profile_step(project_start: &Path, ui: &mut InitUi<'_>) -> Result<(), CliError> {
    let global_config_path = crate::config::config_dir()?.join("config.toml");
    ui.success(format_args!("Config: {}", global_config_path.display()));

    let profile_name = crate::config::active_profile_name(project_start, None)?;
    ui.success(format_args!("Default profile: {profile_name}"));

    let local_rpc = local_rpc_url(project_start).unwrap_or_else(|| DEFAULT_LOCAL_RPC_URL.into());
    ui.success(format_args!("Local RPC: {local_rpc}"));
    ui.success("NATIVE_TOKEN alias configured");

    Ok(())
}

fn usable_default_key_for_init(
    report: &crate::checks::StatusReport,
) -> Option<&crate::checks::KeySummary> {
    let key = report.effective_default_key_summary()?;
    // A public summary alone does not establish that the private key is usable.
    let signer = crate::keys::load_local_signer(&key.name).ok()?;
    let address = thruput_signing::PubkeyAccount::new(
        thruput_signing::Signer::public_key(&signer),
        thruput_signing::DEFAULT_PUBKEY_ACCOUNT_NONCE,
    )
    .address();
    (thruput_encoding::encode(address.as_ref()) == key.address).then_some(key)
}

fn can_generate_initial_key(report: &crate::checks::StatusReport) -> bool {
    report.keys.keys.is_empty()
        && report.keys.global_default_key.is_none()
        && report.project.profile_key.is_none()
}

fn handle_key_step(
    report: &crate::checks::StatusReport,
    options: &InitOptions,
    ui: &mut InitUi<'_>,
) -> Result<bool, CliError> {
    if let Some(key) = usable_default_key_for_init(report) {
        ui.success(format_args!(
            "Default key: `{}` ({})",
            key.name, key.address
        ));
        ui.finish_step(InitStep::SigningKey, StepState::Done);
        return Ok(false);
    }
    let can_generate = can_generate_initial_key(report);
    if can_generate && !options.generate_key && ui.print.can_prompt() {
        ui.note("No signing key found");
    }
    if can_generate
        && (options.generate_key || ui.print.ask("Generate a local signing key now?", true)?)
    {
        // The enclosing init result owns JSON output, not the nested command.
        crate::keys::generate(
            &None,
            crate::keys::KeyScheme::Ed25519,
            false,
            "keys.generate",
            &Print::quiet(),
        )?;
        ui.success("Generated local signing key");
        ui.finish_step(InitStep::SigningKey, StepState::Done);
        return Ok(true);
    }
    ui.note("No usable selected signing key; key setup remains incomplete");
    ui.finish_step(InitStep::SigningKey, StepState::Skipped);
    Ok(false)
}

fn handle_scaffold_step(
    has_project: bool,
    project_start: &Path,
    report: &crate::checks::StatusReport,
    ui: &mut InitUi<'_>,
) -> Result<Option<ProjectContext>, CliError> {
    if has_project {
        if let Some(path) = report.project.config_path.as_ref() {
            ui.success(format_args!("Found project at {}", path.display()));
        }
        ui.finish_step(InitStep::Project, StepState::Done);
        return Ok(report
            .project
            .config_path
            .as_deref()
            .and_then(existing_project_context));
    }

    let created_project = if ui.print.ask("Create a starter project?", false)? {
        let name = ui.print.read_line("Project name: ")?;
        let name = name.trim();
        if name.is_empty() {
            ui.note("Skipping starter project");
            ui.finish_step(InitStep::Project, StepState::Skipped);
            None
        } else {
            let dir = project_start.join(name);
            ui.warnings.extend(crate::new::run_in_with_warnings(
                &dir, name, None, "new", ui.print,
            )?);
            ui.finish_step(InitStep::Project, StepState::Done);
            Some(ProjectContext {
                name: name.to_string(),
                path: dir,
                needs_cd: true,
            })
        }
    } else {
        ui.note("Skipping starter project");
        ui.finish_step(InitStep::Project, StepState::Skipped);
        None
    };

    Ok(created_project)
}

struct ProjectContext {
    name: String,
    path: PathBuf,
    needs_cd: bool,
}

#[derive(Clone, Copy)]
enum NextSteps<'a> {
    Status,
    NewProject,
    BuildProject(&'a ProjectContext),
}

#[derive(Clone, Copy)]
enum InitStep {
    Environment,
    Profiles,
    SigningKey,
    Project,
}

impl InitStep {
    const ALL: [Self; INIT_STEPS] = [
        Self::Environment,
        Self::Profiles,
        Self::SigningKey,
        Self::Project,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Environment => 0,
            Self::Profiles => 1,
            Self::SigningKey => 2,
            Self::Project => 3,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Environment => "Environment",
            Self::Profiles => "Profiles",
            Self::SigningKey => "Signing key",
            Self::Project => "Project",
        }
    }
}

struct InitUi<'a> {
    print: &'a Print,
    wizard: Wizard<'a, INIT_STEPS>,
    warnings: Vec<crate::checks::Finding>,
}

impl<'a> InitUi<'a> {
    fn new(print: &'a Print) -> Self {
        Self {
            print,
            wizard: Wizard::new(
                print,
                "Thruput Setup",
                "Set up your local development environment.",
                InitStep::ALL.map(InitStep::label),
            ),
            warnings: Vec::new(),
        }
    }

    fn begin_step(&mut self, step: InitStep) -> Result<(), CliError> {
        self.wizard.begin_step(step.index())
    }

    fn finish_step(&mut self, step: InitStep, state: StepState) {
        self.wizard.finish_step(step.index(), state);
    }

    fn success(&self, msg: impl std::fmt::Display) {
        self.print.success(msg);
    }

    fn note(&self, msg: impl std::fmt::Display) {
        self.print.note(msg);
    }

    fn finish_view(&self) -> Result<(), CliError> {
        self.wizard.finish_view()
    }

    fn completion_review(
        &self,
        title: &str,
        items: &[(String, String)],
        next_steps: NextSteps<'_>,
    ) -> Result<(), CliError> {
        if self.print.is_quiet() {
            return Ok(());
        }
        self.finish_view()?;
        if self.wizard.is_active() {
            self.print
                .outcome_findings(crate::print::Outcome::Ready, title, items, &self.warnings);
        } else {
            self.print
                .outcome_card(crate::print::Outcome::Ready, title, items);
        }
        let mut commands = Vec::new();
        match next_steps {
            NextSteps::Status => commands.push("thruput status".into()),
            NextSteps::NewProject => {
                commands.extend([
                    "thruput new my-contract".into(),
                    "cd my-contract".into(),
                    "thruput build".into(),
                ]);
            }
            NextSteps::BuildProject(project) => {
                if project.needs_cd {
                    commands.push(format!(
                        "cd {}",
                        crate::terminal::render_shell_arg(&project.name),
                    ));
                }
                commands.push("thruput build".into());
            }
        }
        self.print.next_steps(&commands);
        Ok(())
    }
}

fn summary_items(
    project_start: &Path,
    usable_default_key: Option<&crate::checks::KeySummary>,
    project_context: Option<&ProjectContext>,
) -> Result<Vec<(String, String)>, CliError> {
    let profile_name = crate::config::active_profile_name(project_start, None)?;
    let mut items = vec![
        (
            "Config".to_string(),
            crate::config::config_dir()?
                .join("config.toml")
                .display()
                .to_string(),
        ),
        ("Profile".to_string(), profile_name),
    ];

    if let Some(key) = usable_default_key {
        items.push(("Key".to_string(), format!("{} ({})", key.name, key.address)));
    } else if let Some(key_name) = crate::config::load_default_key()? {
        items.push(("Key".to_string(), key_name));
    }

    let local_rpc = local_rpc_url(project_start).unwrap_or_else(|| DEFAULT_LOCAL_RPC_URL.into());
    items.push(("Local RPC".to_string(), local_rpc));
    if let Some(project) = project_context {
        items.push(("Project".to_string(), project.path.display().to_string()));
    }

    Ok(items)
}

fn existing_project_context(config_path: &Path) -> Option<ProjectContext> {
    let path = config_path.parent()?.to_path_buf();
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(ProjectContext {
        name,
        path,
        needs_cd: false,
    })
}

fn local_rpc_url(project_start: &Path) -> Option<String> {
    crate::config::resolve_profile(project_start, Some("local"))
        .ok()
        .and_then(|profile| profile.rpc_url)
}
