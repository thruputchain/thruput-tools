use std::path::{Path, PathBuf};

use getrandom::getrandom;
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_signing::Signer;
use groundhog_signing::{
    BuildDeployTransaction, BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, LocalSigner,
    SignTransactionParams, WalletCreateProofV2, build_deploy_transaction,
    build_dev_wallet_do_action_transaction, create_wallet_calldata, derive_deploy_address,
    dev_wallet_contract, sign_and_encode_transaction_hex, source_address_auth_claim,
    wallet_contract_hash, wallet_create_proof_message_v2,
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
    Network, ProfileOverrides, ResolvedProfile, cache_dir, find_project_config,
    remove_profile_wallet_address, resolve_default_key_for_profile, resolve_profile,
    resolve_profile_with_overrides, save_profile_wallet_address,
};
use crate::faucet_client::{client as faucet_client, post_json};
use crate::hog;
use crate::keys::load_local_signer;
use crate::profile_backend::{ProfileBackend, resolve_profile_backend};
use crate::result::CommandResult;
use crate::runtime_manifest::{configured_runtime_manifest_path, load_configured_runtime_manifest};
use crate::tx_utils::{
    ensure_tx_success, format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height,
    submit_and_poll_blocking,
};
use crate::{CliError, Print, dev_keys};

pub const WALLET_CREATE_JSON_SCHEMA_VERSION: u32 = 3;
const WALLET_CREATE_PREFUND_RAW_AMOUNT: u128 = 200_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedWalletCreate {
    Local(PreparedLocalWalletCreate),
    Remote(PreparedRemoteWalletCreate),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct SavedWallet {
    name: String,
    address: String,
    owner_public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLocalWalletCreate {
    pub(crate) rpc_url: String,
    pub(crate) profile_name: String,
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
    profile_name: String,
    key_name: String,
    wallet_name: String,
    owner_public_key: String,
    deploy_nonce: String,
    client_nonce: String,
    owner_signature: String,
    network_id: String,
    genesis_hash: String,
    wallet_contract_hash: String,
    wallet_address: String,
    prefund_raw_amount: String,
    attempt_path: PathBuf,
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
struct CreateWalletRequest {
    owner_public_key: String,
    deploy_nonce: String,
    client_nonce: String,
    owner_signature: String,
    network_id: String,
    genesis_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWalletResponse {
    attempt_status: String,
    wallet_address: String,
    prefund_tx_hash: Option<String>,
    tx_hash: Option<String>,
    height: Option<groundhog_json_rpc_types::BlockHeight>,
    status: Option<ExecStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteWalletCreateAttempt {
    profile: String,
    key_name: String,
    wallet_name: String,
    network_id: String,
    genesis_hash: String,
    owner_public_key: String,
    deploy_nonce: String,
    client_nonce: String,
    wallet_contract_hash: String,
    wallet_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedRemoteWalletCreateAttempt {
    attempt: RemoteWalletCreateAttempt,
    path: PathBuf,
}

pub fn create(
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    dry_run: bool,
    profile: Option<&str>,
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
        return match &prepared {
            PreparedWalletCreate::Local(prepared) => print_dry_run(prepared, cmd, print),
            PreparedWalletCreate::Remote(_) => Err(CliError::Other(
                "remote wallet create dry-run should be handled before prepare".into(),
            )),
        };
    }

    match prepared {
        PreparedWalletCreate::Local(prepared) => create_local_wallet(&cwd, &prepared, cmd, print),
        PreparedWalletCreate::Remote(prepared) => create_remote_wallet(&cwd, &prepared, cmd, print),
    }
}

fn create_local_wallet(
    cwd: &Path,
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
        Some(&prepared.profile_name),
        &prepared.wallet_name,
        &prepared.wallet_address,
        &prepared.owner_public_key,
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
    let create: CreateWalletResponse = post_json(
        &client,
        &format!("{}/v1/create-wallet", prepared.faucet_url),
        &CreateWalletRequest {
            owner_public_key: prepared.owner_public_key.clone(),
            deploy_nonce: prepared.deploy_nonce.clone(),
            client_nonce: prepared.client_nonce.clone(),
            owner_signature: prepared.owner_signature.clone(),
            network_id: prepared.network_id.clone(),
            genesis_hash: prepared.genesis_hash.clone(),
        },
    )?;
    if create.attempt_status != "created" {
        return Err(CliError::Other(format!(
            "remote wallet-create is pending ({}) - retry the same command to continue",
            create.attempt_status
        )));
    }
    let create_status = create.status.as_ref().ok_or_else(|| {
        CliError::Other("remote wallet-create response omitted final transaction status".into())
    })?;
    if !matches!(create_status, ExecStatus::Ok) {
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
        Some(&prepared.profile_name),
        &prepared.wallet_name,
        &prepared.wallet_address,
        &prepared.owner_public_key,
    )?;
    remove_remote_wallet_create_attempt(prepared)?;

    let create_tx_hash = create
        .tx_hash
        .clone()
        .ok_or_else(|| CliError::Other("remote wallet-create response omitted txHash".into()))?;
    let prefund_tx_hash = create.prefund_tx_hash.clone().ok_or_else(|| {
        CliError::Other("remote wallet-create response omitted prefundTxHash".into())
    })?;
    let create_height = create.height.ok_or_else(|| {
        CliError::Other("remote wallet-create response omitted create height".into())
    })?;
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
            "mode": "remote",
            "faucet_url": prepared.faucet_url,
            "rpc_url": prepared.rpc_url,
            "owner_public_key": prepared.owner_public_key,
            "deploy_nonce": prepared.deploy_nonce,
            "client_nonce": prepared.client_nonce,
            "prefund_raw_amount": prepared.prefund_raw_amount,
            "prefund_tx_hash": prefund_tx_hash,
            "create_tx_hash": create_tx_hash,
            "create_height": create_height,
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

pub fn remove(
    name: &str,
    profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let project_root = find_project_config(&cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);
    let resolved_profile = resolve_profile(&project_root, profile)?;
    remove_profile_wallet_address(&project_root, Some(&resolved_profile.name), name)?;

    print.status(format_args!("Removed wallet `{}`.", name.trim()));
    print.json(&CommandResult::new(
        cmd,
        json!({
            "name": name.trim(),
            "profile": resolved_profile.name,
        }),
    ))?;

    Ok(())
}

pub fn list(profile: Option<&str>, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let profile = resolve_profile(&cwd, profile)?;
    let wallets: Vec<SavedWallet> = profile
        .wallets
        .into_iter()
        .map(|(name, wallet)| SavedWallet {
            name,
            address: wallet.address,
            owner_public_key: wallet.owner_public_key,
        })
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
        println!(
            "{} {} owner {}",
            wallet.name, wallet.address, wallet.owner_public_key
        );
    }

    Ok(())
}

fn print_dry_run(
    prepared: &PreparedLocalWalletCreate,
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
    print.status("  wallet: derived locally after GET /v1/network");
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
                "POST /v1/create-wallet"
            ],
            "idempotency_key": "profile + key + wallet + networkId + genesisHash + clientNonce",
        }),
    ))?;

    Ok(())
}

