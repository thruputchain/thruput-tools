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
    package: Option<&str>,
    optimize: bool,
    cargo_args: &[String],
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let result = build_project(package, optimize, cargo_args, explicit_profile, print)?;
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
    package: Option<&str>,
    optimize: bool,
    cargo_args: &[String],
    explicit_profile: Option<&str>,
    print: &Print,
) -> Result<BuildResult, CliError> {
    crate::require_cargo()?;

    let metadata = cargo_metadata()?;
    let cwd = std::env::current_dir()?;
    let pkg_name = resolve_package(&metadata, package, &cwd)?;
    let contract_toml = require_contract_toml(&metadata.workspace_root)?;

    if optimize {
        which::which("wasm-opt").map_err(|_| CliError::WasmOptNotFound)?;
    }

    print.status(format_args!("Compiling {pkg_name} to WASM..."));

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&metadata.workspace_root)
        .arg("--config")
        .arg(&contract_toml)
        .arg("build")
        .arg("--release")
        .arg("--no-default-features")
        .arg("--features")
        .arg("entrypoints")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("-p")
        .arg(pkg_name);

    for arg in cargo_args {
        cmd.arg(arg);
    }

    let (cargo_stdout, cargo_stderr) = crate::run_cargo(&mut cmd, print)?;

    let snake_name = crate::scaffold::to_snake(pkg_name);
    let wasm = metadata
        .target_directory
        .join("wasm32-unknown-unknown/release")
        .join(format!("{snake_name}.wasm"));

    let profile = crate::config::resolve_profile(&metadata.workspace_root, explicit_profile)?;

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
    let spec_json = crate::contract_spec::maybe_generate_sidecar(
        &metadata.workspace_root,
        pkg_name,
        &wasm,
        print,
    )?;
    if let Some(ref spec_json) = spec_json {
        print.status(format_args!("Spec {}", spec_json.display()));
    }

    Ok(BuildResult {
        project_name: pkg_name.to_string(),
        project_root: metadata.workspace_root,
        wasm,
        spec_json,
        size_bytes: size,
        optimized: optimize,
        profile,
        cargo_stdout,
        cargo_stderr,
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct CargoMetadata {
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    pub packages: Vec<MetadataPackage>,
}

#[derive(Debug, serde::Deserialize)]
pub struct MetadataPackage {
    pub name: String,
    pub manifest_path: PathBuf,
}

/// Resolve which package to build.
///
/// When `package` is `Some`, validates the name exists in the
/// metadata.  When `None`, picks the package whose manifest
/// directory is the longest prefix of `cwd`.
pub fn resolve_package<'a>(
    metadata: &'a CargoMetadata,
    package: Option<&'a str>,
    cwd: &Path,
) -> Result<&'a str, CliError> {
    if let Some(name) = package {
        if metadata.packages.iter().any(|p| p.name == name) {
            return Ok(name);
        }
        return Err(CliError::PackageNotFound(name.to_string()));
    }

    metadata
        .packages
        .iter()
        .filter(|p| {
            p.manifest_path
                .parent()
                .is_some_and(|dir| cwd.starts_with(dir))
        })
        .max_by_key(|p| p.manifest_path.parent().map_or(0, |d| d.as_os_str().len()))
        .map(|p| p.name.as_str())
        .ok_or(CliError::PackageRequired)
}

/// Run `cargo metadata` and parse the result.
pub fn cargo_metadata() -> Result<CargoMetadata, CliError> {
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .output()
        .map_err(|e| CliError::CargoMetadataFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::CargoMetadataFailed(stderr.trim().to_string()));
    }

    serde_json::from_slice(&output.stdout).map_err(|e| CliError::CargoMetadataFailed(e.to_string()))
}

/// Check that `.cargo/contract.toml` exists at the workspace root.
fn require_contract_toml(workspace_root: &Path) -> Result<PathBuf, CliError> {
    let path = workspace_root.join(".cargo/contract.toml");
    if path.exists() {
        Ok(path)
    } else {
        Err(CliError::ContractTomlNotFound(
            workspace_root.display().to_string(),
        ))
    }
}
