use std::path::{Path, PathBuf};

use getrandom::getrandom;
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::types::SHA256Hash;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::account_resolution::{
    LocalBootstrapRole, lookup_dev_account_address, resolve_local_bootstrap_account,
};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{
    Network, ProfileOverrides, ResolvedProfile, cache_dir, resolve_profile_with_overrides,
};
use crate::dev_keys;
use crate::faucet_client::{client as faucet_client, get_json, post_json};
use crate::hog;
use crate::profile_backend::{ProfileBackend, resolve_profile_backend};
use crate::result::CommandResult;
use crate::runtime_manifest::{configured_runtime_manifest_path, load_configured_runtime_manifest};
use crate::tx_utils::{
    ensure_tx_success, format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height,
    submit_and_poll_blocking,
};
use crate::{CliError, Print};

pub const AIRDROP_JSON_SCHEMA_VERSION: u32 = 1;
const DEFAULT_AIRDROP_HOG: &str = "10";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedAirdrop {
    Local(PreparedLocalAirdrop),
    Remote(PreparedRemoteAirdrop),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLocalAirdrop {
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
    pub(crate) amount_hog: String,
    pub(crate) raw_amount: u128,
    pub(crate) faucet_key_name: String,
    pub(crate) faucet_role: String,
    pub(crate) tx_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedRemoteAirdrop {
    pub(crate) faucet_url: String,
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
    client_nonce: String,
    network_id: String,
    genesis_hash: String,
    attempt_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRemoteAirdropDryRun {
    faucet_url: String,
    rpc_url: String,
    recipient_input: String,
    recipient_wallet: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAirdropRequest {
    recipient: String,
    client_nonce: String,
    network_id: String,
    genesis_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAirdropResponse {
    attempt_status: String,
    raw_amount: String,
    tx_hash: Option<String>,
    height: Option<groundhog_json_rpc_types::BlockHeight>,
    status: Option<ExecStatus>,
    retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAirdropAttempt {
    profile: String,
    recipient_wallet: String,
    network_id: String,
    genesis_hash: String,
    client_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedRemoteAirdropAttempt {
    attempt: RemoteAirdropAttempt,
    path: PathBuf,
}

pub fn run(
    recipient: &str,
    amount: Option<&str>,
    dry_run: bool,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    if dry_run
        && let Some(prepared) =
            prepare_remote_airdrop_dry_run(&cwd, recipient, amount, profile, overrides)?
    {
        return print_remote_dry_run(&prepared, cmd, print);
    }
    let prepared = prepare_airdrop(&cwd, recipient, amount, profile, overrides)?;

    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    match prepared {
        PreparedAirdrop::Local(prepared) => run_local_airdrop(&prepared, cmd, print)?,
        PreparedAirdrop::Remote(prepared) => run_remote_airdrop(&prepared, cmd, print)?,
    }

    Ok(())
}

fn prepare_remote_airdrop_dry_run(
    cwd: &Path,
    recipient: &str,
    amount: Option<&str>,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<Option<PreparedRemoteAirdropDryRun>, CliError> {
    let profile = resolve_profile_with_overrides(cwd, profile_name, overrides)?;
    if !matches!(
        resolve_airdrop_backend(&profile)?,
        ProfileBackend::RemoteFaucet
    ) {
        return Ok(None);
    }
    if amount.is_some() {
        return Err(CliError::Other(
            "remote airdrop amount is set by the faucet service".into(),
        ));
    }
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| CliError::Other("remote airdrop requires faucet_url".into()))?
        .trim_end_matches('/')
        .to_owned();
    let recipient_wallet = resolve_airdrop_recipient(&profile, recipient)?;
    parse_contract_address(&recipient_wallet)?;

    Ok(Some(PreparedRemoteAirdropDryRun {
        faucet_url,
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
    }))
}

fn run_local_airdrop(
    prepared: &PreparedLocalAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Airdropping {} HOG to {}...",
        prepared.amount_hog, prepared.recipient_wallet
    ));
    let (tx_hash, receipt) = submit_and_poll_blocking(&prepared.rpc_url, &prepared.tx_hex)?;
    let tx_hash = format_tx_hash(&tx_hash);
    ensure_tx_success("airdrop", &receipt, &tx_hash)?;
    print.status(format_args!("Airdrop sent: {tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "local",
            "recipient": prepared.recipient_input,
            "recipient_wallet": prepared.recipient_wallet,
            "amount_hog": prepared.amount_hog,
            "raw_amount": prepared.raw_amount.to_string(),
            "faucet": {
                "key": prepared.faucet_key_name,
                "role": prepared.faucet_role,
            },
            "tx_hash": tx_hash,
            "height": receipt.height,
            "status": receipt.status,
        }),
    ))
}

fn run_remote_airdrop(
    prepared: &PreparedRemoteAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Requesting remote airdrop to {}...",
        prepared.recipient_wallet
    ));
    let client = faucet_client()?;
    let response: RemoteAirdropResponse = post_json(
        &client,
        &format!("{}/v1/airdrop", prepared.faucet_url),
        &RemoteAirdropRequest {
            recipient: prepared.recipient_wallet.clone(),
            client_nonce: prepared.client_nonce.clone(),
            network_id: prepared.network_id.clone(),
            genesis_hash: prepared.genesis_hash.clone(),
        },
    )?;
    if response.attempt_status != "succeeded" {
        let retry_after = response
            .retry_after_seconds
            .map(|seconds| format!("; retry after {seconds}s"))
            .unwrap_or_default();
        return Err(CliError::Other(format!(
            "remote airdrop is pending ({}) - retry the same command to continue{}",
            response.attempt_status, retry_after
        )));
    }
    let status = response
        .status
        .as_ref()
        .ok_or_else(|| CliError::Other("remote airdrop response omitted status".into()))?;
    if !matches!(status, ExecStatus::Ok) {
        return Err(CliError::Other(
            "remote airdrop transaction did not commit successfully".into(),
        ));
    }
    remove_remote_airdrop_attempt(prepared)?;
    let tx_hash = response
        .tx_hash
        .clone()
        .ok_or_else(|| CliError::Other("remote airdrop response omitted txHash".into()))?;
    let height = response
        .height
        .ok_or_else(|| CliError::Other("remote airdrop response omitted height".into()))?;
    print.status(format_args!("Airdrop sent: {tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "remote",
            "recipient": prepared.recipient_input,
            "recipient_wallet": prepared.recipient_wallet,
            "faucet_url": prepared.faucet_url,
            "rpc_url": prepared.rpc_url,
            "raw_amount": response.raw_amount,
            "tx_hash": tx_hash,
            "height": height,
            "status": status,
        }),
    ))
}