pub(crate) fn prepare_wallet_create(
    cwd: &Path,
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<PreparedWalletCreate, CliError> {
    let inputs = resolve_wallet_create_inputs(cwd, key_name, wallet_name, profile_name, overrides)?;
    let profile = inputs.profile;
    let key_name = inputs.key_name;
    let wallet_name = inputs.wallet_name;

    match resolve_wallet_create_backend(&profile)? {
        ProfileBackend::RemoteFaucet => {
            prepare_remote_wallet_create(&profile, key_name, wallet_name)
        }
        ProfileBackend::LocalManifest => {
            prepare_local_wallet_create(cwd, profile, key_name, wallet_name)
        }
        ProfileBackend::RpcOnly => Err(CliError::Other(format!(
            "profile `{}` is RPC-only; wallet create requires either `runtime_manifest` or `faucet_url`",
            profile.name
        ))),
    }
}

fn resolve_wallet_create_backend(profile: &ResolvedProfile) -> Result<ProfileBackend, CliError> {
    match profile.network {
        Some(Network::Local) => {
            if profile.rpc_url.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets local, but wallet create requires `rpc_url`; run `groundhog node` or configure `rpc_url`",
                ));
            }
            if configured_runtime_manifest_path(profile)?.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets local, but wallet create requires a local runtime manifest; run `groundhog node` or configure `runtime_manifest`",
                ));
            }
            Ok(ProfileBackend::LocalManifest)
        }
        Some(Network::Devnet) => {
            if configured_runtime_manifest_path(profile)?.is_some() {
                return Err(profile_config_error(
                    profile,
                    "targets devnet, but `--runtime-manifest` requires a local profile",
                ));
            }
            if profile.rpc_url.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets devnet, but wallet create requires `rpc_url`",
                ));
            }
            if profile.faucet_url.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets devnet, but wallet create requires `faucet_url`",
                ));
            }
            Ok(ProfileBackend::RemoteFaucet)
        }
        Some(Network::Mainnet) => Err(profile_config_error(
            profile,
            "targets mainnet, but wallet create is not supported because mainnet does not exist yet",
        )),
        None => resolve_profile_backend(profile),
    }
}

