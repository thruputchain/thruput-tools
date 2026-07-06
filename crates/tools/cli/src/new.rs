use crate::auth::CargoLoginStatus;
use crate::{CliError, Print};
use serde_json::json;
use std::env;
use std::fs;
use std::path::Path;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";

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
    let cargo_login = crate::auth::offer_cargo_login_after_new(print)?;
    render_project_ready(dir, name, template, cargo_login, print);
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

fn render_project_ready(
    dir: &Path,
    name: &str,
    template: &str,
    cargo_login: CargoLoginStatus,
    print: &Print,
) {
    if print.is_quiet() || print.is_json() {
        return;
    }

    let cargo_status = match cargo_login {
        CargoLoginStatus::LoggedIn => "logged in globally",
        CargoLoginStatus::Skipped => "login skipped",
        CargoLoginStatus::NotInteractive => "not checked",
    };
    let items = vec![
        ("Project".to_string(), name.to_string()),
        ("Path".to_string(), dir.display().to_string()),
        ("Template".to_string(), template.to_string()),
        (
            "Config".to_string(),
            dir.join("groundhog.toml").display().to_string(),
        ),
        ("Cargo Registry".to_string(), cargo_status.to_string()),
    ];

    if print.is_human_tty() {
        render_tty_card("Project Ready", &items);
    } else {
        print.status("");
        print.status("Project Ready");
        print.status("");
        for (label, value) in &items {
            print.status(format_args!("{label}: {value}"));
        }
    }

    print.status("");
    print.status("Next:");
    print.status(format_args!("  cd {}", dir.display()));
    if !matches!(cargo_login, CargoLoginStatus::LoggedIn) {
        print.status("  cargo login --registry groundhog");
    }
    print.status("  cargo build");
}

fn render_tty_card(title: &str, items: &[(String, String)]) {
    let label_width = items
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0)
        .max(title.chars().count());
    let value_width = items
        .iter()
        .map(|(_, value)| value.chars().count())
        .max()
        .unwrap_or(0);
    let inner_width = (label_width + value_width + 5).max(title.chars().count() + 2);
    let top_fill = inner_width.saturating_sub(title.chars().count() + 2);
    eprintln!(
        "{GREEN}╭─ {BOLD}{title}{RESET}{GREEN}{}╮{RESET}",
        line_fill('─', top_fill)
    );
    for (label, value) in items {
        let label_padding = line_fill(' ', label_width.saturating_sub(label.chars().count()));
        let value_padding = line_fill(' ', value_width.saturating_sub(value.chars().count()));
        eprintln!(
            "{GREEN}│{RESET} {DIM}{label}:{RESET}{label_padding} {value}{value_padding} {GREEN}│{RESET}"
        );
    }
    eprintln!("{GREEN}╰{}╯{RESET}", line_fill('─', inner_width));
}

fn line_fill(ch: char, count: usize) -> String {
    std::iter::repeat_n(ch, count).collect()
}
