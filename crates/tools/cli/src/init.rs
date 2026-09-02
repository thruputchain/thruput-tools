//! `groundhog init` — interactive first-run wizard.
//!
//! Walks a new user through environment setup, key generation, and
//! optional project scaffolding, and reports whether optional
//! test-executor download credentials are present. When everything is
//! already configured, prints a short summary and exits. `init --json`
//! delegates to `status --json`.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::{CliError, Print};

const INIT_STEPS: usize = 5;
const DEFAULT_LOCAL_RPC_URL: &str = "http://127.0.0.1:9545";

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";

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

    // Executor download credentials are optional (the SDK falls back to an
    // unauthenticated download), so they do not gate readiness.
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

    ui.begin_step(InitStep::ExecutorCredentials);
    handle_credentials_step(&report, &mut ui);

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
        if confirm(ui.print, "Generate a local signing key now?", true)? {
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

fn handle_credentials_step(report: &crate::checks::StatusReport, ui: &mut InitUi<'_>) {
    if report.credentials.executor_credentials {
        ui.success("Test-executor download credentials available");
        ui.finish_step(InitStep::ExecutorCredentials, StepState::Done);
    } else {
        // Public builds download the test executor without credentials;
        // only authenticated internal builds need to configure any.
        ui.note(
            "Test-executor download credentials not configured (optional; \
             set HOGD_FFI_PASSWORD or an [executor] table in the global \
             config only if your build requires authentication)",
        );
        ui.finish_step(InitStep::ExecutorCredentials, StepState::Skipped);
    }
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

    let created_project = if confirm(ui.print, "Create a starter project?", false)? {
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
    ExecutorCredentials,
    Project,
}

impl InitStep {
    const ALL: [Self; INIT_STEPS] = [
        Self::Environment,
        Self::Profiles,
        Self::SigningKey,
        Self::ExecutorCredentials,
        Self::Project,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Environment => 0,
            Self::Profiles => 1,
            Self::SigningKey => 2,
            Self::ExecutorCredentials => 3,
            Self::Project => 4,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Environment => "Environment",
            Self::Profiles => "Profiles",
            Self::SigningKey => "Signing key",
            Self::ExecutorCredentials => "Test-executor download credentials (optional)",
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
    tty: bool,
    steps: [StepState; INIT_STEPS],
}

impl<'a> InitUi<'a> {
    fn new(print: &'a Print) -> Self {
        Self {
            print,
            tty: print.is_human_tty(),
            steps: [StepState::Pending; INIT_STEPS],
        }
    }

    fn render_intro(&mut self) {
        if self.print.is_quiet() {
            return;
        }

        if self.tty {
            self.clear_screen();
            Self::render_tty_header(
                "Groundhog Setup",
                "Set up your local development environment.",
            );
            self.render_tty_progress();
            eprintln!();
        } else {
            self.print.status("Groundhog Setup");
            self.print
                .status("Set up your local development environment.");
            self.print.status("");
        }
    }

    fn begin_step(&mut self, step: InitStep) {
        self.steps[step.index()] = StepState::Current;

        if self.print.is_quiet() {
            return;
        }

        if self.tty {
            self.clear_screen();
            Self::render_tty_header(
                "Groundhog Setup",
                "Set up your local development environment.",
            );
            self.render_tty_progress();
            eprintln!();
        } else {
            self.print.status(format_args!(
                "[{}/{}] {}",
                step.index() + 1,
                INIT_STEPS,
                step.label()
            ));
        }
    }

    fn finish_step(&mut self, step: InitStep, state: StepState) {
        self.steps[step.index()] = state;
    }

    fn success(&self, msg: impl std::fmt::Display) {
        if self.tty {
            eprintln!("{GREEN}✓{RESET} {msg}");
        } else {
            self.print.status(format_args!("[ok] {msg}"));
        }
    }

    fn note(&self, msg: impl std::fmt::Display) {
        if self.tty {
            eprintln!("{YELLOW}•{RESET} {msg}");
        } else {
            self.print.status(format_args!("- {msg}"));
        }
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

        if self.tty {
            self.clear_screen();
            Self::render_tty_completion(title, items);
        } else {
            self.print.status("");
            self.print.status(title);
            self.print.status("");
            for (label, value) in items {
                self.print.status(format_args!("{label}: {value}"));
            }
        }

        self.print.status("");
        self.print.status("Next:");
        match next_steps {
            NextSteps::Status => self.print.status("  groundhog status"),
            NextSteps::NewProject => {
                self.print.status("  groundhog new my-contract");
                self.print.status("  cd my-contract");
                self.print.status("  groundhog build");
            }
            NextSteps::BuildProject(project) => {
                if project.needs_cd {
                    self.print.status(format_args!(
                        "  cd {}",
                        crate::terminal::render_shell_arg(&project.name),
                    ));
                }
                self.print.status("  groundhog build");
            }
        }
    }

    fn clear_screen(&self) {
        if self.tty {
            eprint!("\x1b[2J\x1b[H");
        }
    }

    fn render_tty_header(title: &str, subtitle: &str) {
        let inner_width: usize = 56;
        let top_fill = inner_width.saturating_sub(title.chars().count() + 2);
        let fill = Self::line_fill('─', top_fill);
        eprintln!("{CYAN}╭─ {BOLD}{title}{RESET}{CYAN}{fill}╮{RESET}");
        eprintln!("{CYAN}│{RESET} {subtitle:<54} {CYAN}│{RESET}");
        eprintln!("{CYAN}╰{}╯{RESET}", Self::line_fill('─', inner_width));
        eprintln!();
    }

    fn render_tty_progress(&self) {
        eprintln!("{BOLD}Progress{RESET}");
        for step in InitStep::ALL {
            let state = self.steps[step.index()];
            let marker = match state {
                StepState::Pending => format!("{DIM}○{RESET}"),
                StepState::Current => format!("{CYAN}●{RESET}"),
                StepState::Done => format!("{GREEN}✓{RESET}"),
                StepState::Skipped => format!("{YELLOW}–{RESET}"),
            };
            let raw_label = step.label();
            let label = match state {
                StepState::Pending => format!("{DIM}{raw_label}{RESET}"),
                StepState::Current => format!("{CYAN}{raw_label}{RESET}"),
                StepState::Done => format!("{GREEN}{raw_label}{RESET}"),
                StepState::Skipped => format!("{YELLOW}{raw_label}{RESET}"),
            };
            let padding = Self::line_fill(' ', 16usize.saturating_sub(raw_label.chars().count()));
            let state_label = state.label();
            eprintln!("  {marker} {label}{padding} {DIM}{state_label}{RESET}");
        }
    }

    fn render_tty_completion(title: &str, items: &[(String, String)]) {
        let value_width = items
            .iter()
            .map(|(_, value)| value.chars().count())
            .max()
            .unwrap_or(0)
            .max(18);
        let inner_width = 1 + 10 + 1 + value_width + 1;
        let top_fill = inner_width.saturating_sub(title.chars().count() + 2);
        let fill = Self::line_fill('─', top_fill);
        eprintln!("{GREEN}╭─ {BOLD}{title}{RESET}{GREEN}{fill}╮{RESET}");
        for (label, value) in items {
            let value_padding =
                Self::line_fill(' ', value_width.saturating_sub(value.chars().count()));
            eprintln!("{GREEN}│{RESET} {label:<10} {value}{value_padding} {GREEN}│{RESET}");
        }
        eprintln!("{GREEN}╰{}╯{RESET}", Self::line_fill('─', inner_width));
    }

    fn line_fill(ch: char, len: usize) -> String {
        std::iter::repeat_n(ch, len).collect()
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

fn confirm(print: &Print, message: &str, default_yes: bool) -> Result<bool, CliError> {
    if print.is_quiet() {
        return Ok(default_yes);
    }

    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    print.prompt(format_args!("{message} {suffix} "))?;

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(CliError::Io)?;
    let answer = answer.trim();

    if answer.is_empty() {
        return Ok(default_yes);
    }

    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}
