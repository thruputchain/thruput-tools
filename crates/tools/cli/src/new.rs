use crate::checks::{Finding, FindingLevel};
use crate::terminal::render_shell_arg;
use crate::{CliError, Print};
use serde_json::json;
use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;

/// Schema version of the `new --json` data payload.
pub const NEW_JSON_SCHEMA_VERSION: u32 = 1;

/// Create a new contract project in a subdirectory of
/// the current working directory.
pub fn run(name: &str, template: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let dir = env::current_dir()?.join(name);
    run_in(&dir, name, template, cmd, print)
}

/// Create a new contract project at the given path.
pub fn run_in(
    dir: &Path,
    name: &str,
    template: &str,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    // Use create_dir directly to avoid TOCTOU race with
    // a separate exists() check.
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(CliError::DirExists(dir.display().to_string()));
        }
        Err(e) => return Err(e.into()),
    }
    let task = print.start_task(format_args!("Creating project `{name}`"));
    crate::scaffold::generate(dir, name, template)?;
    task.success(format_args!("Created project `{name}`"));

    let warnings = scaffold_warnings();
    for warning in &warnings {
        print.warn(format_args!("{}: {}", warning.code, warning.message));
        if let Some(hint) = warning.hint.as_deref() {
            print.status(format_args!("  hint: {hint}"));
        }
    }

    render_project_ready(dir, name, template, print);
    print.json(&crate::result::CommandResult::with_warnings(
        cmd,
        warnings,
        json!({
            "schemaVersion": NEW_JSON_SCHEMA_VERSION,
            "projectName": name,
            "template": template,
            "path": dir,
            "configPath": dir.join("groundhog.toml"),
        }),
    ))?;
    Ok(())
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

fn render_project_ready(dir: &Path, name: &str, template: &str, print: &Print) {
    if print.is_quiet() || print.is_json() {
        return;
    }

    let items = vec![
        ("Project".to_string(), name.to_string()),
        ("Path".to_string(), dir.display().to_string()),
        ("Template".to_string(), template.to_string()),
        (
            "Config".to_string(),
            dir.join("groundhog.toml").display().to_string(),
        ),
    ];

    print.status("");
    print.summary_card("Project Ready", &items);

    let next_steps = vec![
        format!("cd {}", render_shell_arg(&dir.to_string_lossy())),
        "cargo test".to_string(),
        "groundhog build".to_string(),
    ];
    print.next_steps(&next_steps);
}
