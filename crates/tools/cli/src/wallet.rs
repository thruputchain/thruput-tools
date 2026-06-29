use std::path::{Path, PathBuf};

use groundhog_signing::{
    BuildDeployTransaction, BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner,
    SignTransactionParams, build_deploy_transaction, build_dev_wallet_do_action_transaction,
    derive_deploy_address, sign_and_encode_transaction_hex, source_address_auth_claim,
};
use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
use serde_json::json;
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

use crate::account_resolution::{LocalBootstrapRole, resolve_local_bootstrap_account};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{
    ProfileOverrides, find_project_config, remove_profile_wallet_address, resolve_default_key,
    resolve_profile, resolve_profile_with_overrides, save_profile_wallet_address,
};
use crate::hog;
use crate::keys::load_local_signer;
use crate::result::CommandResult;
use crate::runtime_manifest::{
    apply_local_node_state_for_local_bootstrap, load_configured_runtime_manifest,
};
use crate::tx_utils::{
    ensure_tx_success, format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height,
    submit_and_poll_blocking,
};
use crate::{CliError, Print};

pub const WALLET_CREATE_JSON_SCHEMA_VERSION: u32 = 3;
const WALLET_CREATE_PREFUND_RAW_AMOUNT: u128 = 100_000_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct SavedWallet {
    name: String,
    address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedWalletCreate {
    pub(crate) rpc_url: String,
    pub(crate) key_name: String,
    pub(crate) wallet_name: String,
    pub(crate) faucet_key_name: String,
    pub(crate) faucet_role: String,
    pub(crate) owner_public_key: String,
    pub(crate) artifact_path: PathBuf,
    pub(crate) wallet_address: String,
    pub(crate) prefund_raw_amount: u128,
    pub(crate) prefund_tx_hex: String,
    pub(crate) create_tx_hex: String,
}

pub fn create(
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    dry_run: bool,
    profile: &str,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let prepared = prepare_wallet_create(&cwd, key_name, wallet_name, profile, overrides)?;

    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    if prepared.wallet_name == prepared.key_name {
        print.status(format_args!(
            "Creating wallet for key '{}'...",
            prepared.key_name
        ));
    } else {
        print.status(format_args!(
            "Creating wallet '{}' for key '{}'...",
            prepared.wallet_name, prepared.key_name
        ));
    }

    let (prefund_tx_hash, prefund_receipt) =
        submit_and_poll_blocking(&prepared.rpc_url, &prepared.prefund_tx_hex)?;
    let prefund_tx_hash = format_tx_hash(&prefund_tx_hash);
    ensure_tx_success("wallet create prefund", &prefund_receipt, &prefund_tx_hash)?;

    let (create_tx_hash, create_receipt) =
        submit_and_poll_blocking(&prepared.rpc_url, &prepared.create_tx_hex)?;
    let create_tx_hash = format_tx_hash(&create_tx_hash);
    ensure_tx_success("wallet create", &create_receipt, &create_tx_hash)?;

    let project_root = find_project_config(&cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);
    save_profile_wallet_address(
        &project_root,
        Some(profile),
        &prepared.wallet_name,
        &prepared.wallet_address,
    )?;

    print.status(format_args!("Wallet created: {}", prepared.wallet_address));
    print.status(format_args!("Prefund txn: {prefund_tx_hash}"));
    print.status(format_args!("Txn: {create_tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": WALLET_CREATE_JSON_SCHEMA_VERSION,
            "name": prepared.wallet_name,
            "key": prepared.key_name,
            "wallet": prepared.wallet_address,
            "faucet": {
                "key": prepared.faucet_key_name,
                "role": prepared.faucet_role,
            },
            "owner_public_key": prepared.owner_public_key,
            "artifact_path": prepared.artifact_path,
            "prefund_raw_amount": prepared.prefund_raw_amount.to_string(),
            "prefund_tx_hash": prefund_tx_hash,
            "prefund_height": prefund_receipt.height,
            "create_tx_hash": create_tx_hash,
            "create_height": create_receipt.height,
        }),
    ))?;

    Ok(())
}

pub fn remove(name: &str, profile: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let project_root = find_project_config(&cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);
    remove_profile_wallet_address(&project_root, Some(profile), name)?;

    print.status(format_args!("Removed wallet `{}`.", name.trim()));
    print.json(&CommandResult::new(
        cmd,
        json!({
            "name": name.trim(),
            "profile": profile,
        }),
    ))?;

    Ok(())
}

pub fn list(profile: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let profile = resolve_profile(&cwd, Some(profile))?;
    let wallets: Vec<SavedWallet> = profile
        .wallets
        .into_iter()
        .map(|(name, address)| SavedWallet { name, address })
        .collect();

    if print.is_json() {
        print.json(&CommandResult::new(cmd, json!({ "wallets": wallets })))?;
        return Ok(());
    }

    if wallets.is_empty() {
        println!("No wallets saved.");
        return Ok(());
    }

    for wallet in wallets {
        println!("{} {}", wallet.name, wallet.address);
    }

    Ok(())
}

