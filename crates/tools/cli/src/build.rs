use crate::artifacts::{self, ArtifactOptions};
use crate::{CliError, Print};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const WASM_TARGET_TRIPLE: &str = "wasm32-unknown-unknown";
pub(crate) const RV32_TARGET_TRIPLE: &str = "riscv32im-unknown-none-elf";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildTarget {
    Wasm,
    Rv32,
}

impl BuildTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wasm => "wasm",
            Self::Rv32 => "rv32",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub project_name: String,
    pub project_root: PathBuf,
    /// Manifest directory of the built package. In a single-package project
    /// this equals `project_root`; in a workspace it is the member's own
    /// directory (e.g. `<workspace>/erc20`).
    #[serde(skip_serializing)]
    pub package_root: PathBuf,
    #[serde(skip_serializing)]
    pub target_directory: PathBuf,
    pub wasm: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_json: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_xdr: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bindings_ts: Option<PathBuf>,
    pub size_bytes: u64,
    pub optimized: bool,
    pub profile: crate::config::ResolvedProfile,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stderr: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rv32BuildResult {
    pub project_name: String,
    pub project_root: PathBuf,
    /// Manifest directory of the built package. In a single-package project
    /// this equals `project_root`; in a workspace it is the member's own
    /// directory.
    #[serde(skip_serializing)]
    pub package_root: PathBuf,
    #[serde(skip_serializing)]
    pub target_directory: PathBuf,
    pub elf: PathBuf,
    pub rv32: PathBuf,
    pub metadata: PathBuf,
    pub spec_json: Option<PathBuf>,
    pub spec_xdr: Option<PathBuf>,
    pub size_bytes: u64,
    pub target: BuildTarget,
    pub profile: crate::config::ResolvedProfile,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stderr: String,
}

/// Build the contract to WASM via `cargo build` with
/// cross-compilation to `wasm32-unknown-unknown`.
#[allow(clippy::too_many_arguments)]
pub fn run(
    package: Option<&str>,
    target: BuildTarget,
    optimize: bool,
    bindings: &[String],
    stack_size: Option<u32>,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if target == BuildTarget::Rv32 {
        if optimize {
            return Err(CliError::InvalidBuildOptions(
                "`--optimize` is only supported for the WASM build target".into(),
            ));
        }
        if !bindings.is_empty() {
            return Err(CliError::InvalidBuildOptions(
                "`--bindings` is only supported for the WASM build target".into(),
            ));
        }
        let result = build_rv32_project(
            package,
            cargo_args,
            artifacts,
            explicit_profile,
            stack_size,
            print,
        )?;
        print.json(&crate::result::CommandResult::new(
            cmd,
            json!({
                "project_name": result.project_name,
                "project_root": result.project_root,
                "target": result.target,
                "elf": result.elf,
                "rv32": result.rv32,
                "metadata": result.metadata,
                "spec_json": result.spec_json,
                "spec_xdr": result.spec_xdr,
                "size_bytes": result.size_bytes,
                "profile": result.profile,
                "cargo_stdout": result.cargo_stdout,
                "cargo_stderr": result.cargo_stderr,
            }),
        ))?;
        return Ok(());
    }

    if stack_size.is_some() {
        return Err(CliError::InvalidBuildOptions(
            "`--stack-size` is only supported for the RV32 build target".into(),
        ));
    }
    let result = build_project(
        package,
        optimize,
        cargo_args,
        artifacts,
        explicit_profile,
        bindings,
        print,
    )?;
    print.json(&crate::result::CommandResult::new(
        cmd,
        json!({
            "project_name": result.project_name,
            "project_root": result.project_root,
            "target": BuildTarget::Wasm,
            "wasm": result.wasm,
            "size_bytes": result.size_bytes,
            "optimized": result.optimized,
            "profile": result.profile,
            "cargo_stdout": result.cargo_stdout,
            "cargo_stderr": result.cargo_stderr,
            "spec_json": result.spec_json,
            "spec_xdr": result.spec_xdr,
            "bindings_ts": result.bindings_ts,
        }),
    ))?;
    Ok(())
}

