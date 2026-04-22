use std::path::{Path, PathBuf};

use groundhog_rpc_api_types::receipt::ExecStatus;
use groundhog_signing::{
    BuildCallTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams, Signer,
    build_call_transaction, sign_transaction,
};
use groundhog_xdr::auth_claim::AuthClaim;
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
use serde_json::json;
use sha2::{Digest, Sha256};

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
struct SpecMethodMetadata {
    interface_name: String,
    version: String,
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
            resolve_local_contract_metadata_for_interface(
                &cwd,
                method,
                &metadata.interface_name,
                &metadata.version,
            )?,
        ),
        SelectorSource::Local(metadata) => (
            contract_entrypoint_id(&metadata.interface_name, &metadata.version, method),
            Some(metadata),
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

    let mut matches = find_method_interfaces(&spec, method)?;

    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        let descriptions = matches
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

    let matched = matches.remove(0);
    Ok(Some(SpecMethodMetadata {
        interface_name: matched.interface_name,
        version: matched.interface_version,
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

fn encode_calldata(
    args: &[String],
    metadata: Option<&ContractMetadata>,
) -> Result<Vec<u8>, CliError> {
    if let Some(metadata) = metadata {
        return encode_local_typed_calldata(args, &metadata.method);
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
        "String" | "&str" => encode_xdr_value(value.to_string()),
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

fn decode_return_value(
    bytes: &[u8],
    metadata: Option<&ContractMetadata>,
) -> Option<serde_json::Value> {
    let ty = metadata?.method.return_type.as_deref()?;
    decode_single_value(ty, bytes)
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
        "String" | "&str" => decode_xdr_value::<String>(bytes).map(serde_json::Value::from),
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
        ContractMetadata, MethodMetadata, contract_entrypoint_id, contract_metadata_from_source,
        decode_return_value, encode_calldata, explicit_entrypoint_id,
        filter_candidates_by_interfaces, load_project_registered_interfaces, major_version,
        parse_contract_attr_value, summarize_exec_status,
    };
    use crate::artifacts::ArtifactOptions;
    use crate::contract_spec::InterfaceSpec;
    use groundhog_rpc_api_types::receipt::ExecStatus;
    use groundhog_spec_tool::{
        ContractSpec, ContractSpecInterface, ContractSpecMethod, ContractType,
    };
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
        let encoded = encode_calldata(&["5".into()], Some(&metadata))
            .expect("bug: typed calldata should encode");
        let decoded: u64 =
            xdrs::cxs::decode(&encoded).expect("bug: encoded calldata should decode as u64");
        assert_eq!(decoded, 5);
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
        let decoded = decode_return_value(&bytes, Some(&metadata))
            .expect("bug: simple return value should decode");
        assert_eq!(decoded, serde_json::json!(7));
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

    fn sample_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
                name: spec_string("Counter"),
                vers: spec_string("0.1.0"),
                throws: vec![].try_into().unwrap(),
                methods: vec![ContractSpecMethod {
                    name: spec_string("get"),
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

    fn spec_string(value: &str) -> groundhog_spec_tool::ContractSpecString {
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