pub(crate) fn prepare_airdrop(
    cwd: &Path,
    recipient: &str,
    amount: Option<&str>,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<PreparedAirdrop, CliError> {
    let profile = resolve_profile_with_overrides(cwd, profile_name, overrides)?;
    match resolve_airdrop_backend(&profile)? {
        ProfileBackend::RemoteFaucet => {
            if amount.is_some() {
                return Err(CliError::Other(
                    "remote airdrop amount is set by the faucet service".into(),
                ));
            }
            prepare_remote_airdrop(&profile, recipient)
        }
        ProfileBackend::LocalManifest => prepare_local_airdrop(&profile, recipient, amount),
        ProfileBackend::RpcOnly => Err(CliError::Other(format!(
            "profile `{}` is RPC-only; airdrop requires either `runtime_manifest` or `faucet_url`",
            profile.name
        ))),
    }
}

fn resolve_airdrop_backend(profile: &ResolvedProfile) -> Result<ProfileBackend, CliError> {
    match profile.network {
        Some(Network::Local) => {
            if profile.rpc_url.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets local, but airdrop requires `rpc_url`; run `groundhog node` or configure `rpc_url`",
                ));
            }
            if configured_runtime_manifest_path(profile)?.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets local, but airdrop requires a local runtime manifest; run `groundhog node` or configure `runtime_manifest`",
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
                    "targets devnet, but airdrop requires `rpc_url`",
                ));
            }
            if profile.faucet_url.is_none() {
                return Err(profile_config_error(
                    profile,
                    "targets devnet, but airdrop requires `faucet_url`",
                ));
            }
            Ok(ProfileBackend::RemoteFaucet)
        }
        Some(Network::Mainnet) => Err(profile_config_error(
            profile,
            "targets mainnet, but airdrop is not supported because mainnet does not exist yet",
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

fn prepare_local_airdrop(
    profile: &ResolvedProfile,
    recipient: &str,
    amount: Option<&str>,
) -> Result<PreparedAirdrop, CliError> {
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let manifest = load_configured_runtime_manifest(profile, "faucet")?
        .ok_or_else(|| CliError::Other("airdrop requires a local runtime manifest".into()))?;
    let wallet_entrypoint = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| CliError::Other(error.to_string()))?;
    let recipient_wallet = resolve_airdrop_recipient(profile, recipient)?;
    let recipient_address = parse_contract_address(&recipient_wallet)?;
    let raw_amount = hog::parse_amount(amount.unwrap_or(DEFAULT_AIRDROP_HOG))?;
    let amount_hog = hog::format_amount(raw_amount);
    let faucet = resolve_local_bootstrap_account(profile, LocalBootstrapRole::Faucet)?;

    let hog_address = parse_contract_address(hog::TOKEN_ADDRESS)?;
    let transfer_entrypoint = hog::transfer_entrypoint();
    let transfer_calldata = hog::encode_transfer_calldata(&recipient_address, raw_amount)?;

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(&rpc_url).await })?;

    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: faucet.wallet_address,
        wallet_entrypoint,
        target_address: hog_address,
        target_entrypoint: transfer_entrypoint,
        target_calldata: transfer_calldata.as_slice(),
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        target_gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce: nonce_from_timestamp(),
    })
    .map_err(|error| CliError::Other(error.to_string()))?;
    let tx_hex =
        sign_and_encode_transaction_hex(tx, &faucet.signer, SignTransactionParams::default())
            .map_err(|error| CliError::Other(error.to_string()))?;

    Ok(PreparedAirdrop::Local(PreparedLocalAirdrop {
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
        amount_hog,
        raw_amount,
        faucet_key_name: faucet.key_name,
        faucet_role: faucet.role,
        tx_hex,
    }))
}

