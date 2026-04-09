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
    crate::scaffold::generate(dir, name, template)?;
    print.status(format_args!(
        "Created project `{name}` in {}",
        dir.display()
    ));
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
