use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use groundhog_contract_spec::{ContractSpec, ContractSpecInterface};

use crate::codec::spec_string_text;
use crate::{CliError, Print};

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
/// - `.rv32a` — returns a targeted error naming the adjacent `.spec.xdr` sidecar.
/// - Anything else — returns an error.
pub fn load_contract_spec_from_file(path: &Path) -> Result<Option<ContractSpec>, CliError> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("xdr") => {
            let bytes = std::fs::read(path)?;
            Ok(Some(load_contract_spec_from_xdr(&bytes)?))
        }
        Some("rv32a") => Err(CliError::SpecFailed(format!(
            "RV32 artifacts do not embed contract specs; pass the sidecar at {}",
            spec_xdr_sidecar_path(path).display(),
        ))),
        _ => Err(CliError::SpecFailed(format!(
            "unsupported file extension for contract spec: {}; expected .xdr",
            path.display(),
        ))),
    }
}

/// Load the contract spec produced by the default build for a Cargo package.
///
/// RV32 stores its finalized spec in an XDR sidecar because the lowered
/// `.rv32a` artifact has no embedded-spec section. The sidecar is current only
/// when it is at least as new as the artifact.
pub fn load_built_contract_spec(
    target_dir: &Path,
    package_name: &str,
) -> Result<Option<ContractSpec>, CliError> {
    let rv32 = crate::artifacts::groundhog_rv32_output_path(target_dir, package_name);
    let rv32_spec = spec_xdr_sidecar_path(&rv32);
    let Some(rv32_modified) = file_modified(&rv32)? else {
        return Ok(None);
    };
    let Some(rv32_spec_modified) = file_modified(&rv32_spec)? else {
        return Ok(None);
    };
    if rv32_spec_modified < rv32_modified {
        return Ok(None);
    }
    let bytes = std::fs::read(&rv32_spec)?;
    load_contract_spec_from_xdr(&bytes).map(Some)
}