/// Build the current project and return the artifact metadata without
/// printing a final command payload.
#[allow(clippy::too_many_lines)]
pub fn build_project(
    package: Option<&str>,
    optimize: bool,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    bindings: &[String],
    print: &Print,
) -> Result<BuildResult, CliError> {
    crate::require_cargo()?;

    let cwd = std::env::current_dir()?;
    let metadata = cargo_metadata_with_artifacts(artifacts)?;
    let pkg_name = resolve_package(&metadata, package, &cwd)?;
    let contract_toml = require_contract_toml(&metadata.workspace_root)?;

    // The built package's own directory. Equals `workspace_root` for a
    // single-package project, but is the member directory in a workspace —
    // which is where its `tests/` live.
    let package_root = metadata
        .packages
        .iter()
        .find(|p| p.name == pkg_name)
        .and_then(|p| p.manifest_path.parent())
        .map_or_else(|| metadata.workspace_root.clone(), Path::to_path_buf);

    if optimize {
        which::which("wasm-opt").map_err(|_| CliError::WasmOptNotFound)?;
    }

    let mut cmd = wasm_build_command(
        &metadata.workspace_root,
        &contract_toml,
        pkg_name,
        cargo_args,
    );

    artifacts::ensure_no_target_dir_passthrough_conflicts(
        artifacts,
        &metadata.workspace_root,
        cargo_args,
    )?;
    let profile = crate::config::resolve_profile(&metadata.workspace_root, explicit_profile)?;
    print.status(format_args!("Compiling {pkg_name} to WASM..."));
    artifacts.apply_to_command(&mut cmd);

    for arg in cargo_args {
        cmd.arg(arg);
    }

    let (cargo_stdout, cargo_stderr) = crate::run_cargo(&mut cmd, print)?;

    let raw_wasm = artifacts::cargo_wasm_output_path(&metadata.target_directory, pkg_name);
    if !raw_wasm.try_exists()? {
        return Err(CliError::WasmNotFound(raw_wasm.display().to_string()));
    }

    let wasm = artifacts::groundhog_wasm_output_path(&metadata.target_directory, pkg_name);
    if let Some(parent) = wasm.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&raw_wasm, &wasm)?;

    if optimize {
        print.status("Optimizing with wasm-opt...");
        let output = Command::new("wasm-opt")
            .arg("-mvp")
            .arg("--enable-bulk-memory")
            .arg("--enable-sign-ext")
            .arg("--enable-mutable-globals")
            .arg("--enable-simd")
            .arg("-Oz")
            .arg(&wasm)
            .arg("-o")
            .arg(&wasm)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(CliError::WasmOptFailed(stderr));
        }
    }

    print.status("Checking for contract spec...");
    let finalized = crate::contract_spec::finalize_spec(&wasm, print)?;
    let spec_json = finalized.as_ref().map(|f| f.json_path.clone());
    let spec_xdr = finalized.as_ref().map(|f| f.xdr_path.clone());

    let bindings_ts = if bindings.iter().any(|b| b == "ts") {
        if let Some(ref finalized) = finalized {
            let ts_path = wasm.with_extension("ts");
            let source_name = wasm
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            let ts = crate::codegen::generate_ts_bindings(&finalized.spec, source_name)?;
            fs::write(&ts_path, ts.as_bytes())?;
            print.status(format_args!("Wrote {}", ts_path.display()));
            Some(ts_path)
        } else {
            print.warn("no contract spec found — skipping TypeScript bindings");
            None
        }
    } else {
        None
    };

    print.status("Instrumenting WASM...");
    crate::wasm_instrument::instrument(&wasm)?;

    let size = fs::metadata(&wasm)?.len();
    print.status(format_args!("Built {} ({size} bytes)", wasm.display()));

    Ok(BuildResult {
        project_name: pkg_name.to_string(),
        project_root: metadata.workspace_root,
        package_root,
        target_directory: metadata.target_directory,
        wasm,
        size_bytes: size,
        optimized: optimize,
        spec_json,
        spec_xdr,
        bindings_ts,
        profile,
        cargo_stdout,
        cargo_stderr,
    })
}

