use crate::checks::{Finding, FindingLevel};
use crate::print::{StepState, Wizard};
use crate::terminal::render_shell_arg;
use crate::{CliError, Print};
use schemars::JsonSchema;
use serde::Serialize;
use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Schema version of the `new --json` data payload.
pub const NEW_JSON_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NewReport<'a> {
    schema_version: u32,
    project_name: &'a str,
    template: Option<&'a str>,
    path: &'a Path,
    config_path: PathBuf,
}

/// Create a project at an explicit destination or in a named subdirectory.
/// Missing names prompt only when terminal interaction is available.
pub fn run(
    name: Option<String>,
    path: Option<PathBuf>,
    template: Option<String>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let interactive = name.is_none() && print.can_prompt();
    let mut wizard = Wizard::new(
        print,
        "New Groundhog Project",
        "Choose a name, destination, and starting template.",
        ["Project name", "Destination", "Template"],
    );
    if interactive {
        wizard.begin_step(0)?;
    }
    let name = match name {
        Some(name) => name,
        None if interactive => print.read_line("Project name: ")?.trim().to_owned(),
        None => {
            return Err(CliError::InvalidArgument {
                argument: "project name".into(),
                message: "pass a project name, or run `groundhog new` in an interactive terminal"
                    .into(),
            });
        }
    };
    crate::scaffold::validate_name(&name)?;
    wizard.finish_step(0, StepState::Done);
    let path = if interactive && path.is_none() {
        wizard.begin_step(1)?;
        let answer = print.read_line(format_args!("Destination [{name}]: "))?;
        Some(PathBuf::from(if answer.trim().is_empty() {
            &name
        } else {
            answer.trim()
        }))
    } else {
        path
    };
    wizard.finish_step(1, StepState::Done);
    let template = if interactive && template.is_none() {
        wizard.begin_step(2)?;
        print.detail(
            "minimal",
            "Empty contract with native and RV32 tests (default)",
        );
        for entry in crate::templates::CATALOG {
            print.detail(entry.id, entry.description);
        }
        let choices = crate::templates::CATALOG
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
            .join(", ");
        let answer = print.read_line(format_args!("Template [{choices}; Enter for minimal]: "))?;
        (!answer.trim().is_empty()).then(|| answer.trim().to_owned())
    } else {
        template
    };
    let allow_empty = path.is_some();
    let dir = destination_path(&path.unwrap_or_else(|| PathBuf::from(&name)))?;
    template
        .as_deref()
        .map(crate::templates::lookup)
        .transpose()?;
    wizard.finish_step(2, StepState::Done);
    wizard.finish_view()?;
    create(&dir, &name, template.as_deref(), allow_empty, cmd, print).map(|_| ())
}

fn destination_path(path: &Path) -> Result<PathBuf, CliError> {
    if let Ok(relative) = path.strip_prefix("~") {
        let home = env::home_dir()
            .filter(|home| home.is_absolute())
            .ok_or_else(|| CliError::InvalidArgument {
                argument: "destination path".into(),
                message: "cannot expand `~`: no absolute home directory could be determined; use an absolute destination instead".into(),
            })?;
        return Ok(home.join(relative));
    }
    if path
        .components()
        .next()
        .is_some_and(|part| part.as_os_str().to_string_lossy().starts_with('~'))
    {
        return Err(CliError::InvalidArgument {
            argument: "destination path".into(),
            message: "use `~/...` for your home directory; `~user` paths are not supported; use an absolute path instead".into(),
        });
    }
    Ok(env::current_dir()?.join(path))
}

/// Create a new contract project at the given path.
pub fn run_in(
    dir: &Path,
    name: &str,
    template: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    run_in_with_warnings(dir, name, template, cmd, print).map(|_| ())
}

