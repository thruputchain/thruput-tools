use std::path::{Path, PathBuf};

use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams, build_deploy_transaction,
    derive_deploy_address, sign_and_encode_transaction_hex, source_address_auth_claim,
};
use groundhog_xdr::types::ContractAddress;
use groundhog_xdr::types::SHA256Hash;
use serde_json::json;
use sha2::{Digest, Sha256};
use xdr_common::VecM;

use crate::account_resolution::resolve_wallet_address;
use crate::artifacts::{ArtifactKind, ArtifactOptions, ResolvedArtifact};
use crate::config::{
    ProfileOverrides, ResolvedProfile, resolve_default_key, resolve_profile_with_overrides,
};
use crate::dev_detail::{fetch_transaction_detail_blocking, print_transaction_detail};
use crate::keys::load_local_signer;
use crate::network_descriptor::resolve_wallet_entrypoints;
use crate::result::CommandResult;
use crate::tx_utils::{
    command_error_for_exec_status, format_tx_hash, nonce_from_timestamp, print_failed_receipt_data,
    resolve_block_expiry_height, submit_and_poll, summarize_exec_status,
};
use crate::{CliError, Print};

pub const DEPLOY_JSON_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployAddressSource {
    Receipt,
    Derived,
}

/// Fully prepared deploy submission inputs shared by live execution and dry-run flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedDeploy {
    pub(crate) rpc_url: String,
    pub(crate) artifact: ResolvedArtifact,
    pub(crate) artifact_bytes_len: usize,
    pub(crate) predicted_address: String,
    pub(crate) wallet_entrypoint_id: String,
    pub(crate) wallet_entrypoint_source: &'static str,
    pub(crate) tx_hex: String,
}

/// Submitted deploy metadata after a live deploy transaction is accepted.
#[derive(Debug, Clone)]
pub(crate) struct SubmittedDeploy {
    pub(crate) tx_hash: TxHash,
    pub(crate) receipt: TransactionReceipt,
    pub(crate) contract_address: Option<String>,
    pub(crate) address_source: Option<DeployAddressSource>,
}