/// Assemble the cargo invocation for the wasm contract build.
///
/// The build is scoped to the cdylib target with `--lib`: older project RV32
/// bin wrappers may satisfy `required-features = ["entrypoints"]` here and
/// would otherwise be built for wasm too — its output filename collides with
/// the cdylib's, and under lto=true/codegen-units=1 building the bin perturbs
/// the cdylib codegen (and thus gas). Cargo rejects a repeated `--lib`, so
/// one supplied via passthrough args is honored instead.
fn wasm_build_command(
    workspace_root: &Path,
    contract_toml: &Path,
    pkg_name: &str,
    cargo_args: &[String],
) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root)
        .arg("--config")
        .arg(contract_toml)
        .arg("build")
        .arg("--release")
        .arg("--no-default-features")
        .arg("--features")
        .arg("entrypoints")
        .arg("--target")
        .arg(WASM_TARGET_TRIPLE)
        .arg("-p")
        .arg(pkg_name);
    if !cargo_args.iter().any(|arg| arg == "--lib") {
        cmd.arg("--lib");
    }
    cmd
}

/// Parse and validate a `--stack-size` value: bytes as decimal or `0x` hex.
///
/// The value becomes the guest's `__stack_size` linker symbol, so it carries
/// the SDK linker-script contract: a positive multiple of 16 below the 64 KiB
/// data address space.
pub fn parse_stack_size(value: &str) -> Result<u32, String> {
    let parsed = match value.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => value.parse::<u32>(),
    }
    .map_err(|_| format!("expected bytes as decimal or 0x hex, got {value:?}"))?;
    if parsed == 0 || !parsed.is_multiple_of(16) {
        return Err(format!(
            "stack size {parsed} must be a positive multiple of 16"
        ));
    }
    if parsed as usize >= groundhog_rv32_artifact::MAX_MEMORY_BYTES {
        return Err(format!(
            "stack size {parsed} must be below the {} data address space",
            groundhog_rv32_artifact::MAX_MEMORY_BYTES
        ));
    }
    Ok(parsed)
}

/// True when a requested stack reserve visibly did not happen: the lowered
/// image stages data below the requested stack top, which the stack-first
/// linker script never produces. Old SDK linker scripts ignore the
/// `__stack_size` defsym silently, so this is the only build-time signal.
fn stack_size_defsym_ignored(
    initial_memory_start: usize,
    initial_memory_len: usize,
    stack_size: u32,
) -> bool {
    initial_memory_len > 0 && initial_memory_start < stack_size as usize
}