fn prepare_remote_airdrop(
    profile: &ResolvedProfile,
    recipient: &str,
) -> Result<PreparedAirdrop, CliError> {
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| CliError::Other("remote airdrop requires faucet_url".into()))?;
    let faucet_url = faucet_url.trim_end_matches('/').to_owned();
    let recipient_wallet = resolve_airdrop_recipient(profile, recipient)?;
    parse_contract_address(&recipient_wallet)?;
    let client = faucet_client()?;
    let descriptor: NetworkDescriptor = get_json(&client, &format!("{faucet_url}/v1/network"))?;

    prepare_remote_airdrop_with_descriptor(
        profile,
        recipient,
        recipient_wallet,
        faucet_url,
        rpc_url,
        &descriptor,
    )
}

fn prepare_remote_airdrop_with_descriptor(
    profile: &ResolvedProfile,
    recipient: &str,
    recipient_wallet: String,
    faucet_url: String,
    rpc_url: String,
    descriptor: &NetworkDescriptor,
) -> Result<PreparedAirdrop, CliError> {
    let loaded_attempt =
        load_or_create_remote_airdrop_attempt(profile, &recipient_wallet, descriptor)?;
    let attempt = loaded_attempt.attempt;

    Ok(PreparedAirdrop::Remote(PreparedRemoteAirdrop {
        faucet_url,
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
        client_nonce: attempt.client_nonce,
        network_id: attempt.network_id,
        genesis_hash: attempt.genesis_hash,
        attempt_path: loaded_attempt.path,
    }))
}