fn profile_config_error(profile: &ResolvedProfile, message: &'static str) -> CliError {
    CliError::ProfileConfigInvalid {
        profile: profile.name.clone(),
        message: message.to_string(),
    }
}

fn prepare_remote_wallet_create_dry_run(
    cwd: &Path,
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<Option<PreparedRemoteWalletCreateDryRun>, CliError> {
    let inputs = resolve_wallet_create_inputs(cwd, key_name, wallet_name, profile_name, overrides)?;
    if !matches!(
        resolve_wallet_create_backend(&inputs.profile)?,
        ProfileBackend::RemoteFaucet
    ) {
        return Ok(None);
    }
    let faucet_url = inputs
        .profile
        .faucet_url
        .as_deref()
        .expect("bug: remote faucet backend must have faucet_url")
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
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<WalletCreateInputs, CliError> {
    let profile = resolve_profile_with_overrides(cwd, profile_name, overrides)?;
    let key_name = match key_name {
        Some(key_name) => key_name.to_owned(),
        None => resolve_default_key_for_profile(&profile)?.ok_or(CliError::MissingSigningKey)?,
    };
    if dev_keys::is_dev_key(&key_name) {
        return Err(CliError::Other(
            "wallet creation requires a non-dev key; run `groundhog keys generate <name>` \
             and retry with `--key <name>`"
                .into(),
        ));
    }
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
    profile: crate::config::ResolvedProfile,
    key_name: String,
    wallet_name: String,
) -> Result<PreparedWalletCreate, CliError> {
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let manifest_path = configured_runtime_manifest_path(&profile)?.ok_or_else(|| {
        CliError::Other("wallet creation requires a local runtime manifest".into())
    })?;
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
        &manifest_path,
        profile.runtime_artifacts_dir.as_deref(),
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
        profile_name: profile.name,
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

    let client = faucet_client()?;
    let descriptor = fetch_network_descriptor(&client, &faucet_url)?;
    let loaded_attempt = load_or_create_remote_wallet_create_attempt(
        profile,
        &key_name,
        &wallet_name,
        &owner_public_key,
        &descriptor,
    )?;
    let attempt = loaded_attempt.attempt;
    let deploy_nonce = parse_sha256_hash(&attempt.deploy_nonce)?;
    let client_nonce = parse_hex_bytes(&attempt.client_nonce)?;
    let wallet_address = parse_contract_address(&attempt.wallet_address)?;
    let proof_message = wallet_create_proof_message_v2(&WalletCreateProofV2 {
        network_id: &attempt.network_id,
        genesis_hash: descriptor.genesis_hash.0,
        faucet_wallet_address: descriptor.faucet.wallet_address.0,
        wallet_contract_hash: descriptor.wallet_create.wallet_contract_hash.0,
        owner_public_key: owner_public_key_xdr,
        deploy_nonce,
        wallet_address,
        client_nonce: &client_nonce,
    })
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
        profile_name: profile.name.clone(),
        key_name,
        wallet_name,
        owner_public_key,
        deploy_nonce: attempt.deploy_nonce,
        client_nonce: attempt.client_nonce,
        owner_signature,
        network_id: attempt.network_id,
        genesis_hash: attempt.genesis_hash,
        wallet_contract_hash: attempt.wallet_contract_hash,
        wallet_address: attempt.wallet_address,
        prefund_raw_amount: descriptor.wallet_create.prefund_raw_amount,
        attempt_path: loaded_attempt.path,
    }))
}

