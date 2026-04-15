use std::path::{Path, PathBuf};

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

/// Finalize the contract spec embedded in the WASM binary.
///
/// Rewrites the WASM in-place (replacing raw spec fragments with a
/// canonical `groundhogspecv0` section) and writes standalone `.spec`
/// (XDR) and `.spec.json` files alongside the WASM.
///
/// The WASM is rewritten before the sidecar files are written.  If a
/// sidecar write fails, a re-run is safe because
/// `rewrite_wasm_with_spec` can extract the spec from an
/// already-canonical WASM.
///
/// Returns `(Some(spec_path), Some(json_path))` on success, or
/// `(None, None)` when the contract contains no spec fragments.
pub fn finalize_spec(
    wasm: &Path,
    print: &Print,
) -> Result<(Option<PathBuf>, Option<PathBuf>), CliError> {
    use groundhog_spec_tool::{render_spec_json, rewrite::rewrite_wasm_with_spec};

    let bytes = std::fs::read(wasm)
        .map_err(|e| CliError::Other(format!("failed to read wasm for spec finalization: {e}")))?;

    let rewritten =
        rewrite_wasm_with_spec(&bytes).map_err(|e| CliError::SpecFailed(e.to_string()))?;

    std::fs::write(wasm, &rewritten.wasm)
        .map_err(|e| CliError::Other(format!("failed to write rewritten wasm: {e}")))?;

    let Some(spec) = rewritten.spec else {
        print.warn("no contract spec found — skipping .spec and .spec.json");
        return Ok((None, None));
    };

    let spec_path = wasm.with_extension("spec");
    let spec_json_path = sidecar_path_for_wasm(wasm);

    let xdr_bytes = xdrs::txs::encode(&spec)
        .map_err(|e| CliError::SpecFailed(format!("XDR encoding failed: {e}")))?;
    std::fs::write(&spec_path, &xdr_bytes)?;
    print.status(format_args!("Wrote {}", spec_path.display()));

    let json_str = render_spec_json(&spec);
    std::fs::write(&spec_json_path, json_str.as_bytes())?;
    print.status(format_args!("Wrote {}", spec_json_path.display()));

    Ok((Some(spec_path), Some(spec_json_path)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ContractSpecSidecar, FullContractSpec, InterfaceSpecFile, TypeSpec, load_full_spec,
        load_interface_sidecar, sidecar_path_for_wasm,
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