fn print_dry_run(
    prepared: &PreparedWalletCreate,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Prepared dry-run wallet create `{}` for key `{}` on {}.",
        prepared.wallet_name, prepared.key_name, prepared.rpc_url
    ));
    print.status(format_args!("  wallet: {}", prepared.wallet_address));
    print.status(format_args!(
        "  faucet: {} ({})",
        prepared.faucet_key_name, prepared.faucet_role
    ));
    print.status(format_args!("  owner: {}", prepared.owner_public_key));
    print.status(format_args!(
        "  prefund raw amount: {}",
        prepared.prefund_raw_amount
    ));
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": WALLET_CREATE_JSON_SCHEMA_VERSION,
            "dry_run": true,
            "submitted": false,
            "name": prepared.wallet_name,
            "key": prepared.key_name,
            "wallet": prepared.wallet_address,
            "faucet": {
                "key": prepared.faucet_key_name,
                "role": prepared.faucet_role,
            },
            "owner_public_key": prepared.owner_public_key,
            "artifact_path": prepared.artifact_path,
            "prefund_raw_amount": prepared.prefund_raw_amount.to_string(),
        }),
    ))?;

    Ok(())
}

pub(crate) fn prepare_wallet_create(
    cwd: &Path,
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    profile_name: &str,
    overrides: &ProfileOverrides,
) -> Result<PreparedWalletCreate, CliError> {
    let mut profile = resolve_profile_with_overrides(cwd, Some(profile_name), overrides)?;
    apply_local_node_state_for_local_bootstrap(&mut profile)?;
    let key_name = match key_name {
        Some(key_name) => key_name.to_owned(),
        None => resolve_default_key(cwd, Some(profile_name))?.ok_or(CliError::MissingSigningKey)?,
    };
    let wallet_name = match wallet_name {
        Some(wallet_name) => {
            let wallet_name = wallet_name.trim();
            if wallet_name.is_empty() {
                return Err(CliError::EmptyWalletName);
            }
            wallet_name.to_owned()
        }
        None => key_name.clone(),
    };
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let manifest = load_configured_runtime_manifest(&profile, "faucet")?.ok_or_else(|| {
        CliError::Other("wallet creation requires a local runtime manifest".into())
    })?;
    let create_wallet_entrypoint = manifest
        .bootstrap
        .dev_wallet
        .create_wallet_entrypoint_id()
        .map_err(|error| CliError::Other(error.to_string()))?;
    let wallet_do_action_entrypoint = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| CliError::Other(error.to_string()))?;
    let faucet = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)?;
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
        derive_deploy_address(&faucet.wallet_address, &deploy_nonce, &contract_hash)
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
    let prefund_tx_hex = sign_wallet_prefund_transaction(&SignWalletPrefundTransaction {
        faucet_wallet_address: faucet.wallet_address,
        wallet_entrypoint: wallet_do_action_entrypoint,
        recipient: wallet_address,
        raw_amount: WALLET_CREATE_PREFUND_RAW_AMOUNT,
        block_expiry_height,
        nonce: deploy_nonce_val,
        signer: &faucet.signer,
    })?;
    let create_tx_hex = sign_create_wallet_transaction(SignCreateWalletTransaction {
        faucet_wallet_address: faucet.wallet_address,
        create_wallet_entrypoint,
        create_calldata,
        block_expiry_height,
        nonce: deploy_nonce_val.saturating_add(1),
        contract,
        signer: &faucet.signer,
    })?;

    Ok(PreparedWalletCreate {
        rpc_url,
        key_name,
        wallet_name,
        faucet_key_name: faucet.key_name,
        faucet_role: faucet.role,
        owner_public_key,
        artifact_path,
        wallet_address: wallet_address_hex,
        prefund_raw_amount: WALLET_CREATE_PREFUND_RAW_AMOUNT,
        prefund_tx_hex,
        create_tx_hex,
    })
}

struct SignWalletPrefundTransaction<'a> {
    faucet_wallet_address: ContractAddress,
    wallet_entrypoint: ContractEntrypointID,
    recipient: ContractAddress,
    raw_amount: u128,
    block_expiry_height: i64,
    nonce: u64,
    signer: &'a LocalSigner,
}

struct SignCreateWalletTransaction<'a> {
    faucet_wallet_address: ContractAddress,
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
        invoked_address: args.faucet_wallet_address,
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

fn sign_wallet_prefund_transaction(
    args: &SignWalletPrefundTransaction<'_>,
) -> Result<String, CliError> {
    let hog_address = parse_contract_address(hog::TOKEN_ADDRESS)?;
    let transfer_entrypoint = ContractEntrypointID::from(hog::TRANSFER_ENTRYPOINT);
    let transfer_calldata = hog::encode_transfer_calldata(&args.recipient, args.raw_amount)?;
    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: args.faucet_wallet_address,
        wallet_entrypoint: args.wallet_entrypoint,
        target_address: hog_address,
        target_entrypoint: transfer_entrypoint,
        target_calldata: transfer_calldata.as_slice(),
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        target_gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
    })
    .map_err(|error| CliError::Other(error.to_string()))?;
    sign_and_encode_transaction_hex(tx, args.signer, SignTransactionParams::default())
        .map_err(|error| CliError::Other(error.to_string()))
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