fn load_or_create_remote_wallet_create_attempt(
    profile: &crate::config::ResolvedProfile,
    key_name: &str,
    wallet_name: &str,
    owner_public_key: &str,
    descriptor: &NetworkDescriptor,
) -> Result<LoadedRemoteWalletCreateAttempt, CliError> {
    let network_id = descriptor.network_id.clone();
    let genesis_hash = format_sha256_hash(&descriptor.genesis_hash.0);
    let wallet_contract_hash = format_sha256_hash(&descriptor.wallet_create.wallet_contract_hash.0);
    let path = remote_wallet_create_attempt_path(
        &profile.name,
        key_name,
        wallet_name,
        &network_id,
        &genesis_hash,
    )?;
    if let Some(attempt) = read_remote_wallet_create_attempt(&path)?
        && attempt.profile == profile.name
        && attempt.key_name == key_name
        && attempt.wallet_name == wallet_name
        && attempt.network_id == network_id
        && attempt.genesis_hash == genesis_hash
        && attempt.owner_public_key == owner_public_key
        && attempt.wallet_contract_hash == wallet_contract_hash
    {
        return Ok(LoadedRemoteWalletCreateAttempt { attempt, path });
    }

    let mut deploy_nonce_bytes = [0u8; 32];
    getrandom(&mut deploy_nonce_bytes).map_err(|error| CliError::Other(error.to_string()))?;
    let deploy_nonce = SHA256Hash::from(deploy_nonce_bytes);
    let mut client_nonce = [0u8; 16];
    getrandom(&mut client_nonce).map_err(|error| CliError::Other(error.to_string()))?;
    let wallet_address = derive_deploy_address(
        &descriptor.faucet.wallet_address.0,
        &deploy_nonce,
        &descriptor.wallet_create.wallet_contract_hash.0,
    )
    .map_err(|error| CliError::Other(error.to_string()))?;
    let attempt = RemoteWalletCreateAttempt {
        profile: profile.name.clone(),
        key_name: key_name.to_owned(),
        wallet_name: wallet_name.to_owned(),
        network_id,
        genesis_hash,
        owner_public_key: owner_public_key.to_owned(),
        deploy_nonce: format_sha256_hash(&deploy_nonce),
        client_nonce: format!("0x{}", hex::encode(client_nonce)),
        wallet_contract_hash,
        wallet_address: format_contract_address(&wallet_address),
    };
    write_remote_wallet_create_attempt(&path, &attempt)?;
    Ok(LoadedRemoteWalletCreateAttempt { attempt, path })
}

fn read_remote_wallet_create_attempt(
    path: &Path,
) -> Result<Option<RemoteWalletCreateAttempt>, CliError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| CliError::Other(format!("failed to parse `{}`: {error}", path.display())))
}

fn write_remote_wallet_create_attempt(
    path: &Path,
    attempt: &RemoteWalletCreateAttempt,
) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents =
        serde_json::to_vec_pretty(attempt).map_err(|error| CliError::Other(error.to_string()))?;
    std::fs::write(path, contents)?;
    Ok(())
}

fn remove_remote_wallet_create_attempt(
    prepared: &PreparedRemoteWalletCreate,
) -> Result<(), CliError> {
    match std::fs::remove_file(&prepared.attempt_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remote_wallet_create_attempt_path(
    profile: &str,
    key_name: &str,
    wallet_name: &str,
    network_id: &str,
    genesis_hash: &str,
) -> Result<PathBuf, CliError> {
    let hash: [u8; 32] = Sha256::digest(format!(
        "{profile}\0{key_name}\0{wallet_name}\0{network_id}\0{genesis_hash}"
    ))
    .into();
    Ok(cache_dir()?
        .join("wallet-create-attempts")
        .join(format!("{}.json", hex::encode(hash))))
}

fn format_sha256_hash(hash: &SHA256Hash) -> String {
    format!("0x{}", hex::encode(hash.as_ref()))
}

fn parse_sha256_hash(value: &str) -> Result<SHA256Hash, CliError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes = hex::decode(value).map_err(|error| CliError::Other(error.to_string()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CliError::Other("expected 32-byte SHA256 hash".into()))?;
    Ok(SHA256Hash::from(bytes))
}

fn parse_hex_bytes(value: &str) -> Result<Vec<u8>, CliError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|error| CliError::Other(error.to_string()))
}

