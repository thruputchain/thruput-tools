use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::{CliError, Print};

/// Parsed sidecar emitted by the SDK tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractSpecSidecar {
    Interface(InterfaceSpecFile),
    Full(FullContractSpec),
}

/// Interface-only sidecar emitted by `groundhog-spec-tool`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InterfaceSpecFile {
    pub format: String,
    pub interfaces: Vec<InterfaceSpec>,
}

/// One registered interface in the sidecar.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InterfaceSpec {
    pub interface_name: String,
    pub interface_version: String,
}

/// Full contract spec emitted from `sdk::spec::HasContractSpec`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FullContractSpec {
    pub interface: String,
    pub version: String,
    pub methods: Vec<FullMethodSpec>,
}

/// One method in the full contract spec.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FullMethodSpec {
    pub name: String,
    pub entrypoint: String,
    pub args: Vec<FullArgSpec>,
    #[serde(rename = "return")]
    pub return_spec: TypeSpec,
}

/// One method argument in the full contract spec.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FullArgSpec {
    pub name: String,
    pub typespec: TypeSpec,
}

/// Recursive type description in the full contract spec.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TypeSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub fields: Vec<TypeFieldSpec>,
}

/// One field inside a structured typespec.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TypeFieldSpec {
    pub name: String,
    pub element_type: TypeSpec,
}

/// Path to the sidecar next to the compiled WASM.
pub fn sidecar_path_for_wasm(wasm: &Path) -> PathBuf {
    wasm.with_extension("spec.json")
}

/// Load any recognized sidecar if one exists.
pub fn load_sidecar(wasm: &Path) -> Result<Option<ContractSpecSidecar>, CliError> {
    let path = sidecar_path_for_wasm(wasm);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let value: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| CliError::Other(format!("invalid contract spec JSON: {e}")))?;

    if value.get("methods").is_some() {
        let spec = serde_json::from_value::<FullContractSpec>(value)
            .map_err(|e| CliError::Other(format!("invalid full contract spec JSON: {e}")))?;
        return Ok(Some(ContractSpecSidecar::Full(spec)));
    }

    if value.get("interfaces").is_some() {
        let spec = serde_json::from_value::<InterfaceSpecFile>(value)
            .map_err(|e| CliError::Other(format!("invalid interface spec JSON: {e}")))?;
        return Ok(Some(ContractSpecSidecar::Interface(spec)));
    }

    Err(CliError::Other("unknown contract spec JSON format".into()))
}

/// Load only the interface sidecar if present.
pub fn load_interface_sidecar(wasm: &Path) -> Result<Option<InterfaceSpecFile>, CliError> {
    match load_sidecar(wasm)? {
        Some(ContractSpecSidecar::Interface(spec)) => Ok(Some(spec)),
        Some(ContractSpecSidecar::Full(_)) | None => Ok(None),
    }
}

/// Load only the full contract spec if present.
pub fn load_full_spec(wasm: &Path) -> Result<Option<FullContractSpec>, CliError> {
    match load_sidecar(wasm)? {
        Some(ContractSpecSidecar::Full(spec)) => Ok(Some(spec)),
        Some(ContractSpecSidecar::Interface(_)) | None => Ok(None),
    }
}