/// Deploy a contract to the network.
#[allow(clippy::too_many_arguments)]
pub fn run(
    artifact_path: Option<&Path>,
    target: ArtifactKind,
    dry_run: bool,
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let prepared = prepare_deploy(artifact_path, target, artifacts, profile, overrides, print)?;
    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    let task = print.start_task(format_args!(
        "Deploying {} artifact",
        prepared.artifact.kind.as_str()
    ));

    let submitted = submit_prepared_deploy_and_resolve(&prepared)?;
    task.clear();
    let tx_hash = submitted.tx_hash;
    let receipt = submitted.receipt;
    let transaction_detail = fetch_transaction_detail_blocking(&prepared.rpc_url, tx_hash, print);
    let contract_addr_hex = submitted.contract_address;
    let address_source = submitted.address_source;

    // Print result
    let cwd = std::env::current_dir()?;
    let tx_hash_formatted = format_tx_hash(&tx_hash);
    match &receipt.status {
        groundhog_json_rpc_types::receipt::ExecStatus::Ok => {
            let contract_addr_hex = contract_addr_hex
                .as_deref()
                .expect("bug: successful deploy must resolve a contract address");
            print.success("Deployed contract");
            print.detail("address", contract_addr_hex);
            if matches!(address_source, Some(DeployAddressSource::Derived)) {
                print.warn(
                    "receipt omitted deploy return_data; address was derived locally from \
                     deployer, nonce, and contract hash",
                );
            }
            print.detail("tx", &tx_hash_formatted);
            print.detail("height", *receipt.height);
        }
        other => {
            print.warn(format_args!("Deploy {}", summarize_exec_status(other)));
            print.detail("tx", &tx_hash_formatted);
            print.detail("height", *receipt.height);
            print_failed_receipt_data(&receipt, print);
        }
    }

    // Save contract alias
    if matches!(
        &receipt.status,
        groundhog_json_rpc_types::receipt::ExecStatus::Ok
    ) {
        let project_root = crate::config::find_project_config(&cwd)
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if let Some(root) = project_root
            && let Some(name) = root.file_name().and_then(|n| n.to_str())
            && let Some(contract_addr_hex) = contract_addr_hex.as_deref()
        {
            let _ =
                crate::config::save_profile_contract_alias(&root, profile, name, contract_addr_hex);
            print.detail("alias", format_args!("{name} -> {contract_addr_hex}"));
        }
    }

    if let Some(detail) = transaction_detail.as_ref() {
        print_transaction_detail(detail, print);
    }

    let command_error = command_error_for_exec_status(&receipt.status);
    let mut data = json!({
        "schemaVersion": DEPLOY_JSON_SCHEMA_VERSION,
        "tx_hash": tx_hash_formatted,
        "artifact_kind": prepared.artifact.kind.as_str(),
        "artifact_path": &prepared.artifact.path,
        "artifact_bytes": prepared.artifact_bytes_len,
        "address": contract_addr_hex,
        "address_source": match address_source {
            Some(DeployAddressSource::Receipt) => Some("receipt"),
            Some(DeployAddressSource::Derived) => Some("derived"),
            None => None,
        },
        "height": receipt.height,
        "status": receipt.status,
        "transaction_detail": transaction_detail,
        "wallet_entrypoint_id": prepared.wallet_entrypoint_id,
        "wallet_entrypoint_source": prepared.wallet_entrypoint_source,
    });
    if command_error.is_some() {
        data["return_data"] = json!(receipt.return_data);
        data["abort_data"] = json!(receipt.abort_data);
    }
    if let Some(error) = command_error {
        print.json(&CommandResult::failure_with_error(cmd, data, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::new(cmd, data))?;

    Ok(())
}

fn print_dry_run(
    prepared: &PreparedDeploy,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Prepared dry-run deploy for {} artifact ({} bytes) to {}.",
        prepared.artifact.kind.as_str(),
        prepared.artifact_bytes_len,
        prepared.rpc_url
    ));
    print.status(format_args!(
        "  predicted address: {}",
        prepared.predicted_address
    ));
    print.status(format_args!("  signed tx: {}", prepared.tx_hex));
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": DEPLOY_JSON_SCHEMA_VERSION,
            "dry_run": true,
            "submitted": false,
            "rpc_url": prepared.rpc_url,
            "artifact_kind": prepared.artifact.kind.as_str(),
            "artifact_path": &prepared.artifact.path,
            "artifact_bytes": prepared.artifact_bytes_len,
            "predicted_address": prepared.predicted_address,
            "wallet_entrypoint_id": prepared.wallet_entrypoint_id,
            "wallet_entrypoint_source": prepared.wallet_entrypoint_source,
            "signed_tx": prepared.tx_hex,
        }),
    ))?;

    Ok(())
}

/// Submit a previously prepared deploy transaction and wait for its receipt.
pub(crate) fn submit_prepared_deploy(
    prepared: &PreparedDeploy,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    rt.block_on(async { submit_and_poll(&prepared.rpc_url, &prepared.tx_hex).await })
}

/// Submit a deploy transaction and resolve the deployed contract address when successful.
pub(crate) fn submit_prepared_deploy_and_resolve(
    prepared: &PreparedDeploy,
) -> Result<SubmittedDeploy, CliError> {
    let (tx_hash, receipt) = submit_prepared_deploy(prepared)?;
    let predicted_address = ContractAddress::from(hex_to_32_bytes(&prepared.predicted_address)?);
    let (contract_address, address_source) = match &receipt.status {
        groundhog_json_rpc_types::receipt::ExecStatus::Ok => {
            let (address, source) =
                resolve_deployed_address(receipt.return_data.as_ref(), &predicted_address)?;
            (Some(address), Some(source))
        }
        _ => (None, None),
    };

    Ok(SubmittedDeploy {
        tx_hash,
        receipt,
        contract_address,
        address_source,
    })
}

