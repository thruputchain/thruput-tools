use std::path::{Path, PathBuf};

use groundhog_contract_spec::{ContractSpec, ContractSpecInterface};

use crate::codec::spec_string_text;
use crate::{CliError, Print};

/// One registered interface in the embedded contract spec.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterfaceSpec {
    pub interface_name: String,
    pub interface_version: String,
}

/// Path to the JSON sidecar next to the compiled WASM.
fn sidecar_path_for_wasm(wasm: &Path) -> PathBuf {
    wasm.with_extension("spec.json")
}

/// Load the canonical contract spec from the built WASM.
///
/// Spec model types come from `groundhog_contract_spec`; this module uses
/// `groundhog_spec_tool` only to extract or render them from compiled WASM.
pub fn load_contract_spec(wasm: &Path) -> Result<Option<ContractSpec>, CliError> {
    load_embedded_contract_spec(wasm)
}

/// Load the embedded `groundhogspecv0` custom section from a WASM file.
fn load_embedded_contract_spec(wasm: &Path) -> Result<Option<ContractSpec>, CliError> {
    let bytes = match std::fs::read(wasm) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    load_embedded_contract_spec_from_bytes(&bytes)
}

/// Extract the embedded `groundhogspecv0` custom section from raw WASM bytes.
fn load_embedded_contract_spec_from_bytes(bytes: &[u8]) -> Result<Option<ContractSpec>, CliError> {
    let rewritten = groundhog_spec_tool::rewrite::rewrite_wasm_with_spec(bytes)
        .map_err(|e| CliError::SpecFailed(format!("failed to read wasm contract spec: {e}")))?;
    Ok(rewritten.spec)
}

/// Load only the registered interface ids from a built contract artifact.
pub fn load_registered_interfaces(wasm: &Path) -> Result<Option<Vec<InterfaceSpec>>, CliError> {
    match load_contract_spec(wasm)? {
        Some(spec) => Ok(Some(registered_interfaces(&spec)?)),
        None => Ok(None),
    }
}

/// Return the registered interfaces declared by a contract spec.
fn registered_interfaces(spec: &ContractSpec) -> Result<Vec<InterfaceSpec>, CliError> {
    spec.interfaces.iter().map(interface_spec).collect()
}

/// Find every interface that exports the named method.
pub fn find_method_interfaces(
    spec: &ContractSpec,
    method: &str,
) -> Result<Vec<InterfaceSpec>, CliError> {
    let mut matches = Vec::new();

    for interface in spec.interfaces.iter() {
        let mut exports_method = false;
        for candidate in interface.methods.iter() {
            if spec_string_text(&candidate.name)? == method {
                exports_method = true;
                break;
            }
        }
        if exports_method {
            matches.push(interface_spec(interface)?);
        }
    }

    matches.sort();
    matches.dedup();
    Ok(matches)
}

fn interface_spec(interface: &ContractSpecInterface) -> Result<InterfaceSpec, CliError> {
    Ok(InterfaceSpec {
        interface_name: spec_string_text(&interface.name)?.to_owned(),
        interface_version: spec_string_text(&interface.vers)?.to_owned(),
    })
}

