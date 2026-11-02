use std::path::{Path, PathBuf};

use thruput_contract_spec::{ContractSpec, ContractSpecInterface};

use crate::codec::spec_string_text;
use crate::{CliError, Print};

/// Canonical TXS/XDR `ContractSpec` schema embedded in RV32 v6 artifacts.
pub const ARTIFACT_SPEC_SCHEMA_VERSION: u32 = 1;

/// One registered interface in the contract spec.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterfaceSpec {
    pub interface_name: String,
    pub interface_version: String,
}

/// Path to the JSON contract-spec sidecar next to a built artifact.
fn spec_json_sidecar_path(artifact: &Path) -> PathBuf {
    artifact.with_extension("spec.json")
}

/// Path to the XDR contract-spec sidecar next to a built artifact.
fn spec_xdr_sidecar_path(artifact: &Path) -> PathBuf {
    artifact.with_extension("spec.xdr")
}

/// XDR-decode raw bytes into a `ContractSpec`.
pub fn load_contract_spec_from_xdr(bytes: &[u8]) -> Result<ContractSpec, CliError> {
    xdrs::txs::decode(bytes)
        .map_err(|e| CliError::SpecFailed(format!("failed to decode .spec.xdr: {e}")))
}

/// Load a contract spec from a file, dispatching by extension.
///
/// - `.xdr` — XDR-decodes the file contents and returns `Some`.
/// - `.rv32a` — validates the container and loads its embedded spec, if present.
/// - Anything else — returns an error.
pub fn load_contract_spec_from_file(path: &Path) -> Result<Option<ContractSpec>, CliError> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("xdr") => {
            let bytes = std::fs::read(path)?;
            Ok(Some(load_contract_spec_from_xdr(&bytes)?))
        }
        Some("rv32a") => {
            let bytes = std::fs::read(path)?;
            load_artifact_spec_bytes(&bytes)
        }
        _ => Err(CliError::SpecFailed(format!(
            "unsupported file extension for contract spec: {}; expected .xdr or .rv32a",
            path.display(),
        ))),
    }
}

/// Read a supported spec from validated artifact bytes. This does not authenticate
/// an address-to-artifact association; callers use the enclosing contract hash.
pub(crate) fn load_artifact_spec_bytes(bytes: &[u8]) -> Result<Option<ContractSpec>, CliError> {
    let Some(spec) = thruput_rv32_artifact::artifact_spec(bytes)
        .map_err(|error| CliError::SpecFailed(format!("invalid artifact: {error}")))?
    else {
        return Ok(None);
    };
    if spec.schema_version != ARTIFACT_SPEC_SCHEMA_VERSION {
        return Err(CliError::SpecFailed(format!(
            "unsupported embedded spec schema {}",
            spec.schema_version
        )));
    }
    load_contract_spec_from_xdr(spec.bytes).map(Some)
}

/// Load the contract spec produced by the default build for a Cargo package.
///
/// The embedded spec is authoritative; adjacent sidecars are exports only.
pub fn load_built_contract_spec(
    target_dir: &Path,
    package_name: &str,
) -> Result<Option<ContractSpec>, CliError> {
    let rv32 = crate::artifacts::thruput_rv32_output_path(target_dir, package_name);
    load_artifact_contract_spec(&rv32)
}