fn file_modified(path: &Path) -> Result<Option<std::time::SystemTime>, CliError> {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.modified().map(Some).map_err(Into::into),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn load_rv32_contract_spec_from_bytes(bytes: &[u8]) -> Result<Option<ContractSpec>, CliError> {
    use groundhog_spec_tool::{SpecSection, check_raw_spec_format, classify_section};

    let sections = groundhog_rv32_lower::elf::parse_elf(bytes)
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
                raw_fragments.extend(groundhog_spec_tool::parse_raw_fragments(data).map_err(
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
    // `groundhogspecv0` section must not fail the build when the raw fragments
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
    raw_fragments: &[groundhog_spec_tool::SpecFragment],
) -> Result<Option<ContractSpec>, CliError> {
    match groundhog_spec_tool::canonicalize_spec(raw_fragments) {
        Ok(spec) => {
            reject_duplicate_event_signatures(&spec)?;
            Ok(Some(spec))
        }
        Err(groundhog_spec_tool::SpecToolError::NoSpecFragments) => Ok(None),
        Err(e) => Err(CliError::SpecFailed(format!(
            "failed to canonicalize rv32 contract spec: {e}"
        ))),
    }
}

/// Reject a spec whose retained events do not all have distinct signatures.
///
/// An event's signature is topic 0 of every emission, and the only thing a
/// consumer has to tell events apart on the wire. It is hashed from (crate,
/// MAJOR, ident), which is less than an event's spec name distinguishes: two
/// events in different modules of one crate have distinct names and one
/// signature, so the spec would promise a distinction the wire cannot deliver.
///
/// This belongs in `groundhog-spec-tool` beside the equivalent rule for
/// exception selectors, and it is there — but this CLI takes that crate from
/// the registry, so the rule cannot reach a build until a release carrying it
/// is published and pinned here. Checking it in the CLI closes the window in
/// between, where an SDK that can express the collision ships against a build
/// system that cannot catch it.
///
/// Delete this once the pinned `groundhog-spec-tool` carries the rule; the
/// wording matches so the diagnostic does not change when it does.
fn reject_duplicate_event_signatures(spec: &ContractSpec) -> Result<(), CliError> {
    let mut by_signature: BTreeMap<&[u8], &str> = BTreeMap::new();
    for event in spec.events.iter() {
        let name = spec_string_text(&event.name)?;
        let signature: &[u8] = event.signature.as_ref();
        if let Some(first) = by_signature.insert(signature, name) {
            let hex = signature.iter().fold(String::new(), |mut acc, b| {
                let _ = write!(acc, "{b:02x}");
                acc
            });
            return Err(CliError::SpecFailed(format!(
                "failed to canonicalize rv32 contract spec: events share an \
                 event signature: {first} and {name} both hash to {hex}; they \
                 differ only in a module path, which the signature does not \
                 carry — rename one"
            )));
        }
    }
    Ok(())
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

    let json_str = groundhog_spec_tool::render_spec_json(&spec);
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

    use groundhog_contract_spec::{
        ContractField, ContractSpec, ContractSpecInterface, ContractSpecMethod, ContractType,
    };

    use super::{
        InterfaceSpec, canonicalize_rv32_raw_fragments, find_method_interfaces,
        load_built_contract_spec, load_contract_spec_from_file, load_contract_spec_from_xdr,
        registered_interfaces, reject_duplicate_event_signatures, remove_spec_sidecars,
        spec_json_sidecar_path, spec_xdr_sidecar_path,
    };

    #[test]
    fn spec_json_sidecar_path_replaces_artifact_extension() {
        let path = spec_json_sidecar_path(Path::new("/tmp/demo.rv32a"));
        assert_eq!(path, Path::new("/tmp/demo.spec.json"));
    }

    #[test]
    fn rv32_artifact_spec_error_points_to_xdr_sidecar() {
        let error = load_contract_spec_from_file(Path::new("/tmp/demo.rv32a"))
            .expect_err("bug: an RV32 artifact is not itself a contract spec");

        assert!(
            error.to_string().contains("/tmp/demo.spec.xdr"),
            "expected sidecar path in error, got: {error}"
        );
    }

    #[test]
    fn rv32_raw_fragments_without_interfaces_are_no_spec() {
        let fragments = [groundhog_spec_tool::SpecFragment::SpecTypeDef {
            type_def: groundhog_spec_tool::ContractTypeDef::TypeDefStruct {
                struct_def: groundhog_spec_tool::ContractStructDef {
                    name: spec_string_bytes("Payload"),
                    fields: vec![].try_into().unwrap(),
                },
            },
        }];

        let spec = canonicalize_rv32_raw_fragments(&fragments)
            .expect("bug: typedef-only raw fragments should not error");

        assert!(spec.is_none());
    }

    #[test]
    fn built_contract_spec_loads_current_rv32_sidecar() {
        let rv32_spec = sample_contract_spec();
        let dir = tempfile::tempdir().expect("bug: temp dir should be created");
        let target_dir = dir.path();
        let output_dir = target_dir.join("groundhog");
        std::fs::create_dir_all(&output_dir).expect("bug: output dir should be created");
        let rv32 = output_dir.join("demo_contract.rv32a");
        std::fs::write(&rv32, b"rv32 fixture").expect("bug: rv32 fixture should be written");
        set_modified(&rv32, 10);
        let rv32_spec_path = rv32.with_extension("spec.xdr");
        std::fs::write(
            &rv32_spec_path,
            xdrs::txs::encode(&rv32_spec).expect("bug: rv32 spec should encode"),
        )
        .expect("bug: rv32 spec sidecar should be written");
        set_modified(&rv32_spec_path, 10);

        let loaded = load_built_contract_spec(target_dir, "demo-contract")
            .expect("bug: current rv32 sidecar should load")
            .expect("bug: current rv32 sidecar spec should exist");
        assert_eq!(loaded, rv32_spec);
    }

    #[test]
    fn built_contract_spec_rejects_rv32_sidecar_older_than_artifact() {
        let dir = tempfile::tempdir().expect("bug: temp dir should be created");
        let target_dir = dir.path();
        let output_dir = target_dir.join("groundhog");
        std::fs::create_dir_all(&output_dir).expect("bug: output dir should be created");
        let rv32 = output_dir.join("demo_contract.rv32a");
        std::fs::write(&rv32, b"new rv32 fixture").expect("bug: rv32 fixture should be written");
        set_modified(&rv32, 20);
        let rv32_spec_path = spec_xdr_sidecar_path(&rv32);
        std::fs::write(
            &rv32_spec_path,
            xdrs::txs::encode(&sample_contract_spec()).expect("bug: spec should encode"),
        )
        .expect("bug: stale rv32 spec sidecar should be written");
        set_modified(&rv32_spec_path, 10);

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

    fn set_modified(path: &Path, seconds_since_epoch: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("bug: fixture should open for timestamp update");
        file.set_times(std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds_since_epoch),
        ))
        .expect("bug: fixture timestamp should update");
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
            events: vec![].try_into().unwrap(),
        }
    }

    #[test]
    fn rendered_spec_json_includes_events() {
        let mut spec = sample_contract_spec();
        spec.events = vec![groundhog_contract_spec::ContractEventDef {
            name: spec_string_bytes("PingEvent"),
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

        let json = groundhog_spec_tool::render_spec_json(&spec);
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
            events: vec![].try_into().unwrap(),
        }
    }

    fn spec_string_bytes(value: &str) -> groundhog_spec_tool::ContractSpecString {
        value.try_into().expect("bug: test string should fit")
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

    fn event(name: &str, signature: [u8; 32]) -> groundhog_contract_spec::ContractEventDef {
        groundhog_contract_spec::ContractEventDef {
            name: name.try_into().unwrap(),
            signature: signature.into(),
            topics: vec![].try_into().unwrap(),
            fields: vec![].try_into().unwrap(),
        }
    }

    fn spec_with_events(events: Vec<groundhog_contract_spec::ContractEventDef>) -> ContractSpec {
        ContractSpec {
            interfaces: vec![].try_into().unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: events.try_into().unwrap(),
        }
    }

    /// Two events the spec names apart and the wire cannot: distinct ids, one
    /// signature, which is topic 0 of every emission.
    #[test]
    fn duplicate_event_signatures_are_rejected() {
        let spec = spec_with_events(vec![
            event("evt@1::ledger::Updated", [7u8; 32]),
            event("evt@1::vault::Updated", [7u8; 32]),
        ]);

        let err = reject_duplicate_event_signatures(&spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("events share an event signature"), "{msg}");
        assert!(msg.contains("evt@1::ledger::Updated"), "{msg}");
        assert!(msg.contains("evt@1::vault::Updated"), "{msg}");
    }

    /// The ordinary case: two crates' events share an ident but not a
    /// signature, because the crate name is in the preimage.
    #[test]
    fn distinct_event_signatures_are_accepted() {
        let spec = spec_with_events(vec![
            event("a@1::Updated", [1u8; 32]),
            event("b@1::Updated", [2u8; 32]),
        ]);

        assert!(reject_duplicate_event_signatures(&spec).is_ok());
    }

    #[test]
    fn a_spec_with_no_events_is_accepted() {
        assert!(reject_duplicate_event_signatures(&spec_with_events(vec![])).is_ok());
    }
}
