use std::path::{Path, PathBuf};

use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::Signer;
use groundhog_signing::{
    BuildDeployTransaction, BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner,
    SignTransactionParams, build_deploy_transaction, build_dev_wallet_do_action_transaction,
    create_wallet_calldata, derive_deploy_address, dev_wallet_contract,
    sign_and_encode_transaction_hex, source_address_auth_claim, wallet_contract_hash,
    wallet_create_proof_message,
};
use groundhog_xdr::contract::{Contract, ContractEntrypointID};
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use xdr_common::VecM;

use crate::account_resolution::{LocalBootstrapRole, resolve_local_bootstrap_account};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{
    ProfileOverrides, find_project_config, remove_profile_wallet_address, resolve_default_key,
    resolve_profile, resolve_profile_with_overrides, save_profile_wallet_address,
};
use crate::faucet_client::{client as faucet_client, post_json};
use crate::hog;
use crate::keys::load_local_signer;
use crate::profile_backend::{ProfileBackend, resolve_profile_backend};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedWalletCreate {
    Local(PreparedLocalWalletCreate),
    Remote(PreparedRemoteWalletCreate),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct SavedWallet {
    name: String,
    address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLocalWalletCreate {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedRemoteWalletCreate {
    faucet_url: String,
    rpc_url: String,
    key_name: String,
    wallet_name: String,
    owner_public_key: String,
    deploy_nonce: String,
    owner_signature: String,
    wallet_address: String,
    prefund_raw_amount: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRemoteWalletCreateDryRun {
    faucet_url: String,
    rpc_url: String,
    key_name: String,
    wallet_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareWalletCreateRequest {
    owner_public_key: String,
    deploy_nonce: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareWalletCreateResponse {
    wallet_address: String,
    prefund_raw_amount: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateWalletRequest {
    owner_public_key: String,
    deploy_nonce: String,
    owner_signature: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWalletResponse {
    wallet_address: String,
    tx_hash: TxHash,
    height: groundhog_json_rpc_types::BlockHeight,
    status: ExecStatus,
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
    if dry_run
        && let Some(prepared) =
            prepare_remote_wallet_create_dry_run(&cwd, key_name, wallet_name, profile, overrides)?
    {
        return print_remote_dry_run_plan(&prepared, cmd, print);
    }

    let prepared = prepare_wallet_create(&cwd, key_name, wallet_name, profile, overrides)?;

    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    match prepared {
        PreparedWalletCreate::Local(prepared) => {
            create_local_wallet(&cwd, profile, &prepared, cmd, print)
        }
        PreparedWalletCreate::Remote(prepared) => {
            create_remote_wallet(&cwd, profile, &prepared, cmd, print)
        }
    }
}

fn create_local_wallet(
    cwd: &Path,
    profile: &str,
    prepared: &PreparedLocalWalletCreate,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
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

    let project_root = find_project_config(cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf());
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

fn create_remote_wallet(
    cwd: &Path,
    profile: &str,
    prepared: &PreparedRemoteWalletCreate,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if prepared.wallet_name == prepared.key_name {
        print.status(format_args!(
            "Creating remote wallet for key '{}'...",
            prepared.key_name
        ));
    } else {
        print.status(format_args!(
            "Creating remote wallet '{}' for key '{}'...",
            prepared.wallet_name, prepared.key_name
        ));
    }

    let client = faucet_client()?;
    let airdrop: serde_json::Value = post_json(
        &client,
        &format!("{}/v1/airdrop", prepared.faucet_url),
        &json!({ "recipient": prepared.wallet_address }),
    )?;
    let create: CreateWalletResponse = post_json(
        &client,
        &format!("{}/v1/create-wallet", prepared.faucet_url),
        &CreateWalletRequest {
            owner_public_key: prepared.owner_public_key.clone(),
            deploy_nonce: prepared.deploy_nonce.clone(),
            owner_signature: prepared.owner_signature.clone(),
        },
    )?;
    if !matches!(create.status, ExecStatus::Ok) {
        return Err(CliError::Other(
            "remote wallet-create transaction did not commit successfully".into(),
        ));
    }
    validate_remote_create_wallet_address(&prepared.wallet_address, &create.wallet_address)?;

    let project_root = find_project_config(cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf());
    save_profile_wallet_address(
        &project_root,
        Some(profile),
        &prepared.wallet_name,
        &prepared.wallet_address,
    )?;

    let create_tx_hash = format_tx_hash(&create.tx_hash);
    print.status(format_args!("Wallet created: {}", prepared.wallet_address));
    if let Some(tx_hash) = airdrop.get("txHash").and_then(serde_json::Value::as_str) {
        print.status(format_args!("Prefund txn: {tx_hash}"));
    }
    print.status(format_args!("Txn: {create_tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": WALLET_CREATE_JSON_SCHEMA_VERSION,
            "name": prepared.wallet_name,
            "key": prepared.key_name,
            "wallet": prepared.wallet_address,
            "mode": "remote",
            "faucet_url": prepared.faucet_url,
            "rpc_url": prepared.rpc_url,
            "owner_public_key": prepared.owner_public_key,
            "deploy_nonce": prepared.deploy_nonce,
            "prefund_raw_amount": prepared.prefund_raw_amount,
            "airdrop": airdrop,
            "create_tx_hash": create_tx_hash,
            "create_height": create.height,
        }),
    ))?;

    Ok(())
}

fn validate_remote_create_wallet_address(prepared: &str, created: &str) -> Result<(), CliError> {
    let prepared_wallet_address = parse_contract_address(prepared)?;
    let created_wallet_address = parse_contract_address(created)?;
    if created_wallet_address != prepared_wallet_address {
        return Err(CliError::Other(format!(
            "remote wallet-create returned wallet `{created}` but prepared `{prepared}`"
        )));
    }

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
    match prepared {
        PreparedWalletCreate::Local(prepared) => {
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
                    "mode": "local",
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
        }
        PreparedWalletCreate::Remote(prepared) => {
            print.status(format_args!(
                "Prepared dry-run remote wallet create `{}` for key `{}`.",
                prepared.wallet_name, prepared.key_name
            ));
            print.status(format_args!("  faucet: {}", prepared.faucet_url));
            print.status(format_args!("  wallet: {}", prepared.wallet_address));
            print.status(format_args!("  owner: {}", prepared.owner_public_key));
            print.status(format_args!("  deploy nonce: {}", prepared.deploy_nonce));
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
                    "mode": "remote",
                    "name": prepared.wallet_name,
                    "key": prepared.key_name,
                    "wallet": prepared.wallet_address,
                    "faucet_url": prepared.faucet_url,
                    "rpc_url": prepared.rpc_url,
                    "owner_public_key": prepared.owner_public_key,
                    "deploy_nonce": prepared.deploy_nonce,
                    "prefund_raw_amount": prepared.prefund_raw_amount,
                    "planned_requests": [
                        "POST /v1/airdrop",
                        "POST /v1/create-wallet"
                    ],
                }),
            ))?;
        }
    }

    Ok(())
}

fn print_remote_dry_run_plan(
    prepared: &PreparedRemoteWalletCreateDryRun,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Prepared dry-run remote wallet create `{}` for key `{}`.",
        prepared.wallet_name, prepared.key_name
    ));
    print.status(format_args!("  faucet: {}", prepared.faucet_url));
    print.status("  wallet: requires faucet prepare request");
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": WALLET_CREATE_JSON_SCHEMA_VERSION,
            "dry_run": true,
            "submitted": false,
            "mode": "remote",
            "name": prepared.wallet_name,
            "key": prepared.key_name,
            "wallet": null,
            "faucet_url": prepared.faucet_url,
            "rpc_url": prepared.rpc_url,
            "planned_requests": [
                "GET /v1/network",
                "POST /v1/create-wallet/prepare",
                "POST /v1/airdrop",
                "POST /v1/create-wallet"
            ],
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
    let inputs = resolve_wallet_create_inputs(cwd, key_name, wallet_name, profile_name, overrides)?;
    let profile = inputs.profile;
    let key_name = inputs.key_name;
    let wallet_name = inputs.wallet_name;

    match resolve_profile_backend(&profile)? {
        ProfileBackend::RemoteFaucet => {
            prepare_remote_wallet_create(&profile, key_name, wallet_name)
        }
        ProfileBackend::LocalManifest | ProfileBackend::RpcOnly => {
            prepare_local_wallet_create(cwd, profile, key_name, wallet_name)
        }
    }
}

fn prepare_remote_wallet_create_dry_run(
    cwd: &Path,
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    profile_name: &str,
    overrides: &ProfileOverrides,
) -> Result<Option<PreparedRemoteWalletCreateDryRun>, CliError> {
    let inputs = resolve_wallet_create_inputs(cwd, key_name, wallet_name, profile_name, overrides)?;
    if !matches!(
        resolve_profile_backend(&inputs.profile)?,
        ProfileBackend::RemoteFaucet
    ) {
        return Ok(None);
    }
    let faucet_url = inputs
        .profile
        .faucet_url
        .as_deref()
        .expect("checked above")
        .trim_end_matches('/')
        .to_owned();
    let rpc_url = inputs
        .profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();

    Ok(Some(PreparedRemoteWalletCreateDryRun {
        faucet_url,
        rpc_url,
        key_name: inputs.key_name,
        wallet_name: inputs.wallet_name,
    }))
}

struct WalletCreateInputs {
    profile: crate::config::ResolvedProfile,
    key_name: String,
    wallet_name: String,
}

fn resolve_wallet_create_inputs(
    cwd: &Path,
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    profile_name: &str,
    overrides: &ProfileOverrides,
) -> Result<WalletCreateInputs, CliError> {
    let profile = resolve_profile_with_overrides(cwd, Some(profile_name), overrides)?;
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

    Ok(WalletCreateInputs {
        profile,
        key_name,
        wallet_name,
    })
}

fn prepare_local_wallet_create(
    cwd: &Path,
    mut profile: crate::config::ResolvedProfile,
    key_name: String,
    wallet_name: String,
) -> Result<PreparedWalletCreate, CliError> {
    apply_local_node_state_for_local_bootstrap(&mut profile)?;
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
    )
    .map_err(|error| {
        CliError::Other(format!("failed to encode wallet create calldata: {error}"))
    })?;

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

    Ok(PreparedWalletCreate::Local(PreparedLocalWalletCreate {
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
    }))
}

