use std::path::{Path, PathBuf};

use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams,
    build_deploy_transaction, derive_deploy_address, sign_and_encode_transaction_hex,
    source_address_auth_claim,
};
use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
use serde_json::json;
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

use crate::account_resolution::{LocalBootstrapRole, resolve_local_bootstrap_account};
use crate::config::{
    find_project_config, resolve_default_key, resolve_profile, save_profile_wallet_address,
};
use crate::keys::load_local_signer;
use crate::result::CommandResult;
use crate::runtime_manifest::{
    apply_local_node_state_for_local_bootstrap, load_configured_runtime_manifest,
};
use crate::tx_utils::{
    format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height,
    submit_and_poll_with_tx_hash, summarize_exec_status,
};
use crate::{CliError, Print};

pub const WALLET_CREATE_JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedWalletCreate {
    pub(crate) rpc_url: String,
    pub(crate) key_name: String,
    pub(crate) sponsor_key_name: String,
    pub(crate) sponsor_role: String,
    pub(crate) owner_public_key: String,
    pub(crate) artifact_path: PathBuf,
    pub(crate) wallet_address: String,
    pub(crate) create_tx_hex: String,
}

pub fn create(
    key_name: Option<&str>,
    dry_run: bool,
    profile: &str,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let prepared = prepare_wallet_create(&cwd, key_name, profile)?;

    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    print.status(format_args!(
        "Creating wallet for key '{}'...",
        prepared.key_name
    ));

    let (create_tx_hash, create_receipt) =
        submit_prepared_create_tx(&prepared.rpc_url, &prepared.create_tx_hex, print)?;
    let create_tx_hash = format_tx_hash(&create_tx_hash);
    ensure_success("wallet create", &create_receipt, &create_tx_hash)?;

    let project_root = find_project_config(&cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);
    save_profile_wallet_address(
        &project_root,
        Some(profile),
        &prepared.key_name,
        &prepared.wallet_address,
    )?;

    print.status(format_args!("Wallet created: {}", prepared.wallet_address));
    print.status(format_args!("Txn: {create_tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": WALLET_CREATE_JSON_SCHEMA_VERSION,
            "key": prepared.key_name,
            "wallet": prepared.wallet_address,
            "sponsor": {
                "key": prepared.sponsor_key_name,
                "role": prepared.sponsor_role,
            },
            "owner_public_key": prepared.owner_public_key,
            "artifact_path": prepared.artifact_path,
            "create_tx_hash": create_tx_hash,
            "create_height": create_receipt.height,
        }),
    ))?;

    Ok(())
}

fn print_dry_run(
    prepared: &PreparedWalletCreate,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Prepared dry-run wallet create for key `{}` on {}.",
        prepared.key_name, prepared.rpc_url
    ));
    print.status(format_args!("  wallet: {}", prepared.wallet_address));
    print.status(format_args!(
        "  sponsor: {} ({})",
        prepared.sponsor_key_name, prepared.sponsor_role
    ));
    print.status(format_args!("  owner: {}", prepared.owner_public_key));
    print.status(format_args!("  signed tx: {}", prepared.create_tx_hex));
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": WALLET_CREATE_JSON_SCHEMA_VERSION,
            "dry_run": true,
            "submitted": false,
            "key": prepared.key_name,
            "wallet": prepared.wallet_address,
            "sponsor": {
                "key": prepared.sponsor_key_name,
                "role": prepared.sponsor_role,
            },
            "owner_public_key": prepared.owner_public_key,
            "artifact_path": prepared.artifact_path,
            "signed_tx": prepared.create_tx_hex,
        }),
    ))?;

    Ok(())
}