fn load_or_create_remote_airdrop_attempt(
    profile: &ResolvedProfile,
    recipient_wallet: &str,
    descriptor: &NetworkDescriptor,
) -> Result<LoadedRemoteAirdropAttempt, CliError> {
    let network_id = descriptor.network_id.clone();
    let genesis_hash = format_sha256_hash(&descriptor.genesis_hash.0);
    let path =
        remote_airdrop_attempt_path(&profile.name, recipient_wallet, &network_id, &genesis_hash)?;
    if let Some(attempt) = read_remote_airdrop_attempt(&path)?
        && attempt.profile == profile.name
        && attempt.recipient_wallet == recipient_wallet
        && attempt.network_id == network_id
        && attempt.genesis_hash == genesis_hash
    {
        return Ok(LoadedRemoteAirdropAttempt { attempt, path });
    }

    let mut client_nonce = [0u8; 16];
    getrandom(&mut client_nonce).map_err(|error| CliError::Other(error.to_string()))?;
    let attempt = RemoteAirdropAttempt {
        profile: profile.name.clone(),
        recipient_wallet: recipient_wallet.to_owned(),
        network_id,
        genesis_hash,
        client_nonce: format!("0x{}", hex::encode(client_nonce)),
    };
    write_remote_airdrop_attempt(&path, &attempt)?;
    Ok(LoadedRemoteAirdropAttempt { attempt, path })
}

fn read_remote_airdrop_attempt(path: &Path) -> Result<Option<RemoteAirdropAttempt>, CliError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| CliError::Other(format!("failed to parse `{}`: {error}", path.display())))
}

fn write_remote_airdrop_attempt(
    path: &Path,
    attempt: &RemoteAirdropAttempt,
) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents =
        serde_json::to_vec_pretty(attempt).map_err(|error| CliError::Other(error.to_string()))?;
    std::fs::write(path, contents)?;
    Ok(())
}

