use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use groundhog_rpc_api_types::receipt::ExecStatus;
use groundhog_signing::{
    BuildCallTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams, Signer,
    build_call_transaction, sign_transaction,
};
use groundhog_spec_tool::{
    ContractEnumDef, ContractField, ContractSpecMethod, ContractSpecString, ContractStructDef,
    ContractType, ContractTypeDef, ContractUnionCase, ContractUnionCaseType, ContractUnionDef,
};
use groundhog_xdr::auth_claim::AuthClaim;
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
use serde_json::json;
use sha2::{Digest, Sha256};
use xdrs::{Reader as _, Writer as _};

use crate::artifacts::ArtifactOptions;
use crate::build::{cargo_metadata_with_artifacts, resolve_package};
use crate::config::{find_project_config, resolve_default_key, resolve_profile};
use crate::contract_spec::{
    InterfaceSpec, find_method_interfaces, load_contract_spec, load_registered_interfaces,
};
use crate::keys::load_local_signer;
use crate::result::CommandResult;
use crate::tx_utils::{
    format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height, submit_and_poll,
};
use crate::{CliError, Print};

/// Wallet `do_action` entrypoint ID (from dev-wallet WASM exports).
const WALLET_DO_ACTION_ENTRYPOINT: [u8; 8] = 0x1984_6D75_0FAD_81D6_u64.to_be_bytes();

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectorSource {
    Spec(SpecMethodMetadata),
    Local(ContractMetadata),
    Explicit(ContractEntrypointID),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedMethodMetadata {
    Spec(SpecMethodMetadata),
    Local(ContractMetadata),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecMethodMetadata {
    interface_name: String,
    version: String,
    method: ContractSpecMethod,
    type_defs: Vec<ContractTypeDef>,
}

/// Call a method on a deployed contract.
#[allow(clippy::too_many_lines)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
    artifacts: &ArtifactOptions,
    profile: &str,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let resolved = resolve_profile(&cwd, Some(profile))?;

    let rpc_url = resolved.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;

    // Resolve contract address (alias or literal)
    let contract_address_hex = resolved.resolve_contract(contract);
    let contract_address = parse_contract_address(&contract_address_hex)?;

    // Resolve signing key
    let key_name = resolve_default_key(&cwd, Some(profile))?.ok_or(CliError::MissingSigningKey)?;
    let signer = load_local_signer(&key_name)?;

    // Resolve the selector from the embedded contract spec when possible.
    let selector = resolve_method_entrypoint(&cwd, method, artifacts)?;
    let (entrypoint, metadata) = match selector {
        SelectorSource::Spec(metadata) => (
            contract_entrypoint_id(&metadata.interface_name, &metadata.version, method),
            Some(ResolvedMethodMetadata::Spec(metadata)),
        ),
        SelectorSource::Local(metadata) => (
            contract_entrypoint_id(&metadata.interface_name, &metadata.version, method),
            Some(ResolvedMethodMetadata::Local(metadata)),
        ),
        SelectorSource::Explicit(entrypoint) => (entrypoint, None),
    };

    // Encode calldata
    let calldata = encode_calldata(args, metadata.as_ref())?;

    print.status(format_args!(
        "Calling {method} on {contract_address_hex} via {rpc_url}..."
    ));

    // Build and sign — invokes the sender's wallet contract with do_action.
    let pubkey = signer.public_key();
    let account = groundhog_signing::PubkeyAccount::new(
        pubkey,
        groundhog_signing::DEFAULT_PUBKEY_ACCOUNT_NONCE,
    );
    let wallet_address = account
        .address()
        .map_err(|e| CliError::Other(e.to_string()))?;

    // Encode the inner invocation (TXS format, matching wallet's txs::decode):
    // (contract_address, method_entrypoint, gas_limit, raw_calldata)
    let inner_gas_limit: u64 = DEFAULT_TX_GAS_LIMIT;
    let inner_invocation = xdrs::txs::encode(&(
        contract_address,
        entrypoint,
        inner_gas_limit,
        calldata.as_slice(),
    ))
    .map_err(|e| CliError::Other(format!("failed to encode inner invocation: {e}")))?;

    // Outer calldata is CXS-encoded — the #[contract] macro deserializes
    // do_action's &[u8] parameter with xdrs::cxs::decode.
    let do_action_calldata = xdrs::cxs::encode(&inner_invocation.as_slice())
        .map_err(|e| CliError::Other(format!("failed to CXS-encode do_action calldata: {e}")))?;

    // Auth claim: kInvokedAddressSignature at key slot 0.
    let auth_claims: groundhog_xdr::auth_claim::AuthClaimData =
        vec![AuthClaim::kInvokedAddressSignature {
            invoked_address_signature_id: 0,
        }]
        .try_into()
        .map_err(|e: xdr_common::Error| CliError::Other(e.to_string()))?;

    let wallet_entrypoint =
        groundhog_xdr::contract::ContractEntrypointID::from(WALLET_DO_ACTION_ENTRYPOINT);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(rpc_url).await })?;

    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: wallet_address,
        entrypoint: wallet_entrypoint,
        calldata: do_action_calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce: nonce_from_timestamp(),
        auth_claims,
    });

    let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
        .map_err(|e| CliError::Other(e.to_string()))?;
    let tx_xdr = groundhog_signing::encode_signed_transaction_xdr(&signed_tx)
        .map_err(|e| CliError::Other(e.to_string()))?;
    let tx_hex = format!("0x{}", hex::encode(&tx_xdr));

    // Submit via JSON-RPC
    let (tx_hash, receipt) = rt.block_on(async { submit_and_poll(rpc_url, &tx_hex).await })?;
    let tx_hash_formatted = format_tx_hash(&tx_hash);
    let status_summary = summarize_exec_status(&receipt.status);
    let decoded_return = receipt
        .return_data
        .as_ref()
        .and_then(|data| decode_return_value(data.0.as_slice(), metadata.as_ref()));

    // Print result
    match &receipt.status {
        ExecStatus::Ok => {
            if let Some(ref value) = decoded_return {
                print.status(format_args!("Call succeeded: {method} -> {value}"));
            } else {
                print.status(format_args!("Call succeeded: {method}"));
            }
            print.status(format_args!("  contract: {contract_address_hex}"));
            print.status(format_args!("  tx: {tx_hash_formatted}"));
            print.status(format_args!("  height: {}", *receipt.height));
            if decoded_return.is_none()
                && let Some(ref data) = receipt.return_data
                && !data.0.is_empty()
            {
                print.status(format_args!("  return: 0x{}", hex::encode(&data.0)));
            }
        }
        ExecStatus::Reverted { .. } => {
            print.warn(format_args!("Call {status_summary}"));
            print.status(format_args!("  method: {method}"));
            print.status(format_args!("  contract: {contract_address_hex}"));
            print.status(format_args!("  tx: {tx_hash_formatted}"));
            print.status(format_args!("  height: {}", *receipt.height));
        }
        other => {
            print.warn(format_args!("Call {}", summarize_exec_status(other)));
            print.status(format_args!("  method: {method}"));
            print.status(format_args!("  contract: {contract_address_hex}"));
            print.status(format_args!("  tx: {tx_hash_formatted}"));
            print.status(format_args!("  height: {}", *receipt.height));
        }
    }

    print.json(&CommandResult::new(
        cmd,
        json!({
            "tx_hash": tx_hash_formatted,
            "contract": contract_address_hex,
            "method": method,
            "height": receipt.height,
            "status": receipt.status,
            "status_summary": status_summary,
            "return_data": receipt.return_data,
            "return_value": decoded_return,
        }),
    ))?;

    Ok(())
}

fn parse_contract_address(hex_str: &str) -> Result<ContractAddress, CliError> {
    Ok(ContractAddress::from(parse_fixed_hex::<32>(
        hex_str,
        "contract address",
    )?))
}

fn explicit_entrypoint_id(method: &str) -> Result<Option<ContractEntrypointID>, CliError> {
    let Some(hex_str) = method
        .strip_prefix("0x")
        .or_else(|| method.strip_prefix("0X"))
    else {
        return Ok(None);
    };
    if hex_str.len() != 16 {
        return Err(CliError::Other(
            "explicit entrypoint IDs must be 8 bytes / 16 hex characters".into(),
        ));
    }
    let bytes = hex::decode(hex_str)
        .map_err(|e| CliError::Other(format!("invalid explicit entrypoint hex: {e}")))?;
    let id: [u8; 8] = bytes
        .try_into()
        .map_err(|_| CliError::Other("entrypoint ID must be exactly 8 bytes".into()))?;
    Ok(Some(ContractEntrypointID::from(id)))
}