/// Resolve an artifact, build and sign a deploy transaction without submitting it.
#[allow(clippy::too_many_lines)]
pub(crate) fn prepare_deploy(
    artifact_path: Option<&Path>,
    target: ArtifactKind,
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    print: &Print,
) -> Result<PreparedDeploy, CliError> {
    let cwd = std::env::current_dir()?;
    let resolved = resolve_profile_with_overrides(&cwd, profile, overrides)?;

    // Resolve signing key
    let key_name = resolve_default_key(&cwd, profile)?;
    if resolved.rpc_url.is_none() && key_name.is_none() {
        return Err(CliError::MissingRpcUrl);
    }
    let key_name = key_name.ok_or(CliError::MissingSigningKey)?;
    resolved.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;

    let artifact = if let Some(path) = artifact_path {
        ResolvedArtifact {
            kind: crate::artifacts::infer_artifact_kind(path)?,
            path: path.to_path_buf(),
        }
    } else {
        let path = crate::artifacts::resolve_groundhog_artifact_path(&cwd, artifacts, target)?;
        let artifact = ResolvedArtifact { kind: target, path };
        warn_if_stale(&artifact, &cwd, print);
        artifact
    };
    let artifact_bytes = std::fs::read(&artifact.path)?;

    prepare_deploy_transaction(artifact, artifact_bytes, &resolved, &key_name)
}

/// Build and sign a deploy transaction for a known artifact and RPC endpoint.
pub(crate) fn prepare_deploy_for_rpc_with_manifest(
    artifact_path: &Path,
    artifact_kind: ArtifactKind,
    rpc_url: &str,
    key_name: &str,
    runtime_manifest: Option<PathBuf>,
) -> Result<PreparedDeploy, CliError> {
    let artifact_bytes = std::fs::read(artifact_path)?;
    let profile = ResolvedProfile {
        name: "integration".into(),
        network: None,
        rpc_url: Some(rpc_url.into()),
        rpc_url_cli_override: false,
        faucet_url: None,
        default_key: Some(key_name.into()),
        runtime_manifest,
        runtime_manifest_cli_override: false,
        runtime_artifacts_dir: None,
        contracts: std::collections::BTreeMap::default(),
        wallets: std::collections::BTreeMap::default(),
        source: None,
    };
    prepare_deploy_transaction(
        ResolvedArtifact {
            kind: artifact_kind,
            path: artifact_path.to_path_buf(),
        },
        artifact_bytes,
        &profile,
        key_name,
    )
}