/// Build the current project to an RV32 artifact by compiling an ELF and
/// lowering it through the in-process RV32 lowerer library.
///
/// `stack_size` overrides the guest's `__stack_size` linker symbol via a
/// `--defsym` on the final link (requires an SDK with the stack-first linker
/// script; older scripts ignore the symbol, which is reported as a warning).
#[allow(clippy::too_many_lines)]
pub fn build_rv32_project(
    package: Option<&str>,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    stack_size: Option<u32>,
    print: &Print,
) -> Result<Rv32BuildResult, CliError> {
    crate::require_cargo()?;

    let cwd = std::env::current_dir()?;
    let metadata = cargo_metadata_with_artifacts(artifacts)?;
    let pkg_name = resolve_package(&metadata, package, &cwd)?;
    let rv32_feature = rv32_entrypoints_feature(&metadata, pkg_name);
    let package_root = metadata
        .packages
        .iter()
        .find(|p| p.name == pkg_name)
        .and_then(|p| p.manifest_path.parent())
        .map_or_else(|| metadata.workspace_root.clone(), Path::to_path_buf);

    artifacts::ensure_no_target_dir_passthrough_conflicts(
        artifacts,
        &metadata.workspace_root,
        cargo_args,
    )?;
    let profile = crate::config::resolve_profile(&metadata.workspace_root, explicit_profile)?;

    print.status(format_args!("Compiling {pkg_name} to RV32 ELF..."));

    // `--stack-size` switches to `cargo rustc`: its trailing flags append to
    // the final rustc invocation, composing with the project's own
    // .cargo/config.toml rustflags (RUSTFLAGS or --config would replace the
    // whole target rustflags list, losing the linker script).
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&metadata.workspace_root)
        .arg(if stack_size.is_some() {
            "rustc"
        } else {
            "build"
        })
        .arg("--release")
        .arg("--no-default-features")
        .arg("--features")
        .arg(rv32_feature)
        .arg("--target")
        .arg(RV32_TARGET_TRIPLE)
        .arg("-p")
        .arg(pkg_name)
        .arg("--bin")
        .arg(crate::scaffold::to_snake(pkg_name));

    artifacts.apply_to_command(&mut cmd);

    for arg in cargo_args {
        cmd.arg(arg);
    }
    if let Some(size) = stack_size {
        cmd.arg("--")
            .arg("-C")
            .arg(format!("link-arg=--defsym=__stack_size={size}"));
    }

    let (cargo_stdout, cargo_stderr) = crate::run_cargo(&mut cmd, print)?;

    let elf = artifacts::cargo_rv32_elf_output_path(&metadata.target_directory, pkg_name);
    if !elf.try_exists()? {
        return Err(CliError::Rv32ElfNotFound(elf.display().to_string()));
    }

    let rv32 = artifacts::groundhog_rv32_output_path(&metadata.target_directory, pkg_name);
    let lower_metadata =
        artifacts::groundhog_rv32_metadata_output_path(&metadata.target_directory, pkg_name);
    if let Some(parent) = rv32.parent() {
        fs::create_dir_all(parent)?;
    }

    print.status("Lowering RV32 ELF...");
    let elf_bytes = fs::read(&elf)?;
    let lowered = groundhog_rv32_lower::lower(&elf_bytes)?;
    if let Some(size) = stack_size
        && stack_size_defsym_ignored(
            lowered.initial_memory_start,
            lowered.initial_memory.len(),
            size,
        )
    {
        print.warn(format_args!(
            "--stack-size {size} appears to have had no effect: data is staged at \
             {:#x}, below the requested stack top. The project's SDK linker script \
             likely predates the stack-first layout (`__stack_size` support).",
            lowered.initial_memory_start
        ));
    }
    let lower_metadata_doc =
        groundhog_rv32_lower::render_metadata(&groundhog_rv32_lower::MetadataInput {
            elf: &elf_bytes,
            artifact: &lowered.artifact,
            code: &lowered.code,
            initial_memory: &lowered.initial_memory,
            initial_memory_start: lowered.initial_memory_start,
            memory_bytes: lowered.memory_bytes,
            rustc_version: rustc_version(),
            target_triple: Some(RV32_TARGET_TRIPLE.to_string()),
            build_flags: Some(rv32_build_flags(
                pkg_name,
                rv32_feature,
                cargo_args,
                stack_size,
            )),
        });
    fs::write(&rv32, &lowered.artifact)?;
    if let Err(error) = fs::write(&lower_metadata, lower_metadata_doc) {
        let _ = fs::remove_file(&rv32);
        return Err(error.into());
    }

    print.status("Checking for contract spec...");
    let finalized = crate::contract_spec::finalize_rv32_spec(&elf, &rv32, print)?;
    let spec_json = finalized.as_ref().map(|f| f.json_path.clone());
    let spec_xdr = finalized.as_ref().map(|f| f.xdr_path.clone());

    let size = fs::metadata(&rv32)?.len();
    print.status(format_args!("Built {} ({size} bytes)", rv32.display()));

    Ok(Rv32BuildResult {
        project_name: pkg_name.to_string(),
        project_root: metadata.workspace_root,
        package_root,
        target_directory: metadata.target_directory,
        elf,
        rv32,
        metadata: lower_metadata,
        spec_json,
        spec_xdr,
        size_bytes: size,
        target: BuildTarget::Rv32,
        profile,
        cargo_stdout,
        cargo_stderr,
    })
}

fn rv32_entrypoints_feature(metadata: &CargoMetadata, pkg_name: &str) -> &'static str {
    metadata
        .packages
        .iter()
        .find(|package| package.name == pkg_name)
        .and_then(|package| manifest_has_feature(&package.manifest_path, "rv32-entrypoints"))
        .unwrap_or("entrypoints")
}

fn manifest_has_feature(manifest_path: &Path, feature: &str) -> Option<&'static str> {
    let contents = fs::read_to_string(manifest_path).ok()?;
    let manifest = toml::from_str::<toml::Value>(&contents).ok()?;
    manifest
        .get("features")
        .and_then(|features| features.get(feature))
        .map(|_| "rv32-entrypoints")
}