fn prepare_remote_wallet_create(
    profile: &crate::config::ResolvedProfile,
    key_name: String,
    wallet_name: String,
) -> Result<PreparedWalletCreate, CliError> {
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| CliError::Other("remote wallet creation requires faucet_url".into()))?
        .trim_end_matches('/')
        .to_owned();
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let owner = load_local_signer(&key_name)?;
    let owner_public_key_xdr = Ed25519PublicKey::from(owner.verifying_key_bytes());
    let owner_public_key = format!("0x{}", hex::encode(owner_public_key_xdr.as_ref()));
    let deploy_nonce_val = nonce_from_timestamp();
    let deploy_nonce = deploy_nonce_from_u64(deploy_nonce_val);
    let deploy_nonce_hex = format!("0x{}", hex::encode(deploy_nonce.as_ref()));

    let client = faucet_client()?;
    let descriptor = fetch_network_descriptor(&faucet_url)?;
    let prepared: PrepareWalletCreateResponse = post_json(
        &client,
        &format!("{faucet_url}/v1/create-wallet/prepare"),
        &PrepareWalletCreateRequest {
            owner_public_key: owner_public_key.clone(),
            deploy_nonce: deploy_nonce_hex.clone(),
        },
    )?;
    let proof_message = wallet_create_proof_message(
        descriptor.genesis_hash.0,
        descriptor.faucet.wallet_address.0,
        descriptor.wallet_create.wallet_contract_hash.0,
        owner_public_key_xdr,
        deploy_nonce,
    )
    .map_err(|error| CliError::Other(error.to_string()))?;
    let owner_signature = match owner
        .sign(&proof_message)
        .map_err(|error| CliError::Other(error.to_string()))?
    {
        groundhog_xdr::types::Signature::Ed25519 { ed25519 } => {
            format!("0x{}", hex::encode(ed25519.as_ref()))
        }
        _ => {
            return Err(CliError::Other(
                "local signer produced unsupported signature".into(),
            ));
        }
    };

    Ok(PreparedWalletCreate::Remote(PreparedRemoteWalletCreate {
        faucet_url,
        rpc_url,
        key_name,
        wallet_name,
        owner_public_key,
        deploy_nonce: deploy_nonce_hex,
        owner_signature,
        wallet_address: prepared.wallet_address,
        prefund_raw_amount: prepared.prefund_raw_amount,
    }))
}

