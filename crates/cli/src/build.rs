use crate::{CliError, Print};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub project_name: String,
    pub project_root: PathBuf,
    pub wasm: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_json: Option<PathBuf>,
    pub size_bytes: u64,
    pub optimized: bool,
    pub profile: crate::config::ResolvedProfile,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stderr: String,
}

/// Build the contract to WASM via `cargo build` with
/// cross-compilation to `wasm32-unknown-unknown`.
pub fn run(
    optimize: bool,
    cargo_args: &[String],
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let result = build_project(optimize, cargo_args, explicit_profile, print)?;
    print.json(&crate::result::CommandResult::new(
        cmd,
        json!({
            "project_name": result.project_name,
            "project_root": result.project_root,
            "wasm": result.wasm,
            "spec_json": result.spec_json,
            "size_bytes": result.size_bytes,
            "optimized": result.optimized,
            "profile": result.profile,
            "cargo_stdout": result.cargo_stdout,
            "cargo_stderr": result.cargo_stderr,
        }),
    ))?;
    Ok(())
}

/// Build the current project and return the artifact metadata without
/// printing a final command payload.
pub fn build_project(
    optimize: bool,
    cargo_args: &[String],
    explicit_profile: Option<&str>,
    print: &Print,
) -> Result<BuildResult, CliError> {
    let cwd = std::env::current_dir()?;
    let (root, name) = find_project(&cwd)?;

    crate::require_cargo()?;

    if optimize {
        which::which("wasm-opt").map_err(|_| CliError::WasmOptNotFound)?;
    }

    print.status(format_args!("Compiling {name} to WASM..."));

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&root)
        .arg("--config")
        .arg(".cargo/contract.toml")
        .arg("build")
        .arg("--release")
        .arg("--no-default-features")
        .arg("--features")
        .arg("entrypoints")
        .arg("--target")
        .arg("wasm32-unknown-unknown");

    for arg in cargo_args {
        cmd.arg(arg);
    }

    let (cargo_stdout, cargo_stderr) = crate::run_cargo(&mut cmd, print)?;

    let wasm = root.join(wasm_output_path(&name));
    let profile = crate::config::resolve_profile(&root, explicit_profile)?;

    if optimize {
        print.status("Optimizing with wasm-opt...");
        let status = Command::new("wasm-opt")
            .arg("-Oz")
            .arg(&wasm)
            .arg("-o")
            .arg(&wasm)
            .status()?;
        if !status.success() {
            return Err(CliError::WasmOptFailed);
        }
    }

    let size = fs::metadata(&wasm)?.len();
    print.status(format_args!("Built {} ({size} bytes)", wasm.display()));
    let spec_json = crate::contract_spec::maybe_generate_sidecar(&root, &name, &wasm, print)?;

    Ok(BuildResult {
        project_name: name,
        project_root: root,
        wasm,
        spec_json,
        size_bytes: size,
        optimized: optimize,
        profile,
        cargo_stdout,
        cargo_stderr,
    })
}

/// Walk up from `start` to find the project root
/// (a directory with a Cargo.toml that depends on
/// the groundhog registry). Returns the root path
/// and the package name.
pub fn find_project(start: &Path) -> Result<(PathBuf, String), CliError> {
    let mut dir = if start.is_file() {
        start
            .parent()
            .map_or_else(|| start.to_path_buf(), Path::to_path_buf)
    } else {
        start.to_path_buf()
    };
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = fs::read_to_string(&cargo_toml)?;
            if contents.contains("registry = \"groundhog\"") {
                let manifest: CargoManifest =
                    toml::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;
                return Ok((dir, manifest.package.name));
            }
        }
        if !dir.pop() {
            return Err(CliError::NotAProject);
        }
    }
}

/// Like [`find_project`] but returns only the root path.
pub fn find_project_root(start: &Path) -> Result<PathBuf, CliError> {
    find_project(start).map(|(root, _)| root)
}

#[derive(serde::Deserialize)]
struct CargoManifest {
    package: Package,
}

#[derive(serde::Deserialize)]
struct Package {
    name: String,
}

/// Derive the WASM output path from a package name.
pub fn wasm_output_path(name: &str) -> PathBuf {
    let snake_name = crate::scaffold::to_snake(name);
    PathBuf::from("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(format!("{snake_name}.wasm"))
}