pub(crate) fn prepare_wallet_create(
    cwd: &Path,
    key_name: Option<&str>,
    profile_name: &str,
) -> Result<PreparedWalletCreate, CliError> {
    let mut profile = resolve_profile(cwd, Some(profile_name))?;
    apply_local_node_state_for_local_bootstrap(&mut profile)?;
    let key_name = match key_name {
        Some(key_name) => key_name.to_owned(),
        None => resolve_default_key(cwd, Some(profile_name))?.ok_or(CliError::MissingSigningKey)?,
    };
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let manifest =
        load_configured_runtime_manifest(&profile, "wallet_sponsor")?.ok_or_else(|| {
            CliError::Other("wallet creation requires a local runtime manifest".into())
        })?;
    let create_wallet_entrypoint = manifest
        .bootstrap
        .dev_wallet
        .create_wallet_entrypoint_id()
        .map_err(|error| CliError::Other(error.to_string()))?;
    let sponsor = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::WalletSponsor)?;
    let owner = load_local_signer(&key_name)?;
    let owner_public_key = format!("0x{}", hex::encode(owner.verifying_key_bytes()));
    let (artifact_path, contract, contract_hash) = load_dev_wallet_contract(
        cwd,
        profile.source.as_deref(),
        &manifest.bootstrap.dev_wallet.artifact_hint,
    )?;
    let deploy_nonce_val = nonce_from_timestamp();
    let deploy_nonce = deploy_nonce_from_u64(deploy_nonce_val);
    let wallet_address =
        derive_deploy_address(&sponsor.wallet_address, &deploy_nonce, &contract_hash)
            .map_err(|error| CliError::Other(error.to_string()))?;
    let wallet_address_hex = format_contract_address(&wallet_address);
    let create_calldata = create_wallet_calldata(
        &deploy_nonce,
        &contract_hash,
        Ed25519PublicKey::from(owner.verifying_key_bytes()),
    )?;

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(&rpc_url).await })?;
    let create_tx_hex = sign_create_wallet_transaction(SignCreateWalletTransaction {
        sponsor_wallet_address: sponsor.wallet_address,
        create_wallet_entrypoint,
        create_calldata,
        block_expiry_height,
        nonce: deploy_nonce_val,
        contract,
        signer: &sponsor.signer,
    })?;

    Ok(PreparedWalletCreate {
        rpc_url,
        key_name,
        sponsor_key_name: sponsor.key_name,
        sponsor_role: sponsor.role,
        owner_public_key,
        artifact_path,
        wallet_address: wallet_address_hex,
        create_tx_hex,
    })
}

struct SignCreateWalletTransaction<'a> {
    sponsor_wallet_address: ContractAddress,
    create_wallet_entrypoint: ContractEntrypointID,
    create_calldata: Vec<u8>,
    block_expiry_height: i64,
    nonce: u64,
    contract: Contract,
    signer: &'a LocalSigner,
}

fn load_dev_wallet_contract(
    cwd: &Path,
    profile_source: Option<&Path>,
    artifact_hint: &str,
) -> Result<(PathBuf, Contract, SHA256Hash), CliError> {
    let artifact_path = resolve_dev_wallet_artifact_path(cwd, profile_source, artifact_hint)?;
    let artifact_bytes = std::fs::read(&artifact_path)?;
    let contract = dev_wallet_contract(&artifact_path, artifact_bytes);
    let contract_hash = contract_hash(&contract)?;
    Ok((artifact_path, contract, contract_hash))
}

fn deploy_nonce_from_u64(nonce: u64) -> SHA256Hash {
    let bytes: [u8; 32] = Sha256::digest(nonce.to_le_bytes()).into();
    SHA256Hash::from(bytes)
}

fn create_wallet_calldata(
    deploy_nonce: &SHA256Hash,
    contract_hash: &SHA256Hash,
    owner: Ed25519PublicKey,
) -> Result<Vec<u8>, CliError> {
    xdrs::cxs::encode(&(deploy_nonce, contract_hash, &owner)).map_err(|error| {
        CliError::Other(format!("failed to encode wallet create calldata: {error}"))
    })
}

fn sign_create_wallet_transaction(
    args: SignCreateWalletTransaction<'_>,
) -> Result<String, CliError> {
    let create_tx = build_deploy_transaction(BuildDeployTransaction {
        invoked_address: args.sponsor_wallet_address,
        entrypoint: args.create_wallet_entrypoint,
        calldata: args.create_calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
        contracts_to_deploy: VecM(vec![args.contract]),
        auth_claim: source_address_auth_claim(),
    });
    sign_and_encode_transaction_hex(create_tx, args.signer, SignTransactionParams::default())
        .map_err(|error| CliError::Other(error.to_string()))
}