fn remove_remote_airdrop_attempt(prepared: &PreparedRemoteAirdrop) -> Result<(), CliError> {
    match std::fs::remove_file(&prepared.attempt_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remote_airdrop_attempt_path(
    profile: &str,
    recipient_wallet: &str,
    network_id: &str,
    genesis_hash: &str,
) -> Result<PathBuf, CliError> {
    let hash: [u8; 32] = Sha256::digest(format!(
        "{profile}\0{recipient_wallet}\0{network_id}\0{genesis_hash}"
    ))
    .into();
    Ok(cache_dir()?
        .join("airdrop-attempts")
        .join(format!("{}.json", hex::encode(hash))))
}

fn format_sha256_hash(hash: &SHA256Hash) -> String {
    format!("0x{}", hex::encode(hash.as_ref()))
}

fn resolve_airdrop_recipient(
    profile: &ResolvedProfile,
    recipient: &str,
) -> Result<String, CliError> {
    if let Some(wallet) = profile.resolve_wallet(recipient) {
        return Ok(wallet);
    }
    if recipient.starts_with("0x") {
        parse_contract_address(recipient).map_err(|_| {
            CliError::Other("wallet address must be a 0x-prefixed 64-character hex string".into())
        })?;
        return Ok(recipient.to_owned());
    }
    if let Some(index) = dev_keys::dev_key_index(recipient) {
        let address = lookup_dev_account_address(profile, recipient, index)?;
        return Ok(format_contract_address(&address.0));
    }
    Err(CliError::Other(format!(
        "unknown wallet `{recipient}`; run `groundhog wallet create --key {recipient}` or pass a 0x-prefixed wallet address"
    )))
}

fn print_dry_run(
    prepared: &PreparedAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    match prepared {
        PreparedAirdrop::Local(prepared) => {
            print.status(format_args!(
                "Prepared dry-run airdrop of {} HOG to {}.",
                prepared.amount_hog, prepared.recipient_wallet
            ));
            print.status(format_args!("  raw amount: {}", prepared.raw_amount));
            print.status(format_args!(
                "  faucet: {} ({})",
                prepared.faucet_key_name, prepared.faucet_role
            ));
            print.status("  submitted: no (--dry-run)");

            print.json(&CommandResult::new(
                cmd,
                json!({
                    "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
                    "dry_run": true,
                    "submitted": false,
                    "mode": "local",
                    "recipient": prepared.recipient_input,
                    "recipient_wallet": prepared.recipient_wallet,
                    "amount_hog": prepared.amount_hog,
                    "raw_amount": prepared.raw_amount.to_string(),
                    "faucet": {
                        "key": prepared.faucet_key_name,
                        "role": prepared.faucet_role,
                    },
                }),
            ))?;
        }
        PreparedAirdrop::Remote(prepared) => {
            print.status(format_args!(
                "Prepared dry-run remote airdrop to {}.",
                prepared.recipient_wallet
            ));
            print.status(format_args!("  faucet: {}", prepared.faucet_url));
            print.status("  submitted: no (--dry-run)");

            print.json(&CommandResult::new(
                cmd,
                json!({
                    "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
                    "dry_run": true,
                    "submitted": false,
                    "mode": "remote",
                    "recipient": prepared.recipient_input,
                    "recipient_wallet": prepared.recipient_wallet,
                    "faucet_url": prepared.faucet_url,
                    "rpc_url": prepared.rpc_url,
                    "planned_requests": [
                        "POST /v1/airdrop"
                    ],
                }),
            ))?;
        }
    }

    Ok(())
}

fn print_remote_dry_run(
    prepared: &PreparedRemoteAirdropDryRun,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Prepared dry-run remote airdrop to {}.",
        prepared.recipient_wallet
    ));
    print.status(format_args!("  faucet: {}", prepared.faucet_url));
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "dry_run": true,
            "submitted": false,
            "mode": "remote",
            "recipient": prepared.recipient_input,
            "recipient_wallet": prepared.recipient_wallet,
            "faucet_url": prepared.faucet_url,
            "rpc_url": prepared.rpc_url,
            "planned_requests": [
                "GET /v1/network",
                "POST /v1/airdrop"
            ],
            "idempotency_key": "profile + recipient + networkId + genesisHash + clientNonce",
        }),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::env;
    use std::ffi::OsString;

    use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
    use serial_test::serial;

    use super::{
        PreparedAirdrop, prepare_remote_airdrop_with_descriptor, resolve_airdrop_backend,
        resolve_airdrop_recipient,
    };
    use crate::config::{Network, ResolvedProfile, SavedWalletBinding};
    use crate::hog;

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
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
    fn resolves_saved_or_direct_recipient() {
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format!("0x{}", "ab".repeat(32)),
                owner_public_key: format!("0x{}", "ef".repeat(32)),
            },
        );
        let profile = test_profile(wallets);

        assert_eq!(
            resolve_airdrop_recipient(&profile, "alice").unwrap(),
            format!("0x{}", "ab".repeat(32))
        );
        assert_eq!(
            resolve_airdrop_recipient(&profile, &format!("0x{}", "cd".repeat(32))).unwrap(),
            format!("0x{}", "cd".repeat(32))
        );
        assert!(resolve_airdrop_recipient(&profile, "missing").is_err());
    }

    #[test]
    fn malformed_direct_recipient_reports_address_format() {
        let profile = test_profile(BTreeMap::new());

        let err = resolve_airdrop_recipient(&profile, "0xdeadbeef")
            .expect_err("bug: malformed direct address should fail");

        assert!(err.to_string().contains("64-character hex string"));
    }

    #[test]
    #[serial]
    fn prepares_remote_airdrop_from_profile_faucet() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        unsafe { env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format!("0x{}", "ab".repeat(32)),
                owner_public_key: format!("0x{}", "ef".repeat(32)),
            },
        );
        let mut profile = test_profile(wallets);
        profile.faucet_url = Some("http://127.0.0.1:8081/".into());
        let recipient_wallet = resolve_airdrop_recipient(&profile, "alice").unwrap();

        let prepared = prepare_remote_airdrop_with_descriptor(
            &profile,
            "alice",
            recipient_wallet,
            "http://127.0.0.1:8081".into(),
            "http://127.0.0.1:9545".into(),
            &test_network_descriptor(),
        )
        .unwrap();

        let PreparedAirdrop::Remote(prepared) = prepared else {
            panic!("bug: expected remote airdrop");
        };
        assert_eq!(prepared.faucet_url, "http://127.0.0.1:8081");
        assert_eq!(prepared.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(prepared.recipient_input, "alice");
        assert_eq!(prepared.recipient_wallet, format!("0x{}", "ab".repeat(32)));
        assert_eq!(prepared.network_id, "devnet-preview");
        assert_eq!(prepared.genesis_hash, format!("0x{}", "01".repeat(32)));
        assert_eq!(prepared.client_nonce.len(), 34);
        unsafe { env::remove_var("XDG_CACHE_HOME") };
    }

    #[test]
    #[serial]
    fn airdrop_devnet_missing_faucet_reports_devnet_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "devnet".into();
        profile.network = Some(Network::Devnet);

        let error =
            resolve_airdrop_backend(&profile).expect_err("bug: missing devnet faucet should fail");

        assert_eq!(
            error.to_string(),
            "profile `devnet` targets devnet, but airdrop requires `faucet_url`"
        );
    }

    #[test]
    #[serial]
    fn airdrop_devnet_missing_rpc_reports_devnet_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "devnet".into();
        profile.network = Some(Network::Devnet);
        profile.rpc_url = None;
        profile.faucet_url = Some("http://127.0.0.1:8081".into());

        let error =
            resolve_airdrop_backend(&profile).expect_err("bug: missing devnet rpc should fail");

        assert_eq!(
            error.to_string(),
            "profile `devnet` targets devnet, but airdrop requires `rpc_url`"
        );
    }

    #[test]
    #[serial]
    fn airdrop_devnet_with_urls_uses_remote_faucet() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "devnet".into();
        profile.network = Some(Network::Devnet);
        profile.faucet_url = Some("http://127.0.0.1:8081".into());

        let backend = resolve_airdrop_backend(&profile)
            .expect("bug: configured devnet profile should resolve");

        assert_eq!(
            backend,
            crate::profile_backend::ProfileBackend::RemoteFaucet
        );
    }

    #[test]
    #[serial]
    fn airdrop_local_missing_manifest_reports_local_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "local".into();
        profile.network = Some(Network::Local);

        let error =
            resolve_airdrop_backend(&profile).expect_err("bug: missing local manifest should fail");

        assert_eq!(
            error.to_string(),
            "profile `local` targets local, but airdrop requires a local runtime manifest; run `groundhog node` or configure `runtime_manifest`"
        );
    }

    #[test]
    fn hog_transfer_entrypoint_matches_standard_erc20_interface() {
        let entrypoint = contract_entrypoint_id(
            Some("ERC20"),
            "0.1.3",
            &NormalizedMethodSignature::new("transfer"),
        );

        assert_eq!(entrypoint.as_ref(), hog::transfer_entrypoint().as_ref());
    }

    fn test_profile(wallets: BTreeMap<String, SavedWalletBinding>) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: Some("http://127.0.0.1:9545".into()),
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            default_wallet: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets,
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
            "endpoints": { "rpcUrl": "http://127.0.0.1:9545" },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "02".repeat(32)),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": { "walletAddress": format!("0x{}", "04".repeat(32)) },
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
}