fn fetch_network_descriptor(
    client: &reqwest::blocking::Client,
    faucet_url: &str,
) -> Result<NetworkDescriptor, CliError> {
    let faucet_url = faucet_url.trim_end_matches('/');
    let url = format!("{faucet_url}/v1/network");
    client
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
    manifest_path: &Path,
    runtime_artifacts_dir: Option<&Path>,
    profile_source: Option<&Path>,
    artifact_hint: &str,
) -> Result<(PathBuf, Contract, SHA256Hash), CliError> {
    let artifact_path = resolve_dev_wallet_artifact_path(
        cwd,
        manifest_path,
        runtime_artifacts_dir,
        profile_source,
        artifact_hint,
    )?;
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
    let transfer_entrypoint = hog::transfer_entrypoint();
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
    manifest_path: &Path,
    runtime_artifacts_dir: Option<&Path>,
    profile_source: Option<&Path>,
    hint: &str,
) -> Result<PathBuf, CliError> {
    let hint_path = Path::new(hint);
    if hint_path.is_absolute() && hint_path.is_file() {
        return Ok(hint_path.to_path_buf());
    }

    let mut candidates = Vec::new();
    if !hint_path.is_absolute() {
        if let Some(root) = manifest_path.parent() {
            candidates.push(root.join(hint_path));
        }
        candidates.push(cwd.join(hint_path));
        if let Some(root) = profile_source.and_then(Path::parent) {
            candidates.push(root.join(hint_path));
        }
        if let Some(root) = runtime_artifacts_dir {
            candidates.push(root.join(hint_path));
            if let Some(filename) = hint_path.file_name() {
                candidates.push(root.join(filename));
            }
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
        "dev-wallet artifact `{hint}` was not found relative to the runtime manifest, current directory, profile config, or configured runtime_artifacts_dir"
    )))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use serial_test::serial;

    use super::{
        PreparedRemoteWalletCreate, RemoteWalletCreateAttempt,
        load_or_create_remote_wallet_create_attempt, read_remote_wallet_create_attempt,
        remote_wallet_create_attempt_path, remove_remote_wallet_create_attempt,
        resolve_dev_wallet_artifact_path, resolve_wallet_create_backend,
        resolve_wallet_create_inputs, validate_remote_create_wallet_address,
        write_remote_wallet_create_attempt,
    };
    use crate::config::{LocalRuntimeState, Network, ProfileOverrides, ResolvedProfile};

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn set_str(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                unsafe { std::env::set_var(self.key, previous) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    #[test]
    #[serial]
    fn wallet_create_rejects_implicit_local_dev_key() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _config_home = EnvGuard::set_path("XDG_CONFIG_HOME", tmp.path());
        let runtime_manifest = tmp.path().join("runtime-manifest.json");
        std::fs::write(&runtime_manifest, "{}").expect("bug: manifest should be written");
        std::fs::write(
            tmp.path().join("groundhog.toml"),
            "default_profile = \"local\"\n[profile.local]\nnetwork = \"local\"\n",
        )
        .expect("bug: project config should be written");
        crate::config::save_local_runtime_state(&LocalRuntimeState {
            node_id: "local-node".into(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest,
            runtime_artifacts_dir: None,
        })
        .expect("bug: local runtime state should be written");

        let error = resolve_wallet_create_inputs(
            tmp.path(),
            None,
            None,
            Some("local"),
            &ProfileOverrides::default(),
        )
        .err()
        .expect("bug: wallet create should reject implicit dev-0");

        assert!(error.to_string().contains("requires a non-dev key"));
    }

    #[test]
    #[serial]
    fn wallet_create_rejects_explicit_dev_key() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _config_home = EnvGuard::set_path("XDG_CONFIG_HOME", tmp.path());
        std::fs::write(
            tmp.path().join("groundhog.toml"),
            "[profile.local]\nnetwork = \"local\"\n",
        )
        .expect("bug: project config should be written");

        let error = resolve_wallet_create_inputs(
            tmp.path(),
            Some("dev-1"),
            None,
            Some("local"),
            &ProfileOverrides::default(),
        )
        .err()
        .expect("bug: wallet create should reject explicit dev keys");

        assert!(error.to_string().contains("requires a non-dev key"));
    }

    #[test]
    #[serial]
    fn wallet_create_devnet_missing_faucet_reports_devnet_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile();
        profile.faucet_url = None;

        let error = resolve_wallet_create_backend(&profile)
            .expect_err("bug: missing devnet faucet should fail");

        assert_eq!(
            error.to_string(),
            "profile `preview` targets devnet, but wallet create requires `faucet_url`"
        );
    }

    #[test]
    #[serial]
    fn wallet_create_devnet_missing_rpc_reports_devnet_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile();
        profile.rpc_url = None;

        let error = resolve_wallet_create_backend(&profile)
            .expect_err("bug: missing devnet rpc should fail");

        assert_eq!(
            error.to_string(),
            "profile `preview` targets devnet, but wallet create requires `rpc_url`"
        );
    }

    #[test]
    #[serial]
    fn wallet_create_devnet_with_urls_uses_remote_faucet() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let profile = test_profile();

        let backend = resolve_wallet_create_backend(&profile)
            .expect("bug: configured devnet profile should resolve");

        assert_eq!(
            backend,
            crate::profile_backend::ProfileBackend::RemoteFaucet
        );
    }

    #[test]
    #[serial]
    fn wallet_create_devnet_runtime_manifest_reports_conflict() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "/tmp/runtime.json");
        let profile = test_profile();

        let error = resolve_wallet_create_backend(&profile)
            .expect_err("bug: devnet runtime manifest should fail");

        assert_eq!(
            error.to_string(),
            "profile `preview` targets devnet, but `--runtime-manifest` requires a local profile"
        );
    }

    #[test]
    #[serial]
    fn wallet_create_local_missing_manifest_reports_local_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile();
        profile.network = Some(Network::Local);
        profile.faucet_url = None;

        let error = resolve_wallet_create_backend(&profile)
            .expect_err("bug: missing local manifest should fail");

        assert_eq!(
            error.to_string(),
            "profile `preview` targets local, but wallet create requires a local runtime manifest; run `groundhog node` or configure `runtime_manifest`"
        );
    }

    #[test]
    #[serial]
    fn wallet_create_rpc_only_profile_stays_rpc_only() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile();
        profile.network = None;
        profile.faucet_url = None;

        let backend = resolve_wallet_create_backend(&profile)
            .expect("bug: legacy rpc-only backend should resolve");

        assert_eq!(backend, crate::profile_backend::ProfileBackend::RpcOnly);
    }

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
            std::path::Path::new("/tmp/runtime-manifest.json"),
            None,
            Some(&config),
            "contracts/dev_wallet.wasm",
        )
        .expect("bug: profile-root artifact should resolve");
        assert_eq!(resolved, artifact);
    }

    #[test]
    fn artifact_path_prefers_manifest_root() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let manifest_dir = tmp.path().join("node-state");
        std::fs::create_dir(&manifest_dir).expect("bug: manifest dir should be created");
        let manifest = manifest_dir.join("runtime-manifest.json");
        let contracts = manifest_dir.join("contracts");
        std::fs::create_dir(&contracts).expect("bug: contracts dir should be created");
        let artifact = contracts.join("dev_wallet.rv32a");
        std::fs::write(&artifact, []).expect("bug: artifact should be written");

        let resolved = resolve_dev_wallet_artifact_path(
            std::path::Path::new("/"),
            &manifest,
            None,
            None,
            "contracts/dev_wallet.rv32a",
        )
        .expect("bug: manifest-root artifact should resolve");
        assert_eq!(resolved, artifact);
    }

    #[test]
    fn artifact_path_uses_runtime_artifacts_dir() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let artifacts_root = tmp.path().join("hog-devnode");
        let contracts = artifacts_root.join("contracts");
        std::fs::create_dir_all(&contracts).expect("bug: contracts dir should be created");
        let artifact = contracts.join("dev_wallet.rv32a");
        std::fs::write(&artifact, []).expect("bug: artifact should be written");

        let resolved = resolve_dev_wallet_artifact_path(
            std::path::Path::new("/"),
            std::path::Path::new("/tmp/runtime-manifest.json"),
            Some(&artifacts_root),
            None,
            "contracts/dev_wallet.rv32a",
        )
        .expect("bug: runtime artifact-dir artifact should resolve");
        assert_eq!(resolved, artifact);
    }

    #[test]
    fn artifact_path_uses_direct_contract_artifact_dir() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let contracts = tmp.path().join("built_contracts");
        std::fs::create_dir(&contracts).expect("bug: contracts dir should be created");
        let artifact = contracts.join("dev_wallet.rv32a");
        std::fs::write(&artifact, []).expect("bug: artifact should be written");

        let resolved = resolve_dev_wallet_artifact_path(
            std::path::Path::new("/"),
            std::path::Path::new("/tmp/runtime-manifest.json"),
            Some(&contracts),
            None,
            "contracts/dev_wallet.rv32a",
        )
        .expect("bug: direct runtime artifact-dir artifact should resolve");
        assert_eq!(resolved, artifact);
    }

    #[test]
    #[serial]
    fn remote_wallet_create_attempt_is_created_and_written() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_path("XDG_CACHE_HOME", tmp.path());
        let profile = test_profile();
        let descriptor = test_network_descriptor();
        let owner_public_key = format!("0x{}", "11".repeat(32));

        let loaded = load_or_create_remote_wallet_create_attempt(
            &profile,
            "alice",
            "main",
            &owner_public_key,
            &descriptor,
        )
        .expect("bug: attempt should be created");

        assert!(loaded.path.is_file());
        assert_eq!(loaded.attempt.profile, "preview");
        assert_eq!(loaded.attempt.key_name, "alice");
        assert_eq!(loaded.attempt.wallet_name, "main");
        assert_eq!(loaded.attempt.owner_public_key, owner_public_key);
        assert_eq!(loaded.attempt.network_id, "devnet-preview");
        assert_eq!(
            loaded.attempt.genesis_hash,
            format!("0x{}", "01".repeat(32))
        );
        assert_eq!(
            loaded.attempt.wallet_contract_hash,
            format!("0x{}", "03".repeat(32))
        );
        assert_eq!(loaded.attempt.deploy_nonce.len(), 66);
        assert_eq!(loaded.attempt.client_nonce.len(), 34);

        let stored = read_remote_wallet_create_attempt(&loaded.path)
            .expect("bug: attempt should read")
            .expect("bug: attempt file should exist");
        assert_eq!(stored, loaded.attempt);
    }

    #[test]
    #[serial]
    fn remote_wallet_create_attempt_reuses_matching_record() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_path("XDG_CACHE_HOME", tmp.path());
        let profile = test_profile();
        let descriptor = test_network_descriptor();
        let owner_public_key = format!("0x{}", "11".repeat(32));

        let first = load_or_create_remote_wallet_create_attempt(
            &profile,
            "alice",
            "main",
            &owner_public_key,
            &descriptor,
        )
        .expect("bug: attempt should be created");
        let second = load_or_create_remote_wallet_create_attempt(
            &profile,
            "alice",
            "main",
            &owner_public_key,
            &descriptor,
        )
        .expect("bug: attempt should be reused");

        assert_eq!(second.path, first.path);
        assert_eq!(second.attempt, first.attempt);
    }

    #[test]
    #[serial]
    fn remote_wallet_create_attempt_overwrites_stale_record() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_path("XDG_CACHE_HOME", tmp.path());
        let profile = test_profile();
        let descriptor = test_network_descriptor();
        let owner_public_key = format!("0x{}", "11".repeat(32));
        let path = remote_wallet_create_attempt_path(
            &profile.name,
            "alice",
            "main",
            "devnet-preview",
            &format!("0x{}", "01".repeat(32)),
        )
        .expect("bug: attempt path should resolve");
        let stale = RemoteWalletCreateAttempt {
            profile: profile.name.clone(),
            key_name: "alice".into(),
            wallet_name: "main".into(),
            network_id: "devnet-preview".into(),
            genesis_hash: format!("0x{}", "01".repeat(32)),
            owner_public_key: format!("0x{}", "22".repeat(32)),
            deploy_nonce: format!("0x{}", "44".repeat(32)),
            client_nonce: format!("0x{}", "55".repeat(16)),
            wallet_contract_hash: format!("0x{}", "03".repeat(32)),
            wallet_address: format!("0x{}", "66".repeat(32)),
        };
        write_remote_wallet_create_attempt(&path, &stale)
            .expect("bug: stale attempt should be written");

        let loaded = load_or_create_remote_wallet_create_attempt(
            &profile,
            "alice",
            "main",
            &owner_public_key,
            &descriptor,
        )
        .expect("bug: stale attempt should be replaced");

        assert_eq!(loaded.path, path);
        assert_ne!(loaded.attempt, stale);
        assert_eq!(loaded.attempt.owner_public_key, owner_public_key);
        let stored = read_remote_wallet_create_attempt(&loaded.path)
            .expect("bug: replacement should read")
            .expect("bug: replacement file should exist");
        assert_eq!(stored, loaded.attempt);
    }

    #[test]
    #[serial]
    fn remote_wallet_create_attempt_remove_ignores_missing_file() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_path("XDG_CACHE_HOME", tmp.path());
        let path = tmp.path().join("missing.json");
        let prepared = test_prepared_remote_wallet_create(path);

        remove_remote_wallet_create_attempt(&prepared)
            .expect("bug: missing attempt removal should be idempotent");
    }

    #[test]
    #[serial]
    fn remote_wallet_create_attempt_remove_deletes_file() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_path("XDG_CACHE_HOME", tmp.path());
        let path = tmp.path().join("attempt.json");
        std::fs::write(&path, b"{}").expect("bug: attempt file should be written");
        let prepared = test_prepared_remote_wallet_create(path.clone());

        remove_remote_wallet_create_attempt(&prepared)
            .expect("bug: attempt removal should succeed");

        assert!(!path.exists());
    }

    #[test]
    #[serial]
    fn remote_wallet_create_attempt_path_uses_cache_dir() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_path("XDG_CACHE_HOME", tmp.path());

        let path = remote_wallet_create_attempt_path(
            "preview",
            "alice",
            "main",
            "devnet-preview",
            &format!("0x{}", "01".repeat(32)),
        )
        .expect("bug: attempt path should resolve");

        assert!(path.starts_with(tmp.path().join("groundhog/wallet-create-attempts")));
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("json"));
    }

    fn test_profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "preview".into(),
            network: Some(crate::config::Network::Devnet),
            rpc_url: Some("https://rpc.devnet.example".into()),
            faucet_url: Some("https://faucet.devnet.example".into()),
            rpc_url_cli_override: false,
            default_key: Some("alice".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        }
    }

    fn test_network_descriptor() -> groundhog_json_rpc_types::network_descriptor::NetworkDescriptor
    {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": groundhog_json_rpc_types::network_descriptor::NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": "groundhog-network",
            "networkId": "devnet-preview",
            "protocol": "x23",
            "genesisHash": format!("0x{}", "01".repeat(32)),
            "endpoints": {
                "rpcUrl": "https://rpc.devnet.example"
            },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "02".repeat(32)),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": {
                "walletAddress": format!("0x{}", "04".repeat(32))
            },
            "walletCreate": {
                "prefundRawAmount": "100000000",
                "walletContractHash": format!("0x{}", "03".repeat(32))
            },
            "devWallet": {
                "deployEntrypoint": "0xf969542572bd6860",
                "doActionEntrypoint": "0xa0fff94c64fbaf3c"
            }
        }))
        .expect("bug: test network descriptor should deserialize")
    }

    fn test_prepared_remote_wallet_create(attempt_path: PathBuf) -> PreparedRemoteWalletCreate {
        PreparedRemoteWalletCreate {
            faucet_url: "https://faucet.devnet.example".into(),
            rpc_url: "https://rpc.devnet.example".into(),
            profile_name: "preview".into(),
            key_name: "alice".into(),
            wallet_name: "main".into(),
            owner_public_key: format!("0x{}", "11".repeat(32)),
            deploy_nonce: format!("0x{}", "22".repeat(32)),
            client_nonce: format!("0x{}", "33".repeat(16)),
            owner_signature: format!("0x{}", "44".repeat(64)),
            network_id: "devnet-preview".into(),
            genesis_hash: format!("0x{}", "01".repeat(32)),
            wallet_contract_hash: format!("0x{}", "03".repeat(32)),
            wallet_address: format!("0x{}", "55".repeat(32)),
            prefund_raw_amount: "100000000".into(),
            attempt_path,
        }
    }
}