/// Best-effort spec generation.
///
/// This always tries the SDK interface-rewrite step first so the built WASM
/// gets the registered interface section. If the project also exposes a
/// `spec` feature, the richer JSON spec replaces the sidecar contents.
pub fn maybe_generate_sidecar(
    project_root: &Path,
    package_name: &str,
    wasm: &Path,
    print: &Print,
) -> Result<Option<PathBuf>, CliError> {
    let spec_path = sidecar_path_for_wasm(wasm);

    let Some(tool) = find_spec_tool(project_root) else {
        return Ok(None);
    };

    print.status("Generating interface spec...");

    let success = match tool {
        SpecTool::Binary(binary) => Command::new(binary)
            .arg("rewrite")
            .arg("--package")
            .arg(package_name)
            .arg("--wasm")
            .arg(wasm)
            .arg("--json-out")
            .arg(&spec_path)
            .status()
            .map(|status| status.success())?,
        SpecTool::CargoManifest(manifest) => {
            let mut cmd = Command::new("cargo");
            cmd.arg("run")
                .arg("--quiet")
                .arg("--manifest-path")
                .arg(manifest)
                .arg("--")
                .arg("rewrite")
                .arg("--package")
                .arg(package_name)
                .arg("--wasm")
                .arg(wasm)
                .arg("--json-out")
                .arg(&spec_path);
            match cmd.status() {
                Ok(status) => status.success(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
                Err(err) => return Err(err.into()),
            }
        }
    };

    if !success {
        print.warn("Interface spec generation failed; continuing without spec sidecar");
        return Ok(None);
    }

    if maybe_generate_full_spec(project_root, package_name, &spec_path, print)? {
        return Ok(Some(spec_path));
    }

    Ok(spec_path.exists().then_some(spec_path))
}

#[derive(Debug, Clone)]
enum SpecTool {
    Binary(PathBuf),
    CargoManifest(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SdkDependency {
    version: String,
    registry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SdkSource {
    Path(PathBuf),
    Registry(SdkDependency),
}

fn maybe_generate_full_spec(
    project_root: &Path,
    package_name: &str,
    spec_path: &Path,
    print: &Print,
) -> Result<bool, CliError> {
    if !project_supports_spec_feature(project_root)? {
        return Ok(false);
    }

    let Some(client_ident) = find_exported_client_ident(project_root)? else {
        return Ok(false);
    };
    let sdk_source = find_local_sdk_path(project_root)
        .map(SdkSource::Path)
        .or_else(|| {
            load_sdk_dependency(project_root)
                .ok()
                .flatten()
                .map(SdkSource::Registry)
        });
    let Some(sdk_source) = sdk_source else {
        return Ok(false);
    };

    print.status("Generating full contract spec...");

    let helper_dir = project_root
        .join("target")
        .join("groundhog-cli-spec-export");
    std::fs::create_dir_all(helper_dir.join("src"))?;
    std::fs::write(
        helper_dir.join("Cargo.toml"),
        helper_manifest(project_root, package_name, &sdk_source),
    )?;
    std::fs::write(
        helper_dir.join("src/main.rs"),
        helper_main(package_name, &client_ident),
    )?;

    let mut cmd = Command::new("cargo");
    cmd.arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(helper_dir.join("Cargo.toml"));
    if let Some(lib_dir) = discover_local_ffi_lib_dir() {
        cmd.env("HOGD_FFI_LIB_DIR", &lib_dir)
            .env("DYLD_LIBRARY_PATH", &lib_dir)
            .env("LD_LIBRARY_PATH", &lib_dir);
    }
    let output = cmd.output()?;

    if !output.status.success() {
        print.warn("Full contract spec generation failed; keeping interface-only sidecar");
        return Ok(false);
    }

    std::fs::write(spec_path, &output.stdout)?;
    Ok(true)
}

fn helper_manifest(project_root: &Path, package_name: &str, sdk: &SdkSource) -> String {
    let project_path = toml_string(project_root.to_string_lossy().as_ref());
    let (sdk_dep, patch_block) = match sdk {
        SdkSource::Path(path) => (
            format!(
                "sdk = {{ path = {}, features = [\"spec\"] }}",
                toml_string(path.to_string_lossy().as_ref())
            ),
            format!(
                "\n[patch.\"sparse+https://groundhog.dev/crates/api/v1/crates/\"]\nsdk = {{ path = {} }}\n",
                toml_string(path.to_string_lossy().as_ref())
            ),
        ),
        SdkSource::Registry(sdk) => {
            let sdk_registry = sdk
                .registry
                .as_ref()
                .map(|registry| format!(", registry = {}", toml_string(registry)))
                .unwrap_or_default();
            (
                format!(
                    "sdk = {{ version = \"{}\", features = [\"spec\"]{} }}",
                    sdk.version, sdk_registry
                ),
                String::new(),
            )
        }
    };

    format!(
        r#"[package]
name = "groundhog-cli-spec-export"
version = "0.1.0"
edition = "2024"

[dependencies]
{}
{} = {{ path = {}, features = ["spec"] }}
"#,
        sdk_dep, package_name, project_path,
    ) + &patch_block
}

fn helper_main(package_name: &str, client_ident: &str) -> String {
    let crate_ident = package_name.replace('-', "_");
    format!(
        r#"use sdk::spec::HasContractSpec;

fn main() {{
    let spec = <{crate_ident}::{client_ident} as HasContractSpec>::contract_spec();
    print!("{{}}", spec.print_json_summary());
}}
"#
    )
}

fn toml_string(value: &str) -> String {
    format!("{value:?}")
}

fn project_supports_spec_feature(project_root: &Path) -> Result<bool, CliError> {
    let manifest = load_manifest(project_root)?;
    Ok(manifest
        .get("features")
        .and_then(|features| features.get("spec"))
        .is_some())
}

fn load_sdk_dependency(project_root: &Path) -> Result<Option<SdkDependency>, CliError> {
    let manifest = load_manifest(project_root)?;
    let Some(dependency) = manifest
        .get("dependencies")
        .and_then(|deps| deps.get("sdk"))
    else {
        return Ok(None);
    };

    if let Some(version) = dependency.as_str() {
        return Ok(Some(SdkDependency {
            version: version.to_string(),
            registry: None,
        }));
    }

    let Some(table) = dependency.as_table() else {
        return Ok(None);
    };

    let Some(version) = table.get("version").and_then(toml::Value::as_str) else {
        return Ok(None);
    };

    Ok(Some(SdkDependency {
        version: version.to_string(),
        registry: table
            .get("registry")
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned),
    }))
}

fn find_exported_client_ident(project_root: &Path) -> Result<Option<String>, CliError> {
    let lib_rs = project_root.join("src/lib.rs");
    let contents = match std::fs::read_to_string(&lib_rs) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let mut clients = contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("pub use ") || !line.ends_with(';') {
                return None;
            }
            let ident = line.rsplit("::").next()?.trim_end_matches(';').trim();
            ident.ends_with("Client").then(|| ident.to_string())
        })
        .collect::<Vec<_>>();

    clients.sort();
    clients.dedup();

    Ok((clients.len() == 1).then(|| clients.remove(0)))
}

fn load_manifest(project_root: &Path) -> Result<toml::Value, CliError> {
    let path = project_root.join("Cargo.toml");
    let contents = std::fs::read_to_string(path)?;
    toml::from_str(&contents).map_err(|e| CliError::Other(format!("invalid Cargo.toml: {e}")))
}

fn find_spec_tool(project_root: &Path) -> Option<SpecTool> {
    if let Some(path) = std::env::var_os("GROUNDHOG_SPEC_TOOL") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(SpecTool::Binary(path));
        }
    }

    if let Ok(binary) = which::which("groundhog-spec-tool") {
        return Some(SpecTool::Binary(binary));
    }

    if let Some(path) = std::env::var_os("GROUNDHOG_SPEC_TOOL_MANIFEST") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(SpecTool::CargoManifest(path));
        }
    }

    candidate_manifests(project_root)
        .into_iter()
        .find(|path| path.exists())
        .map(SpecTool::CargoManifest)
}