fn fetch_network_descriptor(faucet_url: &str) -> Result<NetworkDescriptor, CliError> {
    let faucet_url = faucet_url.trim_end_matches('/');
    let url = format!("{faucet_url}/v1/network");
    faucet_client()?
        .get(&url)
        .send()
        .map_err(|error| CliError::Other(format!("failed to call `{url}`: {error}")))?
        .error_for_status()
        .map_err(|error| CliError::Other(format!("faucet request `{url}` failed: {error}")))?
        .json()
        .map_err(|error| CliError::Other(format!("invalid JSON from `{url}`: {error}")))
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
    let contract_hash = wallet_contract_hash(&contract)
        .map_err(|error| CliError::Other(format!("failed to hash wallet contract: {error}")))?;
    Ok((artifact_path, contract, contract_hash))
}

fn deploy_nonce_from_u64(nonce: u64) -> SHA256Hash {
    let bytes: [u8; 32] = Sha256::digest(nonce.to_le_bytes()).into();
    SHA256Hash::from(bytes)
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
    use super::{resolve_dev_wallet_artifact_path, validate_remote_create_wallet_address};

    #[test]
    fn remote_create_wallet_address_validation_accepts_same_address() {
        let prepared = format!("0x{}", "aa".repeat(32));
        let created = format!("0x{}", "AA".repeat(32));

        validate_remote_create_wallet_address(&prepared, &created)
            .expect("bug: same address should validate regardless of hex casing");
    }

    #[test]
    fn remote_create_wallet_address_validation_rejects_mismatch() {
        let prepared = format!("0x{}", "aa".repeat(32));
        let created = format!("0x{}", "bb".repeat(32));

        let err = validate_remote_create_wallet_address(&prepared, &created)
            .expect_err("bug: mismatched address should fail");

        assert!(err.to_string().contains("returned wallet"));
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