/// Return existing scaffold warnings so an enclosing wizard can retain them on its final view.
pub(crate) fn run_in_with_warnings(
    dir: &Path,
    name: &str,
    template: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<Vec<Finding>, CliError> {
    create(dir, name, template, false, cmd, print)
}

fn create(
    dir: &Path,
    name: &str,
    template: Option<&str>,
    allow_empty: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<Vec<Finding>, CliError> {
    // Validate before creating anything, so a rejected name leaves no
    // empty directory behind.
    crate::scaffold::validate_name(name)?;
    template.map(crate::templates::lookup).transpose()?;

    // Use create_dir directly to avoid TOCTOU race with
    // a separate exists() check.
    let directory_error = |source| CliError::ProjectDirectory {
        path: dir.to_path_buf(),
        source,
    };
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && allow_empty => {
            if fs::symlink_metadata(dir)
                .map_err(&directory_error)?
                .file_type()
                .is_symlink()
                || !dir.is_dir()
                || fs::read_dir(dir)
                    .map_err(&directory_error)?
                    .next()
                    .transpose()
                    .map_err(&directory_error)?
                    .is_some()
            {
                return Err(CliError::DirNotEmpty);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(CliError::DirExists(dir.display().to_string()));
        }
        Err(e) => return Err(directory_error(e)),
    }
    // Resolve against the filesystem after creation so symlinks and `..` retain
    // their actual meaning while every reported path is absolute and clean.
    let dir = fs::canonicalize(dir).map_err(directory_error)?;
    let dir = dir.as_path();
    let task = crate::print::progress::Task::start(print, "Creating project", true);
    task.print()
        .phase(format_args!("Creating project `{name}`"));
    crate::scaffold::generate(dir, name, template)?;
    let _ = task.finish();
    if !print.has_human_ui() {
        print.success(format_args!("Created project `{name}`"));
    }

    let warnings = scaffold_warnings();
    if !print.has_human_ui() {
        for warning in &warnings {
            print.warn(format_args!("{}: {}", warning.code, warning.message));
            if let Some(hint) = warning.hint.as_deref() {
                print.status(format_args!("  hint: {hint}"));
            }
        }
    }

    render_project_ready(dir, name, template, &warnings, print);
    print.json(&crate::result::CommandResult::with_warnings(
        cmd,
        warnings.clone(),
        NewReport {
            schema_version: NEW_JSON_SCHEMA_VERSION,
            project_name: name,
            template,
            path: dir,
            config_path: dir.join("groundhog.toml"),
        },
    ))?;
    Ok(warnings)
}

/// Environment problems that will break the generated project's first build.
///
/// Scaffolding itself always succeeds; these are advisory.  The scaffold pins
/// a nightly channel and cross-compiles to RV32 through `rust-toolchain.toml`,
/// which only a rustup-managed `cargo` honors.  A missing toolchain, target,
/// or component is deliberately *not* reported: under rustup those install
/// themselves on the first build, so warning about them would be noise.
fn scaffold_warnings() -> Vec<Finding> {
    if crate::checks::cargo_is_rustup_managed() {
        return Vec::new();
    }
    vec![Finding {
        level: FindingLevel::Warn,
        code: "cargo_not_rustup_managed",
        message: format!(
            "the `cargo` on PATH is not rustup-managed, so this project's \
             rust-toolchain.toml is inert. It pins a nightly channel and the {} \
             target, neither of which a distro-packaged cargo will honor.",
            crate::build::RV32_TARGET_TRIPLE,
        ),
        hint: Some(Cow::Borrowed(
            "install rustup from https://rustup.rs and make sure its shims come before \
             any distro-packaged cargo on PATH, otherwise `groundhog build` will fail",
        )),
    }]
}

fn render_project_ready(
    dir: &Path,
    name: &str,
    template: Option<&str>,
    warnings: &[Finding],
    print: &Print,
) {
    if print.is_quiet() || print.is_json() {
        return;
    }

    let items = vec![
        ("Project".to_string(), name.to_string()),
        ("Path".to_string(), dir.display().to_string()),
        (
            "Template".to_string(),
            template.unwrap_or("minimal").to_string(),
        ),
        (
            "Config".to_string(),
            dir.join("groundhog.toml").display().to_string(),
        ),
    ];

    print.status("");
    if print.has_human_ui() {
        print.outcome_findings(
            crate::print::Outcome::Success,
            "Project Ready",
            &items,
            warnings,
        );
    } else {
        print.summary_card("Project Ready", &items);
    }

    let mut next_steps = vec![format!("cd {}", render_shell_arg(&dir.to_string_lossy()))];
    if let Some(recipe) = template.and_then(|id| crate::templates::lookup(id).ok()) {
        next_steps.extend(recipe.next_steps.iter().map(|step| (*step).to_owned()));
    } else {
        next_steps.extend(["cargo test".into(), "groundhog build".into()]);
    }
    print.next_steps(&next_steps);
}