fn resolve_method_entrypoint(
    cwd: &Path,
    method: &str,
    artifacts: &ArtifactOptions,
) -> Result<SelectorSource, CliError> {
    if let Some(entrypoint) = explicit_entrypoint_id(method)? {
        return Ok(SelectorSource::Explicit(entrypoint));
    }
    if let Some(metadata) = resolve_spec_method(cwd, method, artifacts)? {
        return Ok(SelectorSource::Spec(metadata));
    }
    match resolve_local_contract_metadata(cwd, method, artifacts)? {
        Some(metadata) => Ok(SelectorSource::Local(metadata)),
        None => Err(CliError::Other(
            "cannot resolve the contract method from the local project or embedded contract spec. \
             Run `groundhog build` from inside the project that defines the contract, or pass an \
             explicit 0x-prefixed 8-byte entrypoint ID."
                .into(),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContractMetadata {
    interface_name: String,
    version: String,
    source: PathBuf,
    method: MethodMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MethodMetadata {
    arg_types: Vec<String>,
    return_type: Option<String>,
}

fn resolve_spec_method(
    cwd: &Path,
    method: &str,
    artifacts: &ArtifactOptions,
) -> Result<Option<SpecMethodMetadata>, CliError> {
    let Ok(metadata) = cargo_metadata_with_artifacts(artifacts) else {
        return Ok(None);
    };
    let Ok(pkg_name) = resolve_package(&metadata, None, cwd) else {
        return Ok(None);
    };
    let wasm = crate::artifacts::groundhog_wasm_output_path(&metadata.target_directory, pkg_name);
    let Some(spec) = load_contract_spec(&wasm)? else {
        return Ok(None);
    };

    let interface_matches = find_method_interfaces(&spec, method)?;
    if interface_matches.is_empty() {
        return Ok(None);
    }
    if interface_matches.len() > 1 {
        let descriptions = interface_matches
            .iter()
            .map(|candidate| {
                format!(
                    "{} v{}",
                    candidate.interface_name, candidate.interface_version
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::Other(format!(
            "method `{method}` matches multiple interfaces in the embedded contract spec: \
             {descriptions}"
        )));
    }

    let matched = interface_matches
        .into_iter()
        .next()
        .expect("bug: interface match count should be non-zero");
    let mut methods = Vec::new();

    for interface in spec.interfaces.iter() {
        let interface_name = spec_string_text(&interface.name)?;
        let version = spec_string_text(&interface.vers)?;
        if interface_name != matched.interface_name || version != matched.interface_version {
            continue;
        }
        for candidate in interface.methods.iter() {
            if spec_string_text(&candidate.name)? == method {
                methods.push(candidate.clone());
            }
        }
    }

    if methods.is_empty() {
        return Err(CliError::SpecFailed(format!(
            "embedded contract spec advertised `{}` v{} for method `{method}` but the method was \
             not found",
            matched.interface_name, matched.interface_version
        )));
    }
    if methods.len() > 1 {
        return Err(CliError::SpecFailed(format!(
            "embedded contract spec defines `{}` v{}::{method} multiple times",
            matched.interface_name, matched.interface_version
        )));
    }

    Ok(Some(SpecMethodMetadata {
        interface_name: matched.interface_name,
        version: matched.interface_version,
        method: methods.remove(0),
        type_defs: spec.type_defs.iter().cloned().collect(),
    }))
}

fn resolve_local_contract_metadata(
    cwd: &Path,
    method: &str,
    artifacts: &ArtifactOptions,
) -> Result<Option<ContractMetadata>, CliError> {
    let Some(config_path) = find_project_config(cwd) else {
        return Ok(None);
    };
    let Some(project_root) = config_path.parent() else {
        return Ok(None);
    };
    let interface_specs = load_project_registered_interfaces(project_root, artifacts)?;
    resolve_local_contract_metadata_in_project(project_root, method, interface_specs.as_deref())
}

#[cfg(test)]
fn resolve_local_contract_metadata_for_interface(
    cwd: &Path,
    method: &str,
    interface_name: &str,
    version: &str,
) -> Result<Option<ContractMetadata>, CliError> {
    let Some(config_path) = find_project_config(cwd) else {
        return Ok(None);
    };
    let Some(project_root) = config_path.parent() else {
        return Ok(None);
    };
    let interface_specs = [InterfaceSpec {
        interface_name: interface_name.to_owned(),
        interface_version: version.to_owned(),
    }];
    resolve_local_contract_metadata_in_project(project_root, method, Some(&interface_specs))
}

fn resolve_local_contract_metadata_in_project(
    project_root: &Path,
    method: &str,
    interface_specs: Option<&[InterfaceSpec]>,
) -> Result<Option<ContractMetadata>, CliError> {
    let package_version = load_package_version(project_root)?;
    let src_root = project_root.join("src");
    if !src_root.exists() {
        return Ok(None);
    }

    let mut candidates = Vec::new();
    collect_contract_metadata(&src_root, method, &package_version, &mut candidates)?;
    candidates = filter_candidates_by_interfaces(candidates, interface_specs);

    if candidates.is_empty() {
        return Ok(None);
    }

    candidates.sort_by(|a, b| {
        (&a.interface_name, &a.version, &a.source).cmp(&(&b.interface_name, &b.version, &b.source))
    });
    candidates.dedup();

    if candidates.len() == 1 {
        return Ok(candidates.into_iter().next());
    }

    let descriptions = candidates
        .iter()
        .map(|candidate| {
            format!(
                "{} v{} ({})",
                candidate.interface_name,
                candidate.version,
                candidate.source.display()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    Err(CliError::Other(format!(
        "method `{method}` matches multiple local contracts: {descriptions}"
    )))
}

fn load_project_registered_interfaces(
    project_root: &Path,
    artifacts: &ArtifactOptions,
) -> Result<Option<Vec<crate::contract_spec::InterfaceSpec>>, CliError> {
    let metadata = cargo_metadata_with_artifacts(artifacts)?;
    let pkg_name = resolve_package(&metadata, None, project_root)?;
    let wasm = crate::artifacts::groundhog_wasm_output_path(&metadata.target_directory, pkg_name);
    load_registered_interfaces(&wasm)
}

fn filter_candidates_by_interfaces(
    mut candidates: Vec<ContractMetadata>,
    interface_specs: Option<&[crate::contract_spec::InterfaceSpec]>,
) -> Vec<ContractMetadata> {
    let Some(interface_specs) = interface_specs else {
        return candidates;
    };
    if interface_specs.is_empty() {
        return candidates;
    }

    candidates.retain(|candidate| {
        interface_specs.iter().any(|interface| {
            interface.interface_name == candidate.interface_name
                && interface.interface_version == candidate.version
        })
    });
    candidates
}

fn load_package_version(project_root: &Path) -> Result<String, CliError> {
    let cargo_toml = project_root.join("Cargo.toml");
    let contents = std::fs::read_to_string(&cargo_toml)?;
    let value: toml::Value = toml::from_str(&contents)
        .map_err(|e| CliError::Other(format!("invalid Cargo.toml: {e}")))?;
    value
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CliError::Other("Cargo.toml is missing package.version".into()))
}

fn collect_contract_metadata(
    dir: &Path,
    method: &str,
    package_version: &str,
    out: &mut Vec<ContractMetadata>,
) -> Result<(), CliError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_contract_metadata(&path, method, package_version, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        out.extend(contract_metadata_from_source(
            &contents,
            &path,
            method,
            package_version,
        ));
    }
    Ok(())
}

fn contract_metadata_from_source(
    source: &str,
    path: &Path,
    method: &str,
    package_version: &str,
) -> Vec<ContractMetadata> {
    let mut out = Vec::new();
    let mut offset = 0;

    while let Some(attr_start_rel) = find_next_contract_attr(source, offset) {
        let attr_start = attr_start_rel;
        let Some(attr_end_rel) = source[attr_start..].find(']') else {
            break;
        };
        let attr_end = attr_start + attr_end_rel;
        let attr = &source[attr_start..=attr_end];
        let item_start = attr_end + 1;
        let Some((header_start, brace_idx)) = find_item_header(source, item_start) else {
            break;
        };
        let Some(block_end) = find_matching_brace(source, brace_idx) else {
            break;
        };

        let header = &source[header_start..brace_idx];
        let body = &source[brace_idx + 1..block_end];
        if !body.contains(&format!("fn {method}")) {
            offset = block_end + 1;
            continue;
        }

        let version = parse_contract_attr_value(attr, "version")
            .unwrap_or_else(|| package_version.to_string());
        if let Some(interface_name) =
            parse_contract_attr_value(attr, "interface").or_else(|| infer_interface_name(header))
            && let Some(method_metadata) = parse_method_metadata(body, method)
        {
            out.push(ContractMetadata {
                interface_name,
                version,
                source: path.to_path_buf(),
                method: method_metadata,
            });
        }

        offset = block_end + 1;
    }

    out
}

fn find_next_contract_attr(source: &str, start: usize) -> Option<usize> {
    let mut search = start;
    while let Some(rel) = source[search..].find("#[contract") {
        let idx = search + rel;
        match source.as_bytes().get(idx + "#[contract".len()) {
            Some(b'(' | b']') => return Some(idx),
            _ => search = idx + "#[contract".len(),
        }
    }
    None
}

fn find_item_header(source: &str, start: usize) -> Option<(usize, usize)> {
    let remainder = &source[start..];
    let impl_idx = remainder.find("impl ");
    let trait_idx = remainder.find("trait ");
    let header_rel = match (impl_idx, trait_idx) {
        (Some(i), Some(t)) => i.min(t),
        (Some(i), None) => i,
        (None, Some(t)) => t,
        (None, None) => return None,
    };
    let header_start = start + header_rel;
    let brace_rel = source[header_start..].find('{')?;
    Some((header_start, header_start + brace_rel))
}

fn find_matching_brace(source: &str, open_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, ch) in source[open_idx..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open_idx + idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_contract_attr_value(attr: &str, key: &str) -> Option<String> {
    let needle = format!("{key} = \"");
    let start = attr.find(&needle)? + needle.len();
    let rest = &attr[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_method_metadata(body: &str, method: &str) -> Option<MethodMetadata> {
    let target = format!("fn {method}");
    let mut search_offset = 0;
    let fn_start = loop {
        let rel = body[search_offset..].find(&target)?;
        let abs = search_offset + rel;
        let after = abs + target.len();
        match body.as_bytes().get(after) {
            Some(b) if b.is_ascii_alphanumeric() || *b == b'_' => {
                search_offset = after;
            }
            _ => break abs,
        }
    };
    let signature = &body[fn_start..];
    let open_paren = signature.find('(')?;
    let close_paren = find_matching_delimiter(signature, open_paren, '(', ')')?;
    let args_src = &signature[open_paren + 1..close_paren];

    let after_args = &signature[close_paren + 1..];
    let signature_end = after_args.find('{').or_else(|| after_args.find(';'))?;
    let header_tail = after_args[..signature_end].trim();
    let return_type = header_tail
        .strip_prefix("->")
        .map(str::trim)
        .filter(|ty| !ty.is_empty())
        .map(ToOwned::to_owned);

    let mut arg_types = Vec::new();
    for arg in split_top_level(args_src, ',') {
        let arg = arg.trim();
        if arg.is_empty()
            || arg == "&self"
            || arg == "&mut self"
            || arg == "self"
            || arg == "mut self"
        {
            continue;
        }
        let (_, ty) = arg.rsplit_once(':')?;
        arg_types.push(ty.trim().to_string());
    }

    Some(MethodMetadata {
        arg_types,
        return_type,
    })
}

fn find_matching_delimiter(
    source: &str,
    open_idx: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, ch) in source[open_idx..].char_indices() {
        match ch {
            c if c == open => depth += 1,
            c if c == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open_idx + idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(source: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut angle = 0usize;

    for (idx, ch) in source.char_indices() {
        match ch {
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            _ if ch == delimiter && paren == 0 && bracket == 0 && angle == 0 => {
                parts.push(source[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(source[start..].trim());
    parts
}

fn infer_interface_name(header: &str) -> Option<String> {
    let header = header.trim();
    if let Some(rest) = header.strip_prefix("trait ") {
        return parse_rust_ident(rest);
    }
    if let Some(rest) = header.strip_prefix("pub trait ") {
        return parse_rust_ident(rest);
    }
    if let Some(rest) = header.strip_prefix("impl ") {
        if let Some((lhs, _rhs)) = rest.split_once(" for ") {
            return last_ident(lhs);
        }
        return parse_rust_ident(rest);
    }
    None
}

fn parse_rust_ident(input: &str) -> Option<String> {
    let ident: String = input
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

fn last_ident(input: &str) -> Option<String> {
    input
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .rfind(|part| !part.is_empty())
        .map(str::to_string)
}

fn contract_entrypoint_id(
    interface_name: &str,
    version_str: &str,
    method: &str,
) -> ContractEntrypointID {
    let mut hasher = Sha256::new();
    hasher.update(interface_name.as_bytes());
    hasher.update(major_version(version_str).as_bytes());
    hasher.update(method.as_bytes());
    let hash = hasher.finalize();
    let mut id = [0u8; 8];
    id.copy_from_slice(&hash[..8]);
    ContractEntrypointID::from(id)
}

fn major_version(version: &str) -> &str {
    let normalized = version.split_once('+').map_or(version, |(base, _)| base);
    let normalized = normalized
        .split_once('-')
        .map_or(normalized, |(base, _)| base);

    let mut parts = normalized.split('.');
    let major = parts.next().unwrap_or(normalized);
    let minor = parts.next().unwrap_or("0");
    let patch = parts.next().unwrap_or("0");

    if major != "0" {
        major
    } else if minor != "0" {
        &normalized[..major.len() + 1 + minor.len()]
    } else if patch != "0" {
        &normalized[..major.len() + 1 + minor.len() + 1 + patch.len()]
    } else {
        normalized
    }
}

type TypeDefIndex<'a> = BTreeMap<String, &'a ContractTypeDef>;

fn encode_calldata(
    args: &[String],
    metadata: Option<&ResolvedMethodMetadata>,
) -> Result<Vec<u8>, CliError> {
    if let Some(metadata) = metadata {
        return match metadata {
            ResolvedMethodMetadata::Spec(metadata) => encode_spec_typed_calldata(args, metadata),
            ResolvedMethodMetadata::Local(metadata) => {
                encode_local_typed_calldata(args, &metadata.method)
            }
        };
    }
    if args.is_empty() {
        return Ok(vec![]);
    }
    if args.len() == 1 {
        let stripped = args[0].strip_prefix("0x").unwrap_or(&args[0]);
        return hex::decode(stripped)
            .map_err(|e| CliError::Other(format!("invalid calldata hex: {e}")));
    }
    Err(CliError::Other(
        "multiple call arguments require local contract metadata — run `groundhog call` from \
         inside the project that defines the contract, or pass a single hex-encoded calldata \
         argument"
            .into(),
    ))
}

fn encode_spec_typed_calldata(
    args: &[String],
    metadata: &SpecMethodMetadata,
) -> Result<Vec<u8>, CliError> {
    if args.len() != metadata.method.args.len() {
        return Err(CliError::Other(format!(
            "method expects {} argument(s) but {} were provided",
            metadata.method.args.len(),
            args.len(),
        )));
    }
    if metadata.method.args.is_empty() {
        return Ok(vec![]);
    }

    let type_defs = build_type_def_index(&metadata.type_defs)?;
    let mut encoded = Vec::new();

    for (index, (arg, field)) in args.iter().zip(metadata.method.args.iter()).enumerate() {
        let field_name = spec_string_text(&field.name)?;
        let path = format!("argument {} (`{field_name}`)", index + 1);
        encoded.extend(encode_cli_spec_value(&field.ty, arg, &type_defs, &path)?);
    }

    Ok(encoded)
}

fn encode_local_typed_calldata(
    args: &[String],
    method: &MethodMetadata,
) -> Result<Vec<u8>, CliError> {
    if args.len() != method.arg_types.len() {
        return Err(CliError::Other(format!(
            "method expects {} argument(s) but {} were provided",
            method.arg_types.len(),
            args.len(),
        )));
    }
    if method.arg_types.is_empty() {
        return Ok(vec![]);
    }

    let mut encoded = Vec::new();
    for (arg, ty) in args.iter().zip(&method.arg_types) {
        encoded.extend(encode_single_value(ty, arg)?);
    }
    Ok(encoded)
}

fn build_type_def_index(type_defs: &[ContractTypeDef]) -> Result<TypeDefIndex<'_>, CliError> {
    let mut index = BTreeMap::new();

    for type_def in type_defs {
        let name = spec_type_def_name(type_def)?.to_owned();
        if index.insert(name.clone(), type_def).is_some() {
            return Err(CliError::SpecFailed(format!(
                "embedded contract spec defines `{name}` multiple times"
            )));
        }
    }

    Ok(index)
}

fn spec_string_text(value: &ContractSpecString) -> Result<&str, CliError> {
    std::str::from_utf8(value.as_ref())
        .map_err(|e| CliError::SpecFailed(format!("contract spec contains invalid UTF-8: {e}")))
}

fn spec_type_def_name(type_def: &ContractTypeDef) -> Result<&str, CliError> {
    match type_def {
        ContractTypeDef::TypeDefStruct { struct_def } => spec_string_text(&struct_def.name),
        ContractTypeDef::TypeDefEnum { enum_def } => spec_string_text(&enum_def.name),
        ContractTypeDef::TypeDefUnion { union_def } => spec_string_text(&union_def.name),
        _ => Err(CliError::SpecFailed(
            "embedded contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

fn resolve_named_type_def<'a>(
    named: &ContractSpecString,
    type_defs: &'a TypeDefIndex<'a>,
) -> Result<&'a ContractTypeDef, CliError> {
    let name = spec_string_text(named)?;
    type_defs
        .get(name)
        .copied()
        .ok_or_else(|| CliError::SpecFailed(format!("embedded contract spec is missing `{name}`")))
}

fn encode_cli_spec_value(
    ty: &ContractType,
    argument: &str,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    match ty {
        ContractType::Option { option_inner } => {
            let inner = option_inner.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing an option inner type".into(),
                )
            })?;
            let mut encoded = encode_xdr_value(argument != "null")
                .map_err(|e| encode_value_error(path, e.to_string()))?;
            if argument != "null" {
                encoded.extend(encode_cli_spec_value(inner, argument, type_defs, path)?);
            }
            Ok(encoded)
        }
        ContractType::Vector { vector } if vector_contains_u8(vector.element.as_deref()) => {
            let bytes = parse_bytes_argument(argument, path)?;
            if bytes.len() > vector.max_len as usize {
                return Err(encode_value_error(
                    path,
                    format!(
                        "byte vector exceeds max length {} (got {})",
                        vector.max_len,
                        bytes.len()
                    ),
                ));
            }
            encode_variable_bytes(bytes, path)
        }
        ContractType::Array { array } if vector_contains_u8(array.element.as_deref()) => {
            let bytes = parse_bytes_argument(argument, path)?;
            if bytes.len() != array.len as usize {
                return Err(encode_value_error(
                    path,
                    format!(
                        "byte array expects length {} but got {}",
                        array.len,
                        bytes.len()
                    ),
                ));
            }
            encode_fixed_bytes(bytes, path)
        }
        ContractType::Tuple { .. } | ContractType::Vector { .. } | ContractType::Array { .. } => {
            let json = parse_json_argument(argument, path)?;
            encode_json_spec_value(ty, &json, type_defs, path)
        }
        ContractType::Named { named } => match resolve_named_type_def(named, type_defs)? {
            ContractTypeDef::TypeDefStruct { .. } => {
                let json = parse_json_argument(argument, path)?;
                encode_json_spec_value(ty, &json, type_defs, path)
            }
            ContractTypeDef::TypeDefEnum { enum_def } => {
                encode_enum_cli_value(enum_def, argument, path)
            }
            ContractTypeDef::TypeDefUnion { union_def } => {
                encode_union_cli_value(union_def, argument, type_defs, path)
            }
            _ => Err(CliError::SpecFailed(
                "embedded contract spec contains an unsupported type definition kind".into(),
            )),
        },
        _ => encode_scalar_cli_value(ty, argument, path),
    }
}

fn encode_json_spec_value(
    ty: &ContractType,
    value: &serde_json::Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    match ty {
        ContractType::Void => match value {
            serde_json::Value::Null => Ok(vec![]),
            _ => Err(encode_value_error(path, "expected null for a void value")),
        },
        ContractType::Bool => encode_xdr_value(parse_json_bool(value, path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U8 => {
            encode_xdr_value(u32::from(parse_json_numeric::<u8>(value, "u8", path)?))
                .map_err(|e| encode_value_error(path, e.to_string()))
        }
        ContractType::U32 => encode_xdr_value(parse_json_numeric::<u32>(value, "u32", path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I32 => encode_xdr_value(parse_json_numeric::<i32>(value, "i32", path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U64 => encode_xdr_value(parse_json_numeric::<u64>(value, "u64", path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I64 => encode_xdr_value(parse_json_numeric::<i64>(value, "i64", path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U128 => encode_xdr_value(parse_json_numeric::<u128>(value, "u128", path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I128 => encode_xdr_value(parse_json_numeric::<i128>(value, "i128", path)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::String => encode_xdr_value(parse_json_string(value, path)?.to_owned())
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::Bytes => encode_variable_bytes(parse_bytes_json_value(value, path)?, path),
        ContractType::Address => {
            encode_xdr_value(parse_contract_address(parse_json_string(value, path)?)?)
                .map_err(|e| encode_value_error(path, e.to_string()))
        }
        ContractType::Option { option_inner } => {
            let inner = option_inner.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing an option inner type".into(),
                )
            })?;
            let mut encoded = encode_xdr_value(!value.is_null())
                .map_err(|e| encode_value_error(path, e.to_string()))?;
            if !value.is_null() {
                encoded.extend(encode_json_spec_value(inner, value, type_defs, path)?);
            }
            Ok(encoded)
        }
        ContractType::Vector { vector } => {
            let inner = vector.element.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing a vector element type".into(),
                )
            })?;
            if vector_contains_u8(Some(inner)) {
                let bytes = parse_bytes_json_value(value, path)?;
                if bytes.len() > vector.max_len as usize {
                    return Err(encode_value_error(
                        path,
                        format!(
                            "byte vector exceeds max length {} (got {})",
                            vector.max_len,
                            bytes.len()
                        ),
                    ));
                }
                return encode_variable_bytes(bytes, path);
            }

            let items = parse_json_array(value, path)?;
            if items.len() > vector.max_len as usize {
                return Err(encode_value_error(
                    path,
                    format!(
                        "vector exceeds max length {} (got {})",
                        vector.max_len,
                        items.len()
                    ),
                ));
            }

            let mut encoded = encode_xdr_value(u32::try_from(items.len()).map_err(|_| {
                encode_value_error(path, "vector length exceeds the supported u32 range")
            })?)
            .map_err(|e| encode_value_error(path, e.to_string()))?;
            for (index, item) in items.iter().enumerate() {
                encoded.extend(encode_json_spec_value(
                    inner,
                    item,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(encoded)
        }
        ContractType::Tuple { tuple } => encode_tuple_json_items(tuple, value, type_defs, path),
        ContractType::Named { named } => match resolve_named_type_def(named, type_defs)? {
            ContractTypeDef::TypeDefStruct { struct_def } => {
                encode_struct_json_value(struct_def, value, type_defs, path)
            }
            ContractTypeDef::TypeDefEnum { enum_def } => {
                encode_enum_json_value(enum_def, value, path)
            }
            ContractTypeDef::TypeDefUnion { union_def } => {
                encode_union_json_value(union_def, value, type_defs, path)
            }
            _ => Err(CliError::SpecFailed(
                "embedded contract spec contains an unsupported type definition kind".into(),
            )),
        },
        ContractType::Array { array } => {
            let inner = array.element.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing an array element type".into(),
                )
            })?;
            if vector_contains_u8(Some(inner)) {
                let bytes = parse_bytes_json_value(value, path)?;
                if bytes.len() != array.len as usize {
                    return Err(encode_value_error(
                        path,
                        format!(
                            "byte array expects length {} but got {}",
                            array.len,
                            bytes.len()
                        ),
                    ));
                }
                return encode_fixed_bytes(bytes, path);
            }

            let items = parse_json_array(value, path)?;
            if items.len() != array.len as usize {
                return Err(encode_value_error(
                    path,
                    format!("array expects length {} but got {}", array.len, items.len()),
                ));
            }

            let mut encoded = Vec::new();
            for (index, item) in items.iter().enumerate() {
                encoded.extend(encode_json_spec_value(
                    inner,
                    item,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(encoded)
        }
        _ => Err(CliError::SpecFailed(
            "embedded contract spec contains an unsupported contract type".into(),
        )),
    }
}

fn encode_struct_json_value(
    struct_def: &ContractStructDef,
    value: &serde_json::Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    encode_field_list(struct_def.fields.iter(), value, type_defs, path)
}

fn encode_field_list<'a>(
    fields: impl ExactSizeIterator<Item = &'a ContractField>,
    value: &serde_json::Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    let fields = fields.collect::<Vec<_>>();
    match value {
        serde_json::Value::Array(values) => {
            if values.len() != fields.len() {
                return Err(encode_value_error(
                    path,
                    format!(
                        "expected {} field value(s) but got {}",
                        fields.len(),
                        values.len()
                    ),
                ));
            }

            let mut encoded = Vec::new();
            for (index, (field, value)) in fields.iter().zip(values).enumerate() {
                encoded.extend(encode_json_spec_value(
                    &field.ty,
                    value,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(encoded)
        }
        serde_json::Value::Object(object) => {
            let expected_names = fields
                .iter()
                .map(|field| spec_string_text(&field.name))
                .collect::<Result<Vec<_>, _>>()?;
            for key in object.keys() {
                if !expected_names.iter().any(|expected| expected == key) {
                    return Err(encode_value_error(
                        path,
                        format!("unexpected field `{key}`"),
                    ));
                }
            }

            let mut encoded = Vec::new();
            for field in fields {
                let field_name = spec_string_text(&field.name)?;
                let field_value = object.get(field_name).ok_or_else(|| {
                    encode_value_error(path, format!("missing field `{field_name}`"))
                })?;
                encoded.extend(encode_json_spec_value(
                    &field.ty,
                    field_value,
                    type_defs,
                    &field_path(path, field_name),
                )?);
            }
            Ok(encoded)
        }
        _ => Err(encode_value_error(path, "expected a JSON object or array")),
    }
}

fn encode_tuple_json_items(
    tuple: &[ContractType],
    value: &serde_json::Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    let items = parse_json_array(value, path)?;
    if items.len() != tuple.len() {
        return Err(encode_value_error(
            path,
            format!(
                "expected {} tuple item(s) but got {}",
                tuple.len(),
                items.len()
            ),
        ));
    }

    let mut encoded = Vec::new();
    for (index, (ty, item)) in tuple.iter().zip(items).enumerate() {
        encoded.extend(encode_json_spec_value(
            ty,
            item,
            type_defs,
            &index_path(path, index),
        )?);
    }
    Ok(encoded)
}

fn encode_enum_cli_value(
    enum_def: &ContractEnumDef,
    argument: &str,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(argument)
        && matches!(
            json,
            serde_json::Value::String(_) | serde_json::Value::Number(_)
        )
    {
        return encode_enum_json_value(enum_def, &json, path);
    }
    encode_enum_json_value(
        enum_def,
        &serde_json::Value::String(argument.to_owned()),
        path,
    )
}

fn encode_enum_json_value(
    enum_def: &ContractEnumDef,
    value: &serde_json::Value,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    let discriminant = match value {
        serde_json::Value::String(name) => match name.parse::<i32>() {
            Ok(discriminant) => discriminant,
            Err(_) => enum_def
                .variants
                .iter()
                .find_map(|variant| {
                    let variant_name = spec_string_text(&variant.name).ok()?;
                    (variant_name == name).then_some(variant.value)
                })
                .ok_or_else(|| {
                    encode_value_error(path, format!("unknown enum variant `{name}`"))
                })?,
        },
        serde_json::Value::Number(number) => number
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| encode_value_error(path, "enum discriminants must fit in i32"))?,
        _ => {
            return Err(encode_value_error(
                path,
                "expected an enum variant name or discriminant",
            ));
        }
    };

    encode_xdr_value(discriminant).map_err(|e| encode_value_error(path, e.to_string()))
}

fn encode_union_cli_value(
    union_def: &ContractUnionDef,
    argument: &str,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(argument) {
        return encode_union_json_value(union_def, &json, type_defs, path);
    }

    let case = resolve_union_case(
        union_def,
        &serde_json::Value::String(argument.to_owned()),
        path,
    )?;
    encode_union_case(union_def, case, None, type_defs, path)
}

fn encode_union_json_value(
    union_def: &ContractUnionDef,
    value: &serde_json::Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    match value {
        serde_json::Value::Object(object) => {
            let case_value = object
                .get("case")
                .or_else(|| object.get("discriminant"))
                .ok_or_else(|| {
                    encode_value_error(
                        path,
                        "union values must include a `case` (or `discriminant`) field",
                    )
                })?;
            let case = resolve_union_case(union_def, case_value, &field_path(path, "case"))?;
            encode_union_case(union_def, case, object.get("value"), type_defs, path)
        }
        serde_json::Value::String(_) | serde_json::Value::Number(_) => {
            let case = resolve_union_case(union_def, value, path)?;
            encode_union_case(union_def, case, None, type_defs, path)
        }
        _ => Err(encode_value_error(
            path,
            "expected a union case name/discriminant or an object with `case` and optional `value`",
        )),
    }
}

fn resolve_union_case<'a>(
    union_def: &'a ContractUnionDef,
    value: &serde_json::Value,
    path: &str,
) -> Result<&'a ContractUnionCase, CliError> {
    match value {
        serde_json::Value::String(name) => {
            if let Ok(discriminant) = name.parse::<i32>() {
                union_def
                    .cases
                    .iter()
                    .find(|case| case.discriminant == discriminant)
                    .ok_or_else(|| {
                        encode_value_error(
                            path,
                            format!("unknown union discriminant `{discriminant}`"),
                        )
                    })
            } else {
                union_def
                    .cases
                    .iter()
                    .find(|case| spec_string_text(&case.name).ok() == Some(name.as_str()))
                    .ok_or_else(|| encode_value_error(path, format!("unknown union case `{name}`")))
            }
        }
        serde_json::Value::Number(number) => {
            let discriminant = number
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| encode_value_error(path, "union discriminants must fit in i32"))?;
            union_def
                .cases
                .iter()
                .find(|case| case.discriminant == discriminant)
                .ok_or_else(|| {
                    encode_value_error(path, format!("unknown union discriminant `{discriminant}`"))
                })
        }
        _ => Err(encode_value_error(
            path,
            "expected a union case name or discriminant",
        )),
    }
}

fn encode_union_case(
    union_def: &ContractUnionDef,
    case: &ContractUnionCase,
    payload: Option<&serde_json::Value>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    let mut encoded =
        encode_xdr_value(case.discriminant).map_err(|e| encode_value_error(path, e.to_string()))?;
    match &case.case_type {
        ContractUnionCaseType::UnionCaseVoid => {
            if let Some(value) = payload
                && !value.is_null()
            {
                return Err(encode_value_error(
                    path,
                    format!(
                        "union case `{}` does not take a payload",
                        spec_string_text(&case.name)?
                    ),
                ));
            }
        }
        ContractUnionCaseType::UnionCaseTuple { tuple } => {
            let payload = payload.ok_or_else(|| {
                encode_value_error(
                    path,
                    format!(
                        "union case `{}` requires a `value` payload",
                        spec_string_text(&case.name).unwrap_or("unknown")
                    ),
                )
            })?;
            encoded.extend(encode_tuple_json_items(
                tuple,
                payload,
                type_defs,
                &field_path(path, "value"),
            )?);
        }
        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
            let payload = payload.ok_or_else(|| {
                encode_value_error(
                    path,
                    format!(
                        "union case `{}` requires a `value` payload",
                        spec_string_text(&case.name).unwrap_or("unknown")
                    ),
                )
            })?;
            encoded.extend(encode_field_list(
                struct_fields.iter(),
                payload,
                type_defs,
                &field_path(path, "value"),
            )?);
        }
        _ => {
            return Err(CliError::SpecFailed(format!(
                "embedded contract spec contains an unsupported union case in `{}`",
                spec_string_text(&union_def.name)?
            )));
        }
    }

    Ok(encoded)
}

fn encode_single_value(ty: &str, value: &str) -> Result<Vec<u8>, CliError> {
    let ty = normalize_type_name(ty);
    match ty.as_str() {
        "u16" => encode_xdr_value::<u16>(parse_numeric(value, &ty)?),
        "u32" => encode_xdr_value::<u32>(parse_numeric(value, &ty)?),
        "u64" => encode_xdr_value::<u64>(parse_numeric(value, &ty)?),
        "u128" => encode_xdr_value::<u128>(parse_numeric(value, &ty)?),
        "i16" => encode_xdr_value::<i16>(parse_numeric(value, &ty)?),
        "i32" => encode_xdr_value::<i32>(parse_numeric(value, &ty)?),
        "i64" => encode_xdr_value::<i64>(parse_numeric(value, &ty)?),
        "i128" => encode_xdr_value::<i128>(parse_numeric(value, &ty)?),
        "bool" => {
            let parsed = value
                .parse::<bool>()
                .map_err(|e| CliError::Other(format!("invalid bool `{value}`: {e}")))?;
            encode_xdr_value(parsed)
        }
        "String" | "str" => encode_xdr_value(value.to_string()),
        "ContractAddress" => encode_xdr_value(parse_contract_address(value)?),
        "Ed25519PublicKey" => encode_xdr_value(Ed25519PublicKey::from(parse_fixed_hex::<32>(
            value,
            "ed25519 public key",
        )?)),
        "SHA256Hash" => encode_xdr_value(SHA256Hash::from(parse_fixed_hex::<32>(
            value,
            "sha256 hash",
        )?)),
        "ContractEntrypointID" => encode_xdr_value(ContractEntrypointID::from(
            parse_fixed_hex::<8>(value, "contract entrypoint")?,
        )),
        _ => Err(CliError::Other(format!(
            "typed call encoding for `{ty}` is not implemented yet; pass an explicit 0x-prefixed \
             entrypoint ID and raw calldata instead"
        ))),
    }
}

fn parse_numeric<T>(value: &str, ty: &str) -> Result<T, CliError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|e| CliError::Other(format!("invalid {ty} `{value}`: {e}")))
}

fn encode_xdr_value<T>(value: T) -> Result<Vec<u8>, CliError>
where
    T: xdrs::WriteXDR,
{
    xdrs::cxs::encode(&value)
        .map_err(|e| CliError::Other(format!("failed to encode calldata: {e}")))
}

fn encode_variable_bytes(bytes: Vec<u8>, path: &str) -> Result<Vec<u8>, CliError> {
    encode_xdr_value(bytes).map_err(|e| encode_value_error(path, e.to_string()))
}

fn encode_fixed_bytes(bytes: Vec<u8>, path: &str) -> Result<Vec<u8>, CliError> {
    let mut writer = xdrs::cxs::buf_writer::BufWriter::with_capacity(bytes.len());
    writer
        .write_opaque_fixed(&bytes)
        .map_err(|e| encode_value_error(path, format!("failed to encode fixed bytes: {e}")))?;
    Ok(writer.into())
}

fn parse_json_argument(argument: &str, path: &str) -> Result<serde_json::Value, CliError> {
    serde_json::from_str(argument)
        .map_err(|e| encode_value_error(path, format!("must be valid JSON: {e}")))
}

fn parse_json_array<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Result<&'a Vec<serde_json::Value>, CliError> {
    value
        .as_array()
        .ok_or_else(|| encode_value_error(path, "expected a JSON array"))
}

fn parse_json_string<'a>(value: &'a serde_json::Value, path: &str) -> Result<&'a str, CliError> {
    value
        .as_str()
        .ok_or_else(|| encode_value_error(path, "expected a JSON string"))
}

fn parse_json_numeric<T>(value: &serde_json::Value, ty: &str, path: &str) -> Result<T, CliError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match value {
        serde_json::Value::Number(number) => parse_numeric(&number.to_string(), ty)
            .map_err(|e| encode_value_error(path, e.to_string())),
        serde_json::Value::String(string) => {
            parse_numeric(string, ty).map_err(|e| encode_value_error(path, e.to_string()))
        }
        _ => Err(encode_value_error(path, format!("expected a {ty}"))),
    }
}

fn parse_json_bool(value: &serde_json::Value, path: &str) -> Result<bool, CliError> {
    match value {
        serde_json::Value::Bool(value) => Ok(*value),
        serde_json::Value::String(value) => value
            .parse::<bool>()
            .map_err(|e| encode_value_error(path, format!("invalid bool `{value}`: {e}"))),
        _ => Err(encode_value_error(path, "expected a bool")),
    }
}

fn encode_scalar_cli_value(
    ty: &ContractType,
    value: &str,
    path: &str,
) -> Result<Vec<u8>, CliError> {
    match ty {
        ContractType::Bool => encode_xdr_value(
            value
                .parse::<bool>()
                .map_err(|e| encode_value_error(path, format!("invalid bool `{value}`: {e}")))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U8 => encode_xdr_value(u32::from(
            parse_numeric::<u8>(value, "u8")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        ))
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U32 => encode_xdr_value(
            parse_numeric::<u32>(value, "u32")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I32 => encode_xdr_value(
            parse_numeric::<i32>(value, "i32")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U64 => encode_xdr_value(
            parse_numeric::<u64>(value, "u64")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I64 => encode_xdr_value(
            parse_numeric::<i64>(value, "i64")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U128 => encode_xdr_value(
            parse_numeric::<u128>(value, "u128")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I128 => encode_xdr_value(
            parse_numeric::<i128>(value, "i128")
                .map_err(|e| encode_value_error(path, e.to_string()))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::String => {
            encode_xdr_value(value.to_owned()).map_err(|e| encode_value_error(path, e.to_string()))
        }
        ContractType::Bytes => encode_variable_bytes(parse_bytes_argument(value, path)?, path),
        ContractType::Address => encode_xdr_value(parse_contract_address(value)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        _ => Err(encode_value_error(
            path,
            "this contract argument type requires structured JSON input",
        )),
    }
}

fn parse_bytes_argument(argument: &str, path: &str) -> Result<Vec<u8>, CliError> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(argument)
        && matches!(
            json,
            serde_json::Value::String(_) | serde_json::Value::Array(_)
        )
    {
        return parse_bytes_json_value(&json, path);
    }
    parse_byte_string(argument, path)
}

fn parse_bytes_json_value(value: &serde_json::Value, path: &str) -> Result<Vec<u8>, CliError> {
    match value {
        serde_json::Value::String(value) => parse_byte_string(value, path),
        serde_json::Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| parse_json_numeric::<u8>(value, "u8", &index_path(path, index)))
            .collect(),
        _ => Err(encode_value_error(
            path,
            "expected a hex string, UTF-8 string, or JSON array of byte values",
        )),
    }
}

fn parse_byte_string(value: &str, path: &str) -> Result<Vec<u8>, CliError> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return hex::decode(hex)
            .map_err(|e| encode_value_error(path, format!("invalid byte hex `{value}`: {e}")));
    }
    Ok(value.as_bytes().to_vec())
}

fn vector_contains_u8(element: Option<&ContractType>) -> bool {
    matches!(element, Some(ContractType::U8))
}

fn encode_value_error(path: &str, message: impl Into<String>) -> CliError {
    CliError::Other(format!("{path}: {}", message.into()))
}

fn field_path(path: &str, field: &str) -> String {
    format!("{path}.{field}")
}

fn index_path(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn decode_return_value(
    bytes: &[u8],
    metadata: Option<&ResolvedMethodMetadata>,
) -> Option<serde_json::Value> {
    match metadata? {
        ResolvedMethodMetadata::Spec(metadata) => {
            if matches!(metadata.method.return_type, ContractType::Void) {
                return None;
            }
            let type_defs = build_type_def_index(&metadata.type_defs).ok()?;
            decode_spec_value(&metadata.method.return_type, bytes, &type_defs).ok()
        }
        ResolvedMethodMetadata::Local(metadata) => {
            let ty = metadata.method.return_type.as_deref()?;
            decode_single_value(ty, bytes)
        }
    }
}

fn decode_spec_value(
    ty: &ContractType,
    bytes: &[u8],
    type_defs: &TypeDefIndex<'_>,
) -> Result<serde_json::Value, CliError> {
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(bytes);
    let value = decode_spec_value_from_reader(ty, &mut reader, type_defs, "return value")?;
    if reader.remaining() != 0 {
        return Err(CliError::Other(format!(
            "return value: {} unread byte(s) remain after decoding",
            reader.remaining()
        )));
    }
    Ok(value)
}

fn decode_spec_value_from_reader(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<serde_json::Value, CliError> {
    match ty {
        ContractType::Void => Ok(serde_json::Value::Null),
        ContractType::Bool => read_xdr_value::<bool>(reader, path).map(serde_json::Value::from),
        ContractType::U8 => {
            let value = read_xdr_value::<u32>(reader, path)?;
            let value: u8 = value.try_into().map_err(|_| {
                CliError::Other(format!("{path}: decoded u8 value is out of range"))
            })?;
            Ok(serde_json::Value::from(value))
        }
        ContractType::U32 => read_xdr_value::<u32>(reader, path).map(serde_json::Value::from),
        ContractType::I32 => read_xdr_value::<i32>(reader, path).map(serde_json::Value::from),
        ContractType::U64 => read_xdr_value::<u64>(reader, path).map(serde_json::Value::from),
        ContractType::I64 => read_xdr_value::<i64>(reader, path).map(serde_json::Value::from),
        ContractType::U128 => read_xdr_value::<u128>(reader, path)
            .map(|value| serde_json::Value::from(value.to_string())),
        ContractType::I128 => read_xdr_value::<i128>(reader, path)
            .map(|value| serde_json::Value::from(value.to_string())),
        ContractType::String => read_xdr_value::<String>(reader, path).map(serde_json::Value::from),
        ContractType::Bytes => reader
            .read_opaque_variable_owned(u32::MAX)
            .map(hex_json_value)
            .map_err(|e| CliError::Other(format!("{path}: failed to decode bytes: {e}"))),
        ContractType::Address => {
            read_xdr_value::<ContractAddress>(reader, path).map(hex_json_value)
        }
        ContractType::Option { option_inner } => {
            let inner = option_inner.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing an option inner type".into(),
                )
            })?;
            if read_xdr_value::<bool>(reader, path)? {
                decode_spec_value_from_reader(inner, reader, type_defs, path)
            } else {
                Ok(serde_json::Value::Null)
            }
        }
        ContractType::Vector { vector } => {
            let inner = vector.element.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing a vector element type".into(),
                )
            })?;
            if vector_contains_u8(Some(inner)) {
                return reader
                    .read_opaque_variable_owned(vector.max_len)
                    .map(hex_json_value)
                    .map_err(|e| {
                        CliError::Other(format!("{path}: failed to decode byte vector: {e}"))
                    });
            }

            let len = read_xdr_value::<u32>(reader, path)? as usize;
            if len > vector.max_len as usize {
                return Err(CliError::Other(format!(
                    "{path}: decoded vector length {len} exceeds max {}",
                    vector.max_len
                )));
            }
            let mut values = Vec::with_capacity(len);
            for index in 0..len {
                values.push(decode_spec_value_from_reader(
                    inner,
                    reader,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(serde_json::Value::Array(values))
        }
        ContractType::Tuple { tuple } => {
            let mut values = Vec::with_capacity(tuple.len());
            for (index, item) in tuple.iter().enumerate() {
                values.push(decode_spec_value_from_reader(
                    item,
                    reader,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(serde_json::Value::Array(values))
        }
        ContractType::Named { named } => match resolve_named_type_def(named, type_defs)? {
            ContractTypeDef::TypeDefStruct { struct_def } => {
                decode_struct_value(struct_def, reader, type_defs, path)
            }
            ContractTypeDef::TypeDefEnum { enum_def } => decode_enum_value(enum_def, reader, path),
            ContractTypeDef::TypeDefUnion { union_def } => {
                decode_union_value(union_def, reader, type_defs, path)
            }
            _ => Err(CliError::SpecFailed(
                "embedded contract spec contains an unsupported type definition kind".into(),
            )),
        },
        ContractType::Array { array } => {
            let inner = array.element.as_deref().ok_or_else(|| {
                CliError::SpecFailed(
                    "embedded contract spec is missing an array element type".into(),
                )
            })?;
            if vector_contains_u8(Some(inner)) {
                return reader
                    .read_opaque_fixed_owned(array.len)
                    .map(hex_json_value)
                    .map_err(|e| {
                        CliError::Other(format!("{path}: failed to decode byte array: {e}"))
                    });
            }

            let mut values = Vec::with_capacity(array.len as usize);
            for index in 0..array.len as usize {
                values.push(decode_spec_value_from_reader(
                    inner,
                    reader,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(serde_json::Value::Array(values))
        }
        _ => Err(CliError::SpecFailed(
            "embedded contract spec contains an unsupported contract type".into(),
        )),
    }
}

fn decode_struct_value(
    struct_def: &ContractStructDef,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<serde_json::Value, CliError> {
    decode_field_values(struct_def.fields.iter(), reader, type_defs, path)
}

fn decode_field_values<'a>(
    fields: impl ExactSizeIterator<Item = &'a ContractField>,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<serde_json::Value, CliError> {
    let fields = fields.collect::<Vec<_>>();
    if fields_decode_to_object(&fields)? {
        let mut object = serde_json::Map::with_capacity(fields.len());
        for field in fields {
            let field_name = spec_string_text(&field.name)?;
            let value = decode_spec_value_from_reader(
                &field.ty,
                reader,
                type_defs,
                &field_path(path, field_name),
            )?;
            object.insert(field_name.to_owned(), value);
        }
        Ok(serde_json::Value::Object(object))
    } else {
        let mut values = Vec::with_capacity(fields.len());
        for (index, field) in fields.iter().enumerate() {
            values.push(decode_spec_value_from_reader(
                &field.ty,
                reader,
                type_defs,
                &index_path(path, index),
            )?);
        }
        Ok(serde_json::Value::Array(values))
    }
}

fn fields_decode_to_object(fields: &[&ContractField]) -> Result<bool, CliError> {
    for field in fields {
        let name = spec_string_text(&field.name)?;
        if name.is_empty() || name.parse::<usize>().is_ok() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn decode_enum_value(
    enum_def: &ContractEnumDef,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    path: &str,
) -> Result<serde_json::Value, CliError> {
    let discriminant = read_xdr_value::<i32>(reader, path)?;
    for variant in enum_def.variants.iter() {
        if variant.value == discriminant {
            return Ok(serde_json::Value::from(
                spec_string_text(&variant.name)?.to_owned(),
            ));
        }
    }
    Ok(serde_json::Value::from(discriminant))
}

fn decode_union_value(
    union_def: &ContractUnionDef,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<serde_json::Value, CliError> {
    let discriminant = read_xdr_value::<i32>(reader, path)?;
    let case = union_def
        .cases
        .iter()
        .find(|case| case.discriminant == discriminant)
        .ok_or_else(|| {
            CliError::Other(format!(
                "{path}: unknown union discriminant `{discriminant}`"
            ))
        })?;

    let mut object = serde_json::Map::new();
    object.insert(
        "case".into(),
        serde_json::Value::from(spec_string_text(&case.name)?.to_owned()),
    );

    match &case.case_type {
        ContractUnionCaseType::UnionCaseVoid => {}
        ContractUnionCaseType::UnionCaseTuple { tuple } => {
            object.insert(
                "value".into(),
                decode_spec_value_from_reader(
                    &ContractType::Tuple {
                        tuple: tuple.clone(),
                    },
                    reader,
                    type_defs,
                    &field_path(path, "value"),
                )?,
            );
        }
        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
            object.insert(
                "value".into(),
                decode_field_values(
                    struct_fields.iter(),
                    reader,
                    type_defs,
                    &field_path(path, "value"),
                )?,
            );
        }
        _ => {
            return Err(CliError::SpecFailed(format!(
                "embedded contract spec contains an unsupported union in `{}`",
                spec_string_text(&union_def.name)?
            )));
        }
    }

    Ok(serde_json::Value::Object(object))
}

fn decode_single_value(ty: &str, bytes: &[u8]) -> Option<serde_json::Value> {
    let ty = normalize_type_name(ty);
    match ty.as_str() {
        "u16" => decode_xdr_value::<u16>(bytes).map(serde_json::Value::from),
        "u32" => decode_xdr_value::<u32>(bytes).map(serde_json::Value::from),
        "u64" => decode_xdr_value::<u64>(bytes).map(serde_json::Value::from),
        "u128" => {
            decode_xdr_value::<u128>(bytes).map(|value| serde_json::Value::from(value.to_string()))
        }
        "i16" => decode_xdr_value::<i16>(bytes).map(serde_json::Value::from),
        "i32" => decode_xdr_value::<i32>(bytes).map(serde_json::Value::from),
        "i64" => decode_xdr_value::<i64>(bytes).map(serde_json::Value::from),
        "i128" => {
            decode_xdr_value::<i128>(bytes).map(|value| serde_json::Value::from(value.to_string()))
        }
        "bool" => decode_xdr_value::<bool>(bytes).map(serde_json::Value::from),
        "String" | "str" => decode_xdr_value::<String>(bytes).map(serde_json::Value::from),
        "ContractAddress" => decode_xdr_value::<ContractAddress>(bytes).map(hex_json_value),
        "Ed25519PublicKey" => decode_xdr_value::<Ed25519PublicKey>(bytes).map(hex_json_value),
        "SHA256Hash" => decode_xdr_value::<SHA256Hash>(bytes).map(hex_json_value),
        "ContractEntrypointID" => {
            decode_xdr_value::<ContractEntrypointID>(bytes).map(hex_json_value)
        }
        _ => None,
    }
}

fn decode_xdr_value<T>(bytes: &[u8]) -> Option<T>
where
    T: xdrs::ReadXDROwned,
{
    xdrs::cxs::decode(bytes).ok()
}

fn read_xdr_value<T>(
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    path: &str,
) -> Result<T, CliError>
where
    T: xdrs::ReadXDROwned,
{
    xdrs::read_xdr(reader)
        .map_err(|e| CliError::Other(format!("{path}: failed to decode value: {e}")))
}

fn normalize_type_name(ty: &str) -> String {
    let mut ty = ty.trim();

    while let Some(stripped) = ty.strip_prefix('&') {
        ty = stripped.trim_start();
        if let Some(lifetime_rest) = ty.strip_prefix('\'') {
            ty = lifetime_rest
                .find(char::is_whitespace)
                .map_or(lifetime_rest, |idx| &lifetime_rest[idx..])
                .trim_start();
        }
        if let Some(stripped) = ty.strip_prefix("mut ") {
            ty = stripped.trim_start();
        }
    }

    ty.to_string()
}

fn parse_fixed_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N], CliError> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    let bytes =
        hex::decode(stripped).map_err(|e| CliError::Other(format!("invalid {label} hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| CliError::Other(format!("{label} must be {N} bytes")))
}

fn hex_json_value<T>(value: T) -> serde_json::Value
where
    T: AsRef<[u8]>,
{
    serde_json::Value::from(format!("0x{}", hex::encode(value.as_ref())))
}

fn summarize_exec_status(status: &ExecStatus) -> String {
    match status {
        ExecStatus::Ok => "succeeded".into(),
        ExecStatus::Reverted {
            message: Some(message),
        } => format!("reverted: {message}"),
        ExecStatus::Reverted { message: None } => "reverted without abort data".into(),
        ExecStatus::DeterministicError { kind } => {
            format!("failed with deterministic execution error ({kind})")
        }
        ExecStatus::NonDeterministicError => "failed with nondeterministic commit error".into(),
        ExecStatus::FailedGasSponsorship => "failed gas sponsorship".into(),
        ExecStatus::Other => "failed with unknown execution result".into(),
        _ => "failed with unsupported execution result".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ContractMetadata, MethodMetadata, ResolvedMethodMetadata, SpecMethodMetadata,
        build_type_def_index, contract_entrypoint_id, contract_metadata_from_source,
        decode_return_value, decode_spec_value_from_reader, encode_calldata,
        explicit_entrypoint_id, filter_candidates_by_interfaces,
        load_project_registered_interfaces, major_version, parse_contract_attr_value,
        resolve_local_contract_metadata_for_interface, resolve_spec_method, summarize_exec_status,
    };
    use crate::artifacts::ArtifactOptions;
    use crate::contract_spec::InterfaceSpec;
    use groundhog_contract_spec::{ContractArrayType, ContractVectorType};
    use groundhog_rpc_api_types::receipt::ExecStatus;
    use groundhog_spec_tool::{
        ContractEnumDef, ContractEnumVariant, ContractField, ContractSpec, ContractSpecInterface,
        ContractSpecMethod, ContractType, ContractTypeDef, ContractUnionCase,
        ContractUnionCaseType, ContractUnionDef,
    };
    use serde_json::json;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn parses_contract_attr_values() {
        let attr = r#"#[contract(interface = "Interface1", version = "1.2.3")]"#;
        assert_eq!(
            parse_contract_attr_value(attr, "interface").as_deref(),
            Some("Interface1")
        );
        assert_eq!(
            parse_contract_attr_value(attr, "version").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn extracts_metadata_from_bare_impl() {
        let source = r"
            #[contract(throws = Error, client = CounterClient)]
            impl Counter {
                pub fn get(&self) -> u64 { 0 }
            }
        ";
        let metadata = contract_metadata_from_source(
            source,
            Path::new("/tmp/implementation.rs"),
            "get",
            "0.1.0",
        );
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].interface_name, "Counter");
        assert_eq!(metadata[0].version, "0.1.0");
        assert_eq!(metadata[0].method.arg_types, Vec::<String>::new());
        assert_eq!(metadata[0].method.return_type.as_deref(), Some("u64"));
    }

    #[test]
    fn method_metadata_does_not_match_prefix_methods() {
        let source = r"
            #[contract(throws = Error, client = CounterClient)]
            impl Counter {
                pub fn get_all(&self, count: u64) -> Vec<u64> { vec![] }
                pub fn get(&self) -> u64 { 0 }
            }
        ";
        let metadata = contract_metadata_from_source(
            source,
            Path::new("/tmp/implementation.rs"),
            "get",
            "0.1.0",
        );
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].method.arg_types, Vec::<String>::new());
        assert_eq!(metadata[0].method.return_type.as_deref(), Some("u64"));
    }

    #[test]
    fn extracts_metadata_from_trait_impl_with_explicit_interface_and_version() {
        let source = r#"
            #[contract(interface = "Interface1", version = "1.2.3")]
            impl Interface for Stateless {
                fn perform_task() -> u32 { 1 }
            }
        "#;
        let metadata = contract_metadata_from_source(
            source,
            Path::new("/tmp/interface.rs"),
            "perform_task",
            "0.1.0",
        );
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].interface_name, "Interface1");
        assert_eq!(metadata[0].version, "1.2.3");
        assert_eq!(metadata[0].method.arg_types, Vec::<String>::new());
        assert_eq!(metadata[0].method.return_type.as_deref(), Some("u32"));
    }

    #[test]
    fn selector_uses_interface_and_major_version() {
        let _id = contract_entrypoint_id("Counter", "0.1.0", "get");
        assert_eq!(major_version("1.2.3-alpha"), "1");
        assert_eq!(major_version("0.9.2"), "0.9");
        assert_eq!(major_version("0.0.2"), "0.0.2");
    }

    #[test]
    fn parses_explicit_entrypoint_id() {
        let entrypoint = explicit_entrypoint_id("0x0123456789abcdef")
            .expect("bug: valid entrypoint hex should parse")
            .expect("bug: explicit entrypoint should be returned");
        assert_eq!(
            entrypoint.into_inner(),
            0x0123_4567_89AB_CDEF_u64.to_be_bytes()
        );
    }

    #[test]
    fn encodes_single_typed_argument_from_local_metadata() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec!["u64".into()],
                return_type: None,
            },
        };
        let encoded = encode_calldata(
            &["5".into()],
            Some(&ResolvedMethodMetadata::Local(metadata)),
        )
        .expect("bug: typed calldata should encode");
        let decoded: u64 =
            xdrs::cxs::decode(&encoded).expect("bug: encoded calldata should decode as u64");
        assert_eq!(decoded, 5);
    }

    #[test]
    fn rejects_multiple_raw_arguments_without_local_metadata() {
        let err = encode_calldata(&["0x01".into(), "0x02".into()], None)
            .expect_err("bug: multiple raw arguments should fail without metadata");
        assert_eq!(
            err.to_string(),
            "multiple call arguments require local contract metadata — run `groundhog call` from \
             inside the project that defines the contract, or pass a single hex-encoded calldata \
             argument"
        );
    }

    #[test]
    fn decodes_simple_return_value() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec![],
                return_type: Some("u64".into()),
            },
        };
        let bytes = xdrs::cxs::encode(&7_u64).expect("bug: u64 should encode");
        let decoded = decode_return_value(&bytes, Some(&ResolvedMethodMetadata::Local(metadata)))
            .expect("bug: simple return value should decode");
        assert_eq!(decoded, serde_json::json!(7));
    }

    #[test]
    fn encodes_and_decodes_embedded_spec_structured_arguments() {
        let metadata = sample_structured_spec_metadata(
            vec![
                contract_field("payload", named_type("Payload")),
                contract_field("mode", named_type("Mode")),
                contract_field(
                    "maybe_count",
                    ContractType::Option {
                        option_inner: Some(Box::new(ContractType::U32)),
                    },
                ),
                contract_field(
                    "digest",
                    ContractType::Array {
                        array: ContractArrayType {
                            element: Some(Box::new(ContractType::U8)),
                            len: 4,
                        },
                    },
                ),
                contract_field(
                    "flags",
                    ContractType::Vector {
                        vector: ContractVectorType {
                            element: Some(Box::new(ContractType::Bool)),
                            max_len: 8,
                        },
                    },
                ),
                contract_field(
                    "pair",
                    ContractType::Tuple {
                        tuple: vec![ContractType::U32, ContractType::String]
                            .try_into()
                            .unwrap(),
                    },
                ),
            ],
            ContractType::Void,
        );

        let encoded = encode_calldata(
            &[
                r#"{"count":7,"bytes":"hi"}"#.into(),
                "Ready".into(),
                "5".into(),
                "0x01020304".into(),
                "[true,false,true]".into(),
                r#"[7,"ok"]"#.into(),
            ],
            Some(&ResolvedMethodMetadata::Spec(metadata.clone())),
        )
        .expect("bug: embedded spec calldata should encode");

        let type_defs =
            build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
        let mut reader = xdrs::cxs::buf_reader::BufReader::new(&encoded);

        assert_eq!(
            decode_spec_value_from_reader(
                &metadata.method.args[0].ty,
                &mut reader,
                &type_defs,
                "arg0",
            )
            .expect("bug: struct arg should decode"),
            json!({"count": 7, "bytes": "0x6869"})
        );
        assert_eq!(
            decode_spec_value_from_reader(
                &metadata.method.args[1].ty,
                &mut reader,
                &type_defs,
                "arg1",
            )
            .expect("bug: enum arg should decode"),
            json!("Ready")
        );
        assert_eq!(
            decode_spec_value_from_reader(
                &metadata.method.args[2].ty,
                &mut reader,
                &type_defs,
                "arg2",
            )
            .expect("bug: option arg should decode"),
            json!(5)
        );
        assert_eq!(
            decode_spec_value_from_reader(
                &metadata.method.args[3].ty,
                &mut reader,
                &type_defs,
                "arg3",
            )
            .expect("bug: fixed byte array should decode"),
            json!("0x01020304")
        );
        assert_eq!(
            decode_spec_value_from_reader(
                &metadata.method.args[4].ty,
                &mut reader,
                &type_defs,
                "arg4",
            )
            .expect("bug: vector arg should decode"),
            json!([true, false, true])
        );
        assert_eq!(
            decode_spec_value_from_reader(
                &metadata.method.args[5].ty,
                &mut reader,
                &type_defs,
                "arg5",
            )
            .expect("bug: tuple arg should decode"),
            json!([7, "ok"])
        );
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn encodes_and_decodes_embedded_spec_unions() {
        let metadata = sample_structured_spec_metadata(
            vec![contract_field("envelope", named_type("Envelope"))],
            named_type("Envelope"),
        );

        let full_case = json!({
            "case": "Full",
            "value": {
                "payload": {
                    "count": 9,
                    "bytes": "0xaabb"
                },
                "enabled": true
            }
        });
        let encoded = encode_calldata(
            &[full_case.to_string()],
            Some(&ResolvedMethodMetadata::Spec(metadata.clone())),
        )
        .expect("bug: union calldata should encode");

        let type_defs =
            build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
        let mut reader = xdrs::cxs::buf_reader::BufReader::new(&encoded);
        let decoded = decode_spec_value_from_reader(
            &metadata.method.args[0].ty,
            &mut reader,
            &type_defs,
            "arg0",
        )
        .expect("bug: union arg should decode");
        assert_eq!(decoded, full_case);
        assert_eq!(reader.remaining(), 0);

        let pair_case = json!({
            "case": "Pair",
            "value": ["Busy", 9]
        });
        let pair_bytes = encode_calldata(
            &[pair_case.to_string()],
            Some(&ResolvedMethodMetadata::Spec(metadata.clone())),
        )
        .expect("bug: tuple union case should encode");
        let decoded_pair = decode_return_value(
            &pair_bytes,
            Some(&ResolvedMethodMetadata::Spec(metadata.clone())),
        )
        .expect("bug: union return should decode");
        assert_eq!(decoded_pair, pair_case);

        let empty_bytes = encode_calldata(
            &["Empty".into()],
            Some(&ResolvedMethodMetadata::Spec(metadata.clone())),
        )
        .expect("bug: void union shorthand should encode");
        let decoded_empty =
            decode_return_value(&empty_bytes, Some(&ResolvedMethodMetadata::Spec(metadata)))
                .expect("bug: void union return should decode");
        assert_eq!(decoded_empty, json!({"case": "Empty"}));
    }

    #[test]
    fn filters_candidates_by_interfaces() {
        let candidates = vec![
            ContractMetadata {
                interface_name: "Counter".into(),
                version: "0.1.0".into(),
                source: Path::new("/tmp/a.rs").to_path_buf(),
                method: MethodMetadata {
                    arg_types: vec![],
                    return_type: Some("u64".into()),
                },
            },
            ContractMetadata {
                interface_name: "Other".into(),
                version: "0.1.0".into(),
                source: Path::new("/tmp/b.rs").to_path_buf(),
                method: MethodMetadata {
                    arg_types: vec![],
                    return_type: Some("u64".into()),
                },
            },
        ];
        let interfaces = vec![InterfaceSpec {
            interface_name: "Counter".into(),
            interface_version: "0.1.0".into(),
        }];

        let filtered = filter_candidates_by_interfaces(candidates, Some(&interfaces));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].interface_name, "Counter");
    }

    #[test]
    #[serial]
    fn load_project_registered_interfaces_uses_explicit_target_dir() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let wasm = custom_target.join("groundhog").join("demo_contract.wasm");
        std::fs::create_dir_all(wasm.parent().expect("bug: wasm parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(&wasm, wasm_with_embedded_spec(&sample_contract_spec()))
            .expect("bug: wasm fixture should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let interfaces = load_project_registered_interfaces(&project_root, &artifacts)
            .expect("bug: registered interface lookup should succeed")
            .expect("bug: registered interfaces should resolve from explicit target dir");

        assert_eq!(
            interfaces,
            vec![InterfaceSpec {
                interface_name: "Counter".into(),
                interface_version: "0.1.0".into(),
            }]
        );
    }

    #[test]
    #[serial]
    fn resolve_spec_method_reports_multiple_interface_matches() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let wasm = custom_target.join("groundhog").join("demo_contract.wasm");
        std::fs::create_dir_all(wasm.parent().expect("bug: wasm parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(
            &wasm,
            wasm_with_embedded_spec(&sample_multi_interface_contract_spec()),
        )
        .expect("bug: wasm fixture should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let err = resolve_spec_method(&project_root, "get", &artifacts)
            .expect_err("bug: ambiguous embedded interface matches should fail");
        assert_eq!(
            err.to_string(),
            "method `get` matches multiple interfaces in the embedded contract spec: Counter \
             v0.1.0, Other v0.1.0"
        );
    }

    #[test]
    fn resolve_local_contract_metadata_for_interface_filters_to_requested_interface() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        write_project_config(&project_root);
        std::fs::write(
            project_root.join("src/lib.rs"),
            r#"
            #[contract(interface = "Counter", version = "0.1.0")]
            impl Counter {
                pub fn get(&self) -> u64 { 0 }
            }

            #[contract(interface = "Other", version = "0.1.0")]
            impl Other {
                pub fn get(&self) -> u64 { 1 }
            }
            "#,
        )
        .expect("bug: lib.rs should be written");

        let metadata =
            resolve_local_contract_metadata_for_interface(&project_root, "get", "Other", "0.1.0")
                .expect("bug: local metadata lookup should succeed")
                .expect("bug: matching interface metadata should exist");

        assert_eq!(metadata.interface_name, "Other");
        assert_eq!(metadata.version, "0.1.0");
        assert_eq!(metadata.method.arg_types, Vec::<String>::new());
        assert_eq!(metadata.method.return_type.as_deref(), Some("u64"));
    }

    #[test]
    fn summarizes_revert_without_message() {
        assert_eq!(
            summarize_exec_status(&ExecStatus::Reverted { message: None }),
            "reverted without abort data"
        );
    }

    #[test]
    fn normalize_type_name_strips_references_without_clobbering_prefixes() {
        assert_eq!(super::normalize_type_name("&str"), "str");
        assert_eq!(super::normalize_type_name("&mut str"), "str");
        assert_eq!(super::normalize_type_name("&'a str"), "str");
        assert_eq!(super::normalize_type_name("&String"), "String");
        assert_eq!(super::normalize_type_name("&strange"), "strange");
    }

    fn create_temp_project(package_name: &str) -> TempDir {
        let project = tempfile::tempdir().expect("bug: temp dir should be created");
        write_project_manifest(project.path(), package_name);
        std::fs::create_dir_all(project.path().join("src"))
            .expect("bug: src dir should be created");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn demo() {}\n")
            .expect("bug: lib.rs should be written");
        project
    }

    fn write_project_manifest(project_root: &Path, package_name: &str) {
        std::fs::write(
            project_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            ),
        )
        .expect("bug: Cargo.toml should be written");
    }

    fn write_project_config(project_root: &Path) {
        std::fs::write(project_root.join("groundhog.toml"), "")
            .expect("bug: groundhog.toml should be written");
    }

    fn sample_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
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

    fn sample_structured_spec_metadata(
        args: Vec<ContractField>,
        return_type: ContractType,
    ) -> SpecMethodMetadata {
        SpecMethodMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            method: ContractSpecMethod {
                name: spec_string_bytes("call"),
                args: args.try_into().unwrap(),
                return_type,
            },
            type_defs: vec![
                ContractTypeDef::TypeDefStruct {
                    struct_def: groundhog_spec_tool::ContractStructDef {
                        name: spec_string_bytes("Payload"),
                        fields: vec![
                            contract_field("count", ContractType::U32),
                            contract_field(
                                "bytes",
                                ContractType::Vector {
                                    vector: ContractVectorType {
                                        element: Some(Box::new(ContractType::U8)),
                                        max_len: u32::MAX,
                                    },
                                },
                            ),
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
                ContractTypeDef::TypeDefEnum {
                    enum_def: ContractEnumDef {
                        name: spec_string_bytes("Mode"),
                        variants: vec![
                            ContractEnumVariant {
                                name: spec_string_bytes("Ready"),
                                value: 1,
                            },
                            ContractEnumVariant {
                                name: spec_string_bytes("Busy"),
                                value: 2,
                            },
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
                ContractTypeDef::TypeDefUnion {
                    union_def: ContractUnionDef {
                        name: spec_string_bytes("Envelope"),
                        cases: vec![
                            ContractUnionCase {
                                name: spec_string_bytes("Empty"),
                                discriminant: 0,
                                case_type: ContractUnionCaseType::UnionCaseVoid,
                            },
                            ContractUnionCase {
                                name: spec_string_bytes("Full"),
                                discriminant: 1,
                                case_type: ContractUnionCaseType::UnionCaseStruct {
                                    struct_fields: vec![
                                        contract_field("payload", named_type("Payload")),
                                        contract_field("enabled", ContractType::Bool),
                                    ]
                                    .try_into()
                                    .unwrap(),
                                },
                            },
                            ContractUnionCase {
                                name: spec_string_bytes("Pair"),
                                discriminant: 2,
                                case_type: ContractUnionCaseType::UnionCaseTuple {
                                    tuple: vec![named_type("Mode"), ContractType::U32]
                                        .try_into()
                                        .unwrap(),
                                },
                            },
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
            ],
        }
    }

    fn contract_field(name: &str, ty: ContractType) -> ContractField {
        ContractField {
            name: spec_string_bytes(name),
            ty,
        }
    }

    fn named_type(name: &str) -> ContractType {
        ContractType::Named {
            named: spec_string_bytes(name),
        }
    }

    fn spec_string_bytes(value: &str) -> groundhog_spec_tool::ContractSpecString {
        value.try_into().expect("bug: test string should fit")
    }

    fn wasm_with_embedded_spec(spec: &ContractSpec) -> Vec<u8> {
        wasm_with_custom_section(
            groundhog_spec_tool::FINAL_SECTION_NAME,
            &xdrs::txs::encode(spec).expect("bug: contract spec fixture should encode"),
        )
    }

    fn wasm_with_custom_section(name: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        encode_uleb128(name.len(), &mut payload);
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(data);

        let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x00];
        encode_uleb128(payload.len(), &mut wasm);
        wasm.extend_from_slice(&payload);
        wasm
    }

    fn encode_uleb128(mut value: usize, out: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
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

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("bug: current dir should resolve");
            std::env::set_current_dir(path).expect("bug: current dir should change");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("bug: current dir should restore");
        }
    }
}