fn submit_prepared_create_tx(
    rpc_url: &str,
    tx_hex: &str,
    _print: &Print,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    rt.block_on(async { submit_and_poll_with_tx_hash(rpc_url, tx_hex, |_| {}).await })
}

fn ensure_success(
    label: &str,
    receipt: &TransactionReceipt,
    tx_hash: &str,
) -> Result<(), CliError> {
    if matches!(receipt.status, ExecStatus::Ok) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "{label} {}; tx: {tx_hash}",
        summarize_exec_status(&receipt.status),
    )))
}

fn dev_wallet_contract(artifact_path: &Path, artifact_bytes: Vec<u8>) -> Contract {
    let blob = ContractBlob {
        data: BytesM(artifact_bytes),
    };
    match artifact_path
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rv32a") => Contract::kRv32 {
            rv32_contract: blob,
        },
        _ => Contract::kWasmi {
            wasmi_contract: blob,
        },
    }
}

fn contract_hash(contract: &Contract) -> Result<SHA256Hash, CliError> {
    let contract_xdr = xdrs::txs::encode(contract).map_err(|error| {
        CliError::Other(format!("failed to XDR-encode wallet contract: {error}"))
    })?;
    let bytes: [u8; 32] = Sha256::digest(contract_xdr).into();
    Ok(SHA256Hash::from(bytes))
}

fn resolve_dev_wallet_artifact_path(
    cwd: &Path,
    profile_source: Option<&Path>,
    hint: &str,
) -> Result<PathBuf, CliError> {
    let hint_path = Path::new(hint);
    if hint_path.is_absolute() && hint_path.is_file() {
        return Ok(hint_path.to_path_buf());
    }

    let mut candidates = Vec::new();
    if !hint_path.is_absolute() {
        candidates.push(cwd.join(hint_path));
        if let Some(root) = profile_source.and_then(Path::parent) {
            candidates.push(root.join(hint_path));
        }
    }
    candidates.sort();
    candidates.dedup();

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(CliError::Other(format!(
        "dev-wallet artifact `{hint}` was not found; run from the local-node project directory or set profile.<name>.runtime_manifest with a reachable artifact hint"
    )))
}

fn format_contract_address(address: &ContractAddress) -> String {
    format!("0x{}", hex::encode(address.as_ref()))
}

#[cfg(test)]
mod tests {
    use groundhog_xdr::types::{Ed25519PublicKey, SHA256Hash};

    use super::{create_wallet_calldata, resolve_dev_wallet_artifact_path};

    #[test]
    fn create_wallet_calldata_round_trips_expected_tuple() {
        let nonce = SHA256Hash::from([0x11; 32]);
        let contract_hash = SHA256Hash::from([0x22; 32]);
        let owner = Ed25519PublicKey::from([0x33; 32]);

        let encoded = create_wallet_calldata(&nonce, &contract_hash, owner)
            .expect("bug: create_wallet calldata should encode");
        let decoded: (SHA256Hash, SHA256Hash, Ed25519PublicKey) =
            xdrs::cxs::decode(&encoded).expect("bug: create_wallet calldata should decode");

        assert_eq!(decoded, (nonce, contract_hash, owner));
    }

    #[test]
    fn artifact_path_uses_profile_root() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let config = tmp.path().join("groundhog.toml");
        std::fs::write(&config, "").expect("bug: config should be written");
        let contracts = tmp.path().join("contracts");
        std::fs::create_dir(&contracts).expect("bug: contracts dir should be created");
        let artifact = contracts.join("dev_wallet.wasm");
        std::fs::write(&artifact, []).expect("bug: artifact should be written");

        let resolved = resolve_dev_wallet_artifact_path(
            std::path::Path::new("/"),
            Some(&config),
            "contracts/dev_wallet.wasm",
        )
        .expect("bug: profile-root artifact should resolve");
        assert_eq!(resolved, artifact);
    }
}