/// Finalize the contract spec embedded in the WASM binary.
///
/// Rewrites the WASM in-place (replacing raw spec fragments with a
/// canonical `groundhogspecv0` section) and writes a `.spec.json`
/// sidecar alongside the WASM for inspection and external tooling.
///
/// The WASM is rewritten before the sidecar file is written. If the
/// sidecar write fails, a re-run is safe because
/// `rewrite_wasm_with_spec` can extract the spec from an
/// already-canonical WASM.
///
/// Returns `Some(json_path)` on success, or `None` when the contract
/// contains no spec fragments.
pub fn finalize_spec(wasm: &Path, print: &Print) -> Result<Option<PathBuf>, CliError> {
    use groundhog_spec_tool::{render_spec_json, rewrite::rewrite_wasm_with_spec};

    let bytes = std::fs::read(wasm)
        .map_err(|e| CliError::Other(format!("failed to read wasm for spec finalization: {e}")))?;

    let rewritten =
        rewrite_wasm_with_spec(&bytes).map_err(|e| CliError::SpecFailed(e.to_string()))?;

    std::fs::write(wasm, &rewritten.wasm)
        .map_err(|e| CliError::Other(format!("failed to write rewritten wasm: {e}")))?;

    let Some(spec) = rewritten.spec else {
        print.warn("no contract spec found — skipping .spec.json");
        return Ok(None);
    };

    let spec_json_path = sidecar_path_for_wasm(wasm);

    let json_str = render_spec_json(&spec);
    std::fs::write(&spec_json_path, json_str.as_bytes()).map_err(|e| {
        CliError::Other(format!("failed to write {}: {e}", spec_json_path.display()))
    })?;
    print.status(format_args!("Wrote {}", spec_json_path.display()));

    Ok(Some(spec_json_path))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use groundhog_contract_spec::{
        ContractField, ContractSpec, ContractSpecInterface, ContractSpecMethod, ContractType,
    };

    use super::{
        InterfaceSpec, find_method_interfaces, load_contract_spec,
        load_embedded_contract_spec_from_bytes, registered_interfaces, sidecar_path_for_wasm,
    };

    #[test]
    fn sidecar_path_replaces_wasm_extension() {
        let path = sidecar_path_for_wasm(Path::new("/tmp/demo.wasm"));
        assert_eq!(path, Path::new("/tmp/demo.spec.json"));
    }

    #[test]
    fn loads_embedded_contract_spec_from_wasm_bytes() {
        let spec = sample_contract_spec();
        let wasm = wasm_with_custom_section(
            groundhog_spec_tool::FINAL_SECTION_NAME,
            &xdrs::txs::encode(&spec).expect("bug: spec should encode"),
        );

        let loaded = load_embedded_contract_spec_from_bytes(&wasm)
            .expect("bug: embedded spec should load")
            .expect("bug: embedded spec should exist");
        assert_eq!(loaded, spec);
    }

    #[test]
    fn loads_contract_spec_from_embedded_wasm_file() {
        let spec = sample_contract_spec();
        let dir = tempfile::tempdir().expect("bug: temp dir should be created");
        let wasm = dir.path().join("demo.wasm");
        std::fs::write(
            &wasm,
            wasm_with_custom_section(
                groundhog_spec_tool::FINAL_SECTION_NAME,
                &xdrs::txs::encode(&spec).expect("bug: spec should encode"),
            ),
        )
        .expect("bug: wasm fixture should be written");

        let loaded = load_contract_spec(&wasm)
            .expect("bug: embedded spec file should load")
            .expect("bug: embedded spec file should exist");
        assert_eq!(loaded, spec);
    }

    #[test]
    fn returns_registered_interfaces() {
        let interfaces =
            registered_interfaces(&sample_contract_spec()).expect("bug: interfaces should decode");
        assert_eq!(
            interfaces,
            vec![InterfaceSpec {
                interface_name: "Counter".into(),
                interface_version: "0.1.0".into(),
            }]
        );
    }

    #[test]
    fn finds_interfaces_for_method() {
        let matches = find_method_interfaces(&sample_contract_spec(), "increment")
            .expect("bug: method interfaces should resolve");
        assert_eq!(
            matches,
            vec![InterfaceSpec {
                interface_name: "Counter".into(),
                interface_version: "0.1.0".into(),
            }]
        );
    }

    #[test]
    fn finds_multiple_interfaces_for_method_in_sorted_order() {
        let matches = find_method_interfaces(&sample_multi_interface_contract_spec(), "get")
            .expect("bug: method interfaces should resolve");
        assert_eq!(
            matches,
            vec![
                InterfaceSpec {
                    interface_name: "Counter".into(),
                    interface_version: "0.1.0".into(),
                },
                InterfaceSpec {
                    interface_name: "Other".into(),
                    interface_version: "0.1.0".into(),
                },
            ]
        );
    }

    fn sample_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
                name: spec_string_bytes("Counter"),
                vers: spec_string_bytes("0.1.0"),
                throws: vec![].try_into().unwrap(),
                methods: vec![
                    ContractSpecMethod {
                        name: spec_string_bytes("get"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U64,
                    },
                    ContractSpecMethod {
                        name: spec_string_bytes("increment"),
                        args: vec![ContractField {
                            name: spec_string_bytes("amount"),
                            ty: ContractType::U64,
                        }]
                        .try_into()
                        .unwrap(),
                        return_type: ContractType::Void,
                    },
                ]
                .try_into()
                .unwrap(),
            }]
            .try_into()
            .unwrap(),
            type_defs: vec![].try_into().unwrap(),
        }
    }

    fn sample_multi_interface_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![
                ContractSpecInterface {
                    name: spec_string_bytes("Other"),
                    vers: spec_string_bytes("0.1.0"),
                    throws: vec![].try_into().unwrap(),
                    methods: vec![ContractSpecMethod {
                        name: spec_string_bytes("get"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U64,
                    }]
                    .try_into()
                    .unwrap(),
                },
                ContractSpecInterface {
                    name: spec_string_bytes("Counter"),
                    vers: spec_string_bytes("0.1.0"),
                    throws: vec![].try_into().unwrap(),
                    methods: vec![ContractSpecMethod {
                        name: spec_string_bytes("get"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U64,
                    }]
                    .try_into()
                    .unwrap(),
                },
            ]
            .try_into()
            .unwrap(),
            type_defs: vec![].try_into().unwrap(),
        }
    }

    fn spec_string_bytes(value: &str) -> groundhog_spec_tool::ContractSpecString {
        value.try_into().expect("bug: test string should fit")
    }

    fn wasm_with_custom_section(name: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        encode_uleb128(name.len(), &mut payload);
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(data);

        let mut wasm = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x00];
        encode_uleb128(payload.len(), &mut wasm);
        wasm.extend_from_slice(&payload);
        wasm
    }

    fn encode_uleb128(mut value: usize, out: &mut Vec<u8>) {
        loop {
            let mut byte = u8::try_from(value & 0x7f).expect("bug: uleb128 byte should fit");
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }
}
