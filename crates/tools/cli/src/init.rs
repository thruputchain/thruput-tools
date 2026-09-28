//! `groundhog init` — interactive first-run wizard.
//!
//! Walks a new user through environment setup, key generation, and optional
//! project scaffolding. When everything is already configured, prints a short
//! summary and exits. `init --json` delegates to `status --json`.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use crate::{CliError, Print};

const INIT_STEPS: usize = 4;
const DEFAULT_LOCAL_RPC_URL: &str = "http://127.0.0.1:9545";

/// Run the interactive first-run wizard.
pub fn run(project_start: &Path, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        return crate::status::run(project_start, None, cmd, print);
    }

    let report = crate::checks::collect_report(project_start, None)?;
    let has_project = report.project.config_path.is_some();
    let usable_default_key = usable_default_key_for_init(&report);

    crate::config::initialize_global_profiles()?;
    let mut ui = InitUi::new(print);
    ui.render_intro();

    if report.environment.cargo && usable_default_key.is_some() {
        report_ready(project_start, usable_default_key, &mut ui)?;
        return Ok(());
    }

    if !report.environment.cargo {
        return Err(CliError::CargoNotFound);
    }

    ui.begin_step(InitStep::Environment);
    handle_environment_step(&mut ui);
    ui.finish_step(InitStep::Environment, StepState::Done);

    ui.begin_step(InitStep::Profiles);
    handle_profile_step(project_start, &mut ui)?;
    ui.finish_step(InitStep::Profiles, StepState::Done);

    ui.begin_step(InitStep::SigningKey);
    handle_key_step(&report, usable_default_key, &mut ui)?;

    ui.begin_step(InitStep::Project);
    let project_context = handle_scaffold_step(has_project, project_start, &report, &mut ui)?;

    let final_report = crate::checks::collect_report(project_start, None)?;
    let final_default_key = usable_default_key_for_init(&final_report);
    print_init_summary(
        project_start,
        final_default_key,
        project_context.as_ref(),
        &ui,
    )?;
    Ok(())
}

fn report_ready(
    project_start: &Path,
    usable_default_key: Option<&crate::checks::KeySummary>,
    ui: &mut InitUi<'_>,
) -> Result<(), CliError> {
    ui.completion_review(
        "Groundhog Ready",
        &summary_items(project_start, usable_default_key, None)?,
        NextSteps::Status,
    );

    Ok(())
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
    ui.success("HOG alias configured");

    Ok(())
}

fn handle_key_step(
    report: &crate::checks::StatusReport,
    usable_default_key: Option<&crate::checks::KeySummary>,
    ui: &mut InitUi<'_>,
) -> Result<(), CliError> {
    if report.keys.keys.is_empty() {
        ui.note("No signing key found");
        if ui.print.ask("Generate a local signing key now?", true)? {
            crate::keys::generate(&None, false, "keys.generate", ui.print)?;
            ui.success("Generated local signing key");
            ui.finish_step(InitStep::SigningKey, StepState::Done);
        } else {
            ui.finish_step(InitStep::SigningKey, StepState::Skipped);
        }
    } else if let Some(key) = usable_default_key {
        ui.success(format_args!(
            "Default key: `{}` ({})",
            key.name, key.address
        ));
        ui.finish_step(InitStep::SigningKey, StepState::Done);
    } else {
        ui.note(
            "You have keys but no usable default is selected. Set one \
             with `groundhog keys use <name>`.",
        );
        ui.finish_step(InitStep::SigningKey, StepState::Skipped);
    }

    Ok(())
}

fn usable_default_key_for_init(
    report: &crate::checks::StatusReport,
) -> Option<&crate::checks::KeySummary> {
    if let Some(key) = report.effective_default_key_summary() {
        return Some(key);
    }

    let global_default = report.keys.global_default_key.as_deref()?;
    report
        .keys
        .keys
        .iter()
        .find(|key| key.name == global_default)
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
        ui.print.prompt("Project name: ")?;
        let mut name = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut name)
            .map_err(CliError::Io)?;
        let name = name.trim();
        if name.is_empty() {
            ui.note("Skipping starter project");
            ui.finish_step(InitStep::Project, StepState::Skipped);
            None
        } else {
            let dir = project_start.join(name);
            crate::new::run_in(&dir, name, "counter", "new", ui.print)?;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Pending,
    Current,
    Done,
    Skipped,
}

impl StepState {
    const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Current => "current",
            Self::Done => "done",
            Self::Skipped => "skipped",
        }
    }
}

struct InitUi<'a> {
    print: &'a Print,
    steps: [StepState; INIT_STEPS],
}

impl<'a> InitUi<'a> {
    fn new(print: &'a Print) -> Self {
        Self {
            print,
            steps: [StepState::Pending; INIT_STEPS],
        }
    }

    fn render_intro(&mut self) {
        if self.print.is_quiet() {
            return;
        }

        self.print.summary_card(
            "Groundhog Setup",
            &[(
                "Purpose".into(),
                "Set up your local development environment.".into(),
            )],
        );
    }

    fn begin_step(&mut self, step: InitStep) {
        self.steps[step.index()] = StepState::Current;

        if self.print.is_quiet() {
            return;
        }

        self.print.status(format_args!(
            "[{}/{}] {}",
            step.index() + 1,
            INIT_STEPS,
            step.label()
        ));
    }

    fn finish_step(&mut self, step: InitStep, state: StepState) {
        self.steps[step.index()] = state;
        if state == StepState::Skipped {
            self.print
                .note(format_args!("{}: {}", step.label(), state.label()));
        }
    }

    fn success(&self, msg: impl std::fmt::Display) {
        self.print.success(msg);
    }

    fn note(&self, msg: impl std::fmt::Display) {
        self.print.note(msg);
    }

    fn completion_review(
        &self,
        title: &str,
        items: &[(String, String)],
        next_steps: NextSteps<'_>,
    ) {
        if self.print.is_quiet() {
            return;
        }

        self.print.outcome_card(
            if self.steps.contains(&StepState::Skipped) {
                crate::print::Outcome::Partial
            } else {
                crate::print::Outcome::Ready
            },
            title,
            items,
        );
        let mut commands = Vec::new();
        match next_steps {
            NextSteps::Status => commands.push("groundhog status".into()),
            NextSteps::NewProject => {
                commands.extend([
                    "groundhog new my-contract".into(),
                    "cd my-contract".into(),
                    "groundhog build".into(),
                ]);
            }
            NextSteps::BuildProject(project) => {
                if project.needs_cd {
                    commands.push(format!(
                        "cd {}",
                        crate::terminal::render_shell_arg(&project.name),
                    ));
                }
                commands.push("groundhog build".into());
            }
        }
        self.print.next_steps(&commands);
    }
}

fn print_init_summary(
    project_start: &Path,
    usable_default_key: Option<&crate::checks::KeySummary>,
    project_context: Option<&ProjectContext>,
    ui: &InitUi<'_>,
) -> Result<(), CliError> {
    let next_steps = if let Some(project) = project_context {
        NextSteps::BuildProject(project)
    } else {
        NextSteps::NewProject
    };
    ui.completion_review(
        "Setup Complete",
        &summary_items(project_start, usable_default_key, project_context)?,
        next_steps,
    );
    Ok(())
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