#[allow(clippy::too_many_lines)]
fn prepare_deploy_transaction(
    artifact: ResolvedArtifact,
    artifact_bytes: Vec<u8>,
    profile: &ResolvedProfile,
    key_name: &str,
) -> Result<PreparedDeploy, CliError> {
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let signer = load_local_signer(key_name)?;

    let artifact_bytes_len = artifact_bytes.len();

    // Build the deploy transaction — invokes the sender's wallet contract.
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let wallet_address = resolve_wallet_address(profile, key_name, &signer)?;

    let blob = groundhog_xdr::contract::ContractBlob {
        data: xdr_common::BytesM(artifact_bytes),
    };
    let contract = match artifact.kind {
        ArtifactKind::Wasm => groundhog_xdr::contract::Contract::kWasmi {
            wasmi_contract: blob,
        },
        ArtifactKind::Rv32 => groundhog_xdr::contract::Contract::kRv32 {
            rv32_contract: blob,
        },
    };

    // Compute the contract hash: sha256(xdr_encode(contract)).
    let contract_xdr = xdrs::txs::encode(&contract)
        .map_err(|e| CliError::Other(format!("failed to XDR-encode contract: {e}")))?;
    let contract_hash_bytes: [u8; 32] = Sha256::digest(&contract_xdr).into();
    let contract_hash = SHA256Hash::from(contract_hash_bytes);

    // Create a deploy nonce for address derivation.
    let deploy_nonce_val = nonce_from_timestamp();
    let deploy_nonce_bytes: [u8; 32] = Sha256::digest(deploy_nonce_val.to_le_bytes()).into();
    let deploy_nonce = SHA256Hash::from(deploy_nonce_bytes);
    let predicted_address = derive_deploy_address(&wallet_address, &deploy_nonce, &contract_hash)
        .map_err(|e| CliError::Other(e.to_string()))?;

    // Calldata = CXS-encoded (nonce, contract_hash) — matches wallet deploy signature.
    // Must use CXS encoding (not txs) because the #[contract] macro
    // deserializes calldata with xdrs::cxs::decode.
    let calldata = xdrs::cxs::encode(&(&deploy_nonce, &contract_hash))
        .map_err(|e| CliError::Other(format!("failed to encode deploy calldata: {e}")))?;

    // Auth claim: source-address auth data at key slot 0.
    let auth_claim = source_address_auth_claim();

    let wallet_entrypoints = resolve_wallet_entrypoints(profile, key_name)?;

    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(rpc_url).await })?;

    let tx = build_deploy_transaction(BuildDeployTransaction {
        invoked_address: wallet_address,
        entrypoint: wallet_entrypoints.deploy,
        calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce: deploy_nonce_val,
        contracts_to_deploy: VecM(vec![contract]),
        auth_claim,
    });

    let tx_hex = sign_and_encode_transaction_hex(tx, &signer, SignTransactionParams::default())
        .map_err(|e| CliError::Other(e.to_string()))?;

    Ok(PreparedDeploy {
        rpc_url: rpc_url.to_owned(),
        artifact,
        artifact_bytes_len,
        predicted_address: format!("0x{}", hex::encode(predicted_address.as_ref())),
        wallet_entrypoint_id: format!("0x{}", hex::encode(wallet_entrypoints.deploy.into_inner())),
        wallet_entrypoint_source: wallet_entrypoints.source,
        tx_hex,
    })
}

/// Parse a `0x`-prefixed hex string into a 32-byte array.
fn hex_to_32_bytes(hex_str: &str) -> Result<[u8; 32], CliError> {
    let stripped = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = hex::decode(stripped)
        .map_err(|e| CliError::Other(format!("invalid predicted address hex: {e}")))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| CliError::Other("predicted address must be exactly 32 bytes".into()))
}

fn warn_if_stale(artifact: &ResolvedArtifact, project_start: &Path, print: &Print) {
    let Ok(artifact_mtime) = std::fs::metadata(&artifact.path).and_then(|m| m.modified()) else {
        return;
    };
    let src_dir = match crate::config::find_project_config(project_start)
        .and_then(|p| p.parent().map(|d| d.join("src")))
    {
        Some(d) if d.is_dir() => d,
        _ => return,
    };
    if newest_source_mtime(&src_dir).is_some_and(|t| t > artifact_mtime) {
        print.warn(format_args!(
            "source files are newer than the {} artifact — run `groundhog build --target {}` to avoid deploying stale code",
            artifact.kind.as_str(),
            artifact.kind.as_str()
        ));
    }
}

fn newest_source_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            if let Some(t) = newest_source_mtime(&path) {
                newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
            && let Ok(t) = std::fs::metadata(&path).and_then(|m| m.modified())
        {
            newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
        }
    }
    newest
}