fn rv32_build_flags(
    pkg_name: &str,
    rv32_feature: &str,
    cargo_args: &[String],
    stack_size: Option<u32>,
) -> String {
    let mut flags = vec![
        "cargo".to_string(),
        if stack_size.is_some() {
            "rustc"
        } else {
            "build"
        }
        .to_string(),
        "--release".to_string(),
        "--no-default-features".to_string(),
        "--features".to_string(),
        rv32_feature.to_string(),
        "--target".to_string(),
        RV32_TARGET_TRIPLE.to_string(),
        "-p".to_string(),
        pkg_name.to_string(),
        "--bin".to_string(),
        crate::scaffold::to_snake(pkg_name),
    ];
    flags.extend(cargo_args.iter().cloned());
    if let Some(size) = stack_size {
        flags.push("--".to_string());
        flags.push("-C".to_string());
        flags.push(format!("link-arg=--defsym=__stack_size={size}"));
    }
    flags.join(" ")
}

fn rustc_version() -> Option<String> {
    let output = Command::new("rustc").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
    cargo_metadata_with_artifacts(&ArtifactOptions::default())
}

pub fn cargo_metadata_with_artifacts(
    artifacts: &ArtifactOptions,
) -> Result<CargoMetadata, CliError> {
    let mut command = Command::new("cargo");
    command
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps");
    artifacts.apply_to_command(&mut command);

    let output = command
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
    if path.try_exists()? {
        Ok(path)
    } else {
        Err(CliError::ContractTomlNotFound(
            workspace_root.display().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CargoMetadata, MetadataPackage, parse_stack_size, rv32_entrypoints_feature,
        stack_size_defsym_ignored,
    };
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn parse_stack_size_accepts_decimal_and_hex() {
        assert_eq!(parse_stack_size("32768"), Ok(32768));
        assert_eq!(parse_stack_size("0x8000"), Ok(32768));
        assert_eq!(parse_stack_size("0x6000"), Ok(24576));
        assert_eq!(parse_stack_size("16"), Ok(16));
        assert_eq!(parse_stack_size("65520"), Ok(65520)); // last multiple of 16 below 64K
    }

    #[test]
    fn parse_stack_size_rejects_invalid_values() {
        for bad in [
            "0", "0x0", "24", "0x6008", "65536", "0x10000", "", "abc", "0x", "-16",
        ] {
            assert!(
                parse_stack_size(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn stack_size_defsym_ignored_detects_pre_stack_first_layouts() {
        // Data staged below the requested stack top: the linker script did not
        // honor __stack_size, so the reserve is not actually there.
        assert!(stack_size_defsym_ignored(0x8, 4, 0x6000));
        // Data staged at (or above) the stack top: layout honored the reserve.
        assert!(!stack_size_defsym_ignored(0x6000, 4, 0x6000));
        assert!(!stack_size_defsym_ignored(0x7000, 4, 0x6000));
        // No stored data at all: nothing to infer, stay quiet.
        assert!(!stack_size_defsym_ignored(0, 0, 0x6000));
    }

    #[test]
    fn rv32_feature_prefers_new_wrapper_feature_when_present() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let manifest = tmp.path().join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nentrypoints = []\nrv32-entrypoints = [\"entrypoints\"]\n",
        )
        .expect("bug: manifest should be written");
        let metadata = metadata_for(manifest);

        assert_eq!(
            rv32_entrypoints_feature(&metadata, "demo"),
            "rv32-entrypoints"
        );
    }

    #[test]
    fn rv32_feature_falls_back_for_legacy_entrypoints_only_manifests() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let manifest = tmp.path().join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nentrypoints = []\n",
        )
        .expect("bug: manifest should be written");
        let metadata = metadata_for(manifest);

        assert_eq!(rv32_entrypoints_feature(&metadata, "demo"), "entrypoints");
    }

    fn metadata_for(manifest_path: PathBuf) -> CargoMetadata {
        CargoMetadata {
            workspace_root: manifest_path
                .parent()
                .expect("bug: manifest should have parent")
                .to_path_buf(),
            target_directory: PathBuf::from("target"),
            packages: vec![MetadataPackage {
                name: "demo".into(),
                manifest_path,
            }],
        }
    }
}
