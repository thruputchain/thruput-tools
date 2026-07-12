use crate::{CliError, Print};
use serde_json::json;
use std::env;
use std::fs;
use std::path::Path;

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
    render_project_ready(dir, name, template, print);
    print.json(&crate::result::CommandResult::new(
        cmd,
        json!({
            "project_name": name,
            "template": template,
            "path": dir,
            "config_path": dir.join("groundhog.toml"),
        }),
    ))?;
    Ok(())
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
        format!("cd {}", dir.display()),
        "groundhog build".to_string(),
    ];
    print.next_steps(&next_steps);
}