fn resolve_deployed_address(
    return_data: Option<&HexBytes>,
    predicted_address: &ContractAddress,
) -> Result<(String, DeployAddressSource), CliError> {
    if let Some(return_data) = return_data {
        if return_data.0.len() == 32 {
            return Ok((
                format!("0x{}", hex::encode(&return_data.0)),
                DeployAddressSource::Receipt,
            ));
        }
        return Err(CliError::Other(format!(
            "deploy receipt return_data had {} bytes, expected 32-byte contract address",
            return_data.0.len()
        )));
    }

    Ok((
        format!("0x{}", hex::encode(predicted_address.as_ref())),
        DeployAddressSource::Derived,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        DeployAddressSource, derive_deploy_address, hex_to_32_bytes, resolve_deployed_address,
    };
    use crate::artifacts::{ArtifactKind, ArtifactOptions, resolve_groundhog_artifact_path};
    use groundhog_json_rpc_types::hex::HexBytes;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn hex_to_32_bytes_parses_prefixed_hex() {
        let input = format!("0x{}", "ab".repeat(32));
        let bytes = hex_to_32_bytes(&input).expect("bug: valid prefixed hex should parse");
        assert_eq!(bytes, [0xab; 32]);
    }

    #[test]
    fn hex_to_32_bytes_parses_unprefixed_hex() {
        let input = "cd".repeat(32);
        let bytes = hex_to_32_bytes(&input).expect("bug: valid unprefixed hex should parse");
        assert_eq!(bytes, [0xcd; 32]);
    }

    #[test]
    fn hex_to_32_bytes_rejects_wrong_length() {
        let short = format!("0x{}", "ab".repeat(16));
        assert!(hex_to_32_bytes(&short).is_err());
    }

    #[test]
    fn hex_to_32_bytes_rejects_invalid_hex() {
        let bad = format!("0x{}", "zz".repeat(32));
        assert!(hex_to_32_bytes(&bad).is_err());
    }

    #[test]
    fn deploy_address_resolution_falls_back_to_prediction() {
        let predicted = ContractAddress::from([0x11; 32]);
        let resolved = resolve_deployed_address(None, &predicted)
            .expect("bug: predicted address fallback should succeed");
        assert_eq!(resolved.0, format!("0x{}", "11".repeat(32)));
        assert_eq!(resolved.1, DeployAddressSource::Derived);
    }

    #[test]
    fn deploy_address_resolution_prefers_receipt_return_data() {
        let predicted = ContractAddress::from([0x11; 32]);
        let resolved = resolve_deployed_address(Some(&HexBytes(vec![0x22; 32])), &predicted)
            .expect("bug: receipt return_data should be accepted");
        assert_eq!(resolved.0, format!("0x{}", "22".repeat(32)));
        assert_eq!(resolved.1, DeployAddressSource::Receipt);
    }

    #[test]
    fn deploy_address_derivation_is_deterministic() {
        let deployer = ContractAddress::from([0xAB; 32]);
        let nonce = SHA256Hash::from([0xCD; 32]);
        let contract_hash = SHA256Hash::from([0xEF; 32]);

        let first = derive_deploy_address(&deployer, &nonce, &contract_hash)
            .expect("bug: derivation should succeed");
        let second = derive_deploy_address(&deployer, &nonce, &contract_hash)
            .expect("bug: derivation should succeed");

        assert_eq!(first, second);
        assert_ne!(first, deployer);
    }

    #[test]
    #[serial]
    fn resolve_artifact_path_uses_explicit_target_dir() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let expected_wasm = custom_target.join("groundhog").join("demo_contract.wasm");
        std::fs::create_dir_all(
            expected_wasm
                .parent()
                .expect("bug: wasm parent should exist"),
        )
        .expect("bug: custom target dir should be created");
        std::fs::write(&expected_wasm, b"\0asm").expect("bug: wasm fixture should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let resolved =
            resolve_groundhog_artifact_path(&project_root, &artifacts, ArtifactKind::Wasm)
                .expect("bug: wasm should resolve from explicit target dir");

        assert_eq!(resolved, expected_wasm);
    }

    #[test]
    #[serial]
    fn resolve_artifact_path_reports_selected_missing_artifact() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let expected_rv32 = custom_target.join("groundhog").join("demo_contract.rv32a");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let err = resolve_groundhog_artifact_path(&project_root, &artifacts, ArtifactKind::Rv32)
            .expect_err("bug: missing rv32 artifact should error");

        assert!(
            matches!(err, crate::CliError::ArtifactNotFound { kind: "rv32", path } if path == expected_rv32.display().to_string())
        );
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