/// Load the embedded spec from the selected artifact, if the artifact exists.
pub(crate) fn load_artifact_contract_spec(
    artifact: &Path,
) -> Result<Option<ContractSpec>, CliError> {
    match load_contract_spec_from_file(artifact) {
        Err(CliError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        result => result,
    }
}

fn load_rv32_contract_spec_from_bytes(bytes: &[u8]) -> Result<Option<ContractSpec>, CliError> {
    use thruput_spec_tool::{SpecSection, check_raw_spec_format, classify_section};

    let sections = thruput_rv32_lower::elf::parse_elf(bytes)
        .map_err(|e| CliError::SpecFailed(format!("failed to read rv32 ELF contract spec: {e}")))?;
    let mut raw_fragments = Vec::new();
    let mut final_data: Option<&[u8]> = None;

    for section in sections {
        // Classify by prefix so a raw section this build cannot read is
        // reported as version skew rather than skipped — skipping it yields
        // `Ok(None)`, which surfaces as "this contract has no spec": the
        // wrong diagnosis, and harder to act on than the skew message.
        let kind = classify_section(&section.name);
        if kind == SpecSection::Other {
            continue;
        }
        let start = section.offset as usize;
        let end = start
            .checked_add(section.size as usize)
            .ok_or_else(|| CliError::SpecFailed("rv32 ELF spec section size overflow".into()))?;
        let data = bytes.get(start..end).ok_or_else(|| {
            CliError::SpecFailed(format!(
                "rv32 ELF spec section {} extends past end of file",
                section.name
            ))
        })?;

        match kind {
            // The raw body is an unframed XDR stream, so the format check has
            // to happen before the first fragment byte is decoded: a mismatch
            // desynchronizes the parse instead of failing cleanly.
            SpecSection::Raw(format) => {
                check_raw_spec_format(format).map_err(CliError::RawSpecFormat)?;
                raw_fragments.extend(thruput_spec_tool::parse_raw_fragments(data).map_err(
                    |e| {
                        CliError::SpecFailed(format!("failed to parse rv32 raw contract spec: {e}"))
                    },
                )?);
            }
            SpecSection::Final => {
                if final_data.replace(data).is_some() {
                    return Err(CliError::SpecFailed(
                        "rv32 ELF contains multiple finalized contract spec sections".into(),
                    ));
                }
            }
            // Skipped above, before the section body is sliced: an
            // unrelated section (`.bss` in particular) has no file bytes to
            // slice.
            SpecSection::Other => {}
        }
    }

    // Raw fragments win over a finalized section, so decode the finalized
    // section only when there are no raw fragments. A stale or malformed
    // finalized section must not fail the build when the raw fragments
    // already fully describe the spec.
    if !raw_fragments.is_empty() {
        return canonicalize_rv32_raw_fragments(&raw_fragments);
    }

    match final_data {
        Some(data) => load_contract_spec_from_xdr(data).map(Some),
        None => Ok(None),
    }
}

fn canonicalize_rv32_raw_fragments(
    raw_fragments: &[thruput_spec_tool::SpecFragment],
) -> Result<Option<ContractSpec>, CliError> {
    match thruput_spec_tool::canonicalize_spec(raw_fragments) {
        Ok(spec) => Ok(Some(spec)),
        Err(thruput_spec_tool::SpecToolError::NoSpecFragments) => Ok(None),
        Err(e) => Err(CliError::SpecFailed(format!(
            "failed to canonicalize rv32 contract spec: {e}"
        ))),
    }
}

/// Load registered interfaces from the built RV32 contract spec.
pub fn load_built_registered_interfaces(
    target_dir: &Path,
    package_name: &str,
) -> Result<Option<Vec<InterfaceSpec>>, CliError> {
    match load_built_contract_spec(target_dir, package_name)? {
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

/// Result of spec finalization when the contract contains a spec.
#[derive(Debug)]
pub struct FinalizedSpec {
    /// The parsed contract spec.
    pub spec: ContractSpec,
    /// Canonical bytes shared by the artifact and exported XDR sidecar.
    pub xdr_bytes: Vec<u8>,
    /// Path to the `.spec.json` sidecar.
    pub json_path: PathBuf,
    /// Path to the `.spec.xdr` sidecar.
    pub xdr_path: PathBuf,
}

/// Finalize raw contract spec fragments emitted into an RV32 ELF and write sidecars
/// next to the lowered `.rv32a` artifact.
pub fn finalize_rv32_spec(
    elf: &Path,
    rv32_artifact: &Path,
    print: &Print,
) -> Result<Option<FinalizedSpec>, CliError> {
    let bytes = std::fs::read(elf).map_err(|e| {
        CliError::Other(format!(
            "failed to read rv32 ELF for spec finalization: {e}"
        ))
    })?;
    let Some(spec) = load_rv32_contract_spec_from_bytes(&bytes)? else {
        remove_spec_sidecars(rv32_artifact)?;
        print.warn("no contract spec found — skipping .spec.json");
        return Ok(None);
    };

    write_spec_sidecars(rv32_artifact, spec, print).map(Some)
}

fn write_spec_sidecars(
    artifact: &Path,
    spec: ContractSpec,
    print: &Print,
) -> Result<FinalizedSpec, CliError> {
    let spec_json_path = spec_json_sidecar_path(artifact);

    let json_str = thruput_spec_tool::render_spec_json(&spec);
    std::fs::write(&spec_json_path, json_str.as_bytes()).map_err(|e| {
        CliError::Other(format!("failed to write {}: {e}", spec_json_path.display()))
    })?;
    print.status(format_args!("Wrote {}", spec_json_path.display()));

    let spec_xdr_path = spec_xdr_sidecar_path(artifact);
    let xdr_bytes = xdrs::txs::encode(&spec)
        .map_err(|e| CliError::SpecFailed(format!("failed to encode .spec.xdr: {e}")))?;
    std::fs::write(&spec_xdr_path, &xdr_bytes).map_err(|e| {
        CliError::Other(format!("failed to write {}: {e}", spec_xdr_path.display()))
    })?;
    print.status(format_args!("Wrote {}", spec_xdr_path.display()));

    Ok(FinalizedSpec {
        spec,
        xdr_bytes,
        json_path: spec_json_path,
        xdr_path: spec_xdr_path,
    })
}

fn remove_spec_sidecars(artifact: &Path) -> Result<(), CliError> {
    for path in [
        spec_json_sidecar_path(artifact),
        spec_xdr_sidecar_path(artifact),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CliError::Other(format!(
                    "failed to remove stale contract spec {}: {err}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use thruput_contract_spec::{
        ContractField, ContractSpec, ContractSpecInterface, ContractSpecMethod, ContractType,
    };

    use super::{
        InterfaceSpec, canonicalize_rv32_raw_fragments, find_method_interfaces,
        load_built_contract_spec, load_contract_spec_from_file, load_contract_spec_from_xdr,
        registered_interfaces, remove_spec_sidecars, spec_json_sidecar_path, spec_xdr_sidecar_path,
    };

    #[test]
    fn spec_json_sidecar_path_replaces_artifact_extension() {
        let path = spec_json_sidecar_path(Path::new("/tmp/demo.rv32a"));
        assert_eq!(path, Path::new("/tmp/demo.spec.json"));
    }

    #[test]
    fn rv32_artifact_rejects_unknown_spec_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.rv32a");
        let artifact = thruput_rv32_artifact::build_artifact_with_spec(
            &[0; 4],
            &[],
            0,
            64,
            Some(thruput_rv32_artifact::ArtifactSpec {
                schema_version: 99,
                bytes: &[1],
            }),
        )
        .unwrap();
        std::fs::write(&path, artifact).unwrap();
        let error = load_contract_spec_from_file(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported embedded spec schema 99")
        );
    }

    #[test]
    fn rv32_raw_fragments_without_interfaces_are_no_spec() {
        let fragments = [thruput_spec_tool::SpecFragment::SpecTypeDef {
            type_def: thruput_spec_tool::ContractTypeDef::TypeDefStruct {
                struct_def: thruput_spec_tool::ContractStructDef {
                    name: spec_id("Payload"),
                    fields: vec![].try_into().unwrap(),
                },
            },
        }];

        let spec = canonicalize_rv32_raw_fragments(&fragments)
            .expect("bug: typedef-only raw fragments should not error");

        assert!(spec.is_none());
    }

    #[test]
    fn built_contract_spec_uses_embedded_bytes_not_sidecar() {
        let rv32_spec = sample_contract_spec();
        let dir = tempfile::tempdir().expect("bug: temp dir should be created");
        let target_dir = dir.path();
        let output_dir = target_dir.join("thruput");
        std::fs::create_dir_all(&output_dir).expect("bug: output dir should be created");
        let rv32 = output_dir.join("demo_contract.rv32a");
        let bytes = xdrs::txs::encode(&rv32_spec).unwrap();
        let artifact = thruput_rv32_artifact::build_artifact_with_spec(
            &[0; 4],
            &[],
            0,
            64,
            Some(thruput_rv32_artifact::ArtifactSpec {
                schema_version: 1,
                bytes: &bytes,
            }),
        )
        .unwrap();
        std::fs::write(&rv32, artifact).unwrap();
        let rv32_spec_path = rv32.with_extension("spec.xdr");
        std::fs::write(&rv32_spec_path, b"unrelated invalid sidecar")
            .expect("bug: rv32 spec sidecar should be written");

        let loaded = load_built_contract_spec(target_dir, "demo-contract")
            .expect("bug: embedded spec should load")
            .expect("bug: embedded spec should exist");
        assert_eq!(loaded, rv32_spec);
    }

    #[test]
    fn spec_less_artifact_does_not_fall_back_to_sidecar() {
        let dir = tempfile::tempdir().expect("bug: temp dir should be created");
        let target_dir = dir.path();
        let output_dir = target_dir.join("thruput");
        std::fs::create_dir_all(&output_dir).expect("bug: output dir should be created");
        let rv32 = output_dir.join("demo_contract.rv32a");
        let artifact = thruput_rv32_artifact::build_artifact(&[0; 4], &[], 0, 64).unwrap();
        std::fs::write(&rv32, artifact).unwrap();
        let rv32_spec_path = spec_xdr_sidecar_path(&rv32);
        std::fs::write(
            &rv32_spec_path,
            xdrs::txs::encode(&sample_contract_spec()).expect("bug: spec should encode"),
        )
        .expect("bug: stale rv32 spec sidecar should be written");

        let loaded = load_built_contract_spec(target_dir, "demo-contract")
            .expect("bug: stale sidecar lookup should not error");

        assert!(loaded.is_none());
    }

    #[test]
    fn removes_stale_spec_sidecars() {
        let dir = tempfile::tempdir().expect("bug: temp dir should be created");
        let artifact = dir.path().join("demo_contract.rv32a");
        let json = spec_json_sidecar_path(&artifact);
        let xdr = spec_xdr_sidecar_path(&artifact);
        std::fs::write(&json, b"stale json").expect("bug: json sidecar should be written");
        std::fs::write(&xdr, b"stale xdr").expect("bug: xdr sidecar should be written");

        remove_spec_sidecars(&artifact).expect("bug: stale sidecars should be removed");

        assert!(!json.exists());
        assert!(!xdr.exists());
        remove_spec_sidecars(&artifact).expect("bug: removing absent sidecars should succeed");
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
                        effect: None,
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
                        effect: None,
                    },
                ]
                .try_into()
                .unwrap(),
            }]
            .try_into()
            .unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        }
    }

    #[test]
    fn rendered_spec_json_includes_events() {
        let mut spec = sample_contract_spec();
        spec.events = vec![thruput_contract_spec::ContractEventDef {
            name: spec_id("PingEvent"),
            signature: [0xAB; 32].into(),
            topics: vec![ContractField {
                name: spec_string_bytes("tag"),
                ty: ContractType::U64,
            }]
            .try_into()
            .unwrap(),
            fields: vec![].try_into().unwrap(),
        }]
        .try_into()
        .unwrap();

        let json = thruput_spec_tool::render_spec_json(&spec);
        assert!(
            json.contains("\"events\""),
            "rendered spec JSON missing events: {json}"
        );
        assert!(
            json.contains("\"PingEvent\""),
            "rendered spec JSON missing event name: {json}"
        );
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
                        effect: None,
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
                        effect: None,
                    }]
                    .try_into()
                    .unwrap(),
                },
            ]
            .try_into()
            .unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        }
    }

    fn spec_string_bytes(value: &str) -> thruput_spec_tool::ContractSpecString {
        value.try_into().expect("bug: test string should fit")
    }

    /// A fixture id: one crate, one module, so the declared name is what
    /// tells these types apart.
    fn spec_id(declared: &str) -> thruput_contract_spec::ContractSpecId {
        thruput_contract_spec::ContractSpecId {
            crate_name: spec_string_bytes("demo"),
            major: spec_string_bytes("1"),
            module_path: spec_string_bytes("demo"),
            declared: spec_string_bytes(declared),
        }
    }

    #[test]
    fn loads_contract_spec_from_xdr_bytes() {
        let spec = sample_contract_spec();
        let xdr_bytes = xdrs::txs::encode(&spec).expect("bug: spec should encode");
        let loaded = load_contract_spec_from_xdr(&xdr_bytes).expect("bug: should decode");
        assert_eq!(loaded, spec);
    }

    #[test]
    fn loads_contract_spec_from_xdr_file() {
        let spec = sample_contract_spec();
        let dir = tempfile::tempdir().expect("bug: temp dir");
        let xdr_path = dir.path().join("demo.spec.xdr");
        let xdr_bytes = xdrs::txs::encode(&spec).expect("bug: should encode");
        std::fs::write(&xdr_path, &xdr_bytes).expect("bug: should write");
        let loaded = load_contract_spec_from_file(&xdr_path)
            .expect("bug: should load")
            .expect("bug: should exist");
        assert_eq!(loaded, spec);
    }
}