fn find_local_sdk_path(project_root: &Path) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GROUNDHOG_SDK_DIR") {
        let path = PathBuf::from(path);
        if path.join("Cargo.toml").exists() {
            return Some(path);
        }
    }

    candidate_sdk_manifests(project_root)
        .into_iter()
        .find(|path| path.exists())
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn discover_local_ffi_lib_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOGD_FFI_LIB_DIR") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    candidate_cli_targets()
        .into_iter()
        .flat_map(|target| {
            std::fs::read_dir(target.join("debug").join("build"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        })
        .map(|path| path.join("out"))
        .flat_map(|out| {
            std::fs::read_dir(&out)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        })
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hogd_lib_"))
        })
}

fn candidate_cli_targets() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let cli_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for ancestor in cli_manifest_dir.ancestors() {
        candidates.push(ancestor.join("target"));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn candidate_manifests(project_root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for ancestor in project_root.ancestors() {
        candidates.push(
            ancestor
                .join("sdk")
                .join("sdk_rs")
                .join("groundhog-spec-tool")
                .join("Cargo.toml"),
        );
    }

    let cli_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for ancestor in cli_manifest_dir.ancestors() {
        candidates.push(
            ancestor
                .join("sdk")
                .join("sdk_rs")
                .join("groundhog-spec-tool")
                .join("Cargo.toml"),
        );
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn candidate_sdk_manifests(project_root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for ancestor in project_root.ancestors() {
        candidates.push(ancestor.join("sdk").join("sdk_rs").join("Cargo.toml"));
    }

    let cli_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for ancestor in cli_manifest_dir.ancestors() {
        candidates.push(ancestor.join("sdk").join("sdk_rs").join("Cargo.toml"));
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ContractSpecSidecar, FullContractSpec, InterfaceSpecFile, TypeSpec, candidate_manifests,
        find_exported_client_ident, load_full_spec, load_interface_sidecar, sidecar_path_for_wasm,
    };

    #[test]
    fn sidecar_path_replaces_wasm_extension() {
        let path = sidecar_path_for_wasm(Path::new("/tmp/demo.wasm"));
        assert_eq!(path, Path::new("/tmp/demo.spec.json"));
    }

    #[test]
    fn parses_interface_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("demo.wasm");
        std::fs::write(&wasm, b"\0asm").unwrap();
        std::fs::write(
            sidecar_path_for_wasm(&wasm),
            r#"{"format":"gh_iface_v1","interfaces":[{"interface_name":"Counter","interface_version":"0.1.0"}]}"#,
        )
        .unwrap();

        let spec = load_interface_sidecar(&wasm)
            .expect("bug: sidecar load should succeed")
            .expect("bug: sidecar should exist");
        assert_eq!(
            spec,
            InterfaceSpecFile {
                format: "gh_iface_v1".into(),
                interfaces: vec![super::InterfaceSpec {
                    interface_name: "Counter".into(),
                    interface_version: "0.1.0".into(),
                }],
            }
        );
    }

    #[test]
    fn parses_full_contract_spec_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("demo.wasm");
        std::fs::write(&wasm, b"\0asm").unwrap();
        std::fs::write(
            sidecar_path_for_wasm(&wasm),
            r#"{"interface":"Counter","version":"0.1.0","methods":[{"name":"get","return":{"type":"unsigned hyper"},"entrypoint":"0x1234","args":[]}]}"#,
        )
        .unwrap();

        let spec = load_full_spec(&wasm)
            .expect("bug: full spec load should succeed")
            .expect("bug: full spec should exist");
        assert_eq!(
            spec,
            FullContractSpec {
                interface: "Counter".into(),
                version: "0.1.0".into(),
                methods: vec![super::FullMethodSpec {
                    name: "get".into(),
                    entrypoint: "0x1234".into(),
                    args: vec![],
                    return_spec: TypeSpec {
                        kind: "unsigned hyper".into(),
                        fields: vec![],
                    },
                }],
            }
        );
    }

    #[test]
    fn candidate_manifest_search_is_deduplicated() {
        let candidates = candidate_manifests(Path::new("/tmp/project"));
        let unique = candidates.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(candidates.len(), unique.len());
    }

    #[test]
    fn finds_single_exported_client() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub use implementation::CounterClient;\npub use implementation::Counter;\n",
        )
        .unwrap();

        let client = find_exported_client_ident(dir.path())
            .expect("bug: client lookup should succeed")
            .expect("bug: client should be found");
        assert_eq!(client, "CounterClient");
    }

    #[test]
    fn load_sidecar_distinguishes_formats() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("demo.wasm");
        std::fs::write(&wasm, b"\0asm").unwrap();
        std::fs::write(
            sidecar_path_for_wasm(&wasm),
            r#"{"format":"gh_iface_v1","interfaces":[{"interface_name":"Counter","interface_version":"0.1.0"}]}"#,
        )
        .unwrap();
        let sidecar = super::load_sidecar(&wasm)
            .expect("bug: sidecar should load")
            .expect("bug: sidecar should exist");
        assert!(matches!(sidecar, ContractSpecSidecar::Interface(_)));
    }
}
