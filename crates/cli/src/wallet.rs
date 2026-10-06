use crate::account_resolution::{resolve_selected_wallet, validate_saved_wallet_binding};
use crate::codec::format_contract_address;
use crate::config::{
    ProfileOverrides, SavedWalletBinding, WalletKind, find_project_config,
    prepare_profile_wallet_removal, remove_prepared_profile_wallet,
    resolve_default_key_for_profile, resolve_profile, resolve_profile_with_overrides,
    save_native_wallet, save_profile_default_wallet,
};
use crate::keys::load_local_public_key;
use crate::result::CommandResult;
use crate::{CliError, Print};
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PubkeyAccount};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

/// JSON contract for offline native account creation.
pub const WALLET_CREATE_JSON_SCHEMA_VERSION: u32 = 6;

/// Result of deriving and optionally saving a native account. No chain state is inferred.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WalletCreateReport {
    /// Version of this command result.
    pub schema_version: u32,
    /// Saved wallet name.
    pub name: String,
    /// Stored signing key name.
    pub key: String,
    /// Selected configuration profile.
    pub profile: String,
    /// Derived account address.
    pub wallet: String,
    /// Account owner public key.
    pub owner_public_key: String,
    /// Transaction format for this account.
    pub kind: WalletKind,
    /// Nonce used to derive the account address.
    #[serde(with = "groundhog_json_rpc_types::decimal")]
    #[schemars(with = "groundhog_json_rpc_types::decimal::Decimal<u64>")]
    pub account_nonce: u64,
    /// Whether saving was skipped.
    pub dry_run: bool,
    /// Whether the binding is now persisted.
    pub saved: bool,
    /// Whether this invocation inserted the binding.
    pub created: bool,
    /// Explicit funding command for this binding and profile.
    pub funding_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SavedWallet {
    name: String,
    address: String,
    owner_public_key: String,
    #[serde(skip_serializing_if = "WalletKind::is_legacy")]
    kind: WalletKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "groundhog_json_rpc_types::decimal::option")]
    account_nonce: Option<u64>,
    is_default: bool,
}

/// Derive an empty native account and save its local binding without contacting a node or faucet.
#[expect(clippy::too_many_arguments, reason = "CLI command boundary")]
pub fn create(
    key_name: Option<&str>,
    wallet_name: Option<&str>,
    dry_run: bool,
    prefund: Option<&str>,
    force: bool,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if prefund.is_some() {
        return Err(CliError::WalletCreatePrefundUnsupported);
    }
    if !dry_run {
        print.require_confirmation_available(force, "create a wallet")?;
    }
    let cwd = std::env::current_dir()?;
    let inputs = resolve_wallet_create_inputs(&cwd, key_name, wallet_name, profile, overrides)?;
    let owner = load_local_public_key(&inputs.key_name)?
        .ok_or_else(|| CliError::KeyNotFound(inputs.key_name.clone()))?;
    let account = PubkeyAccount::from_public_key_bytes(owner, DEFAULT_PUBKEY_ACCOUNT_NONCE);
    let binding = SavedWalletBinding {
        address: format_contract_address(&account.address()),
        owner_public_key: format!("0x{}", hex::encode(owner)),
        kind: WalletKind::Native,
        account_nonce: Some(account.nonce),
    };
    if let Some(existing) = inputs.profile.wallets.get(&inputs.wallet_name) {
        let validated = validate_saved_wallet_binding(&inputs.wallet_name, existing)?;
        if existing.kind != binding.kind
            || existing.account_nonce != binding.account_nonce
            || validated.address != account.address()
            || validated.owner_public_key != owner
        {
            return Err(CliError::WalletBindingConflict {
                wallet_name: inputs.wallet_name,
            });
        }
    }
    let mut created = false;
    if !dry_run {
        if !print.confirm_review(
            "Review native wallet",
            force,
            "create a wallet",
            &format!("Save empty native wallet `{}`?", inputs.wallet_name),
            |print| {
                print.detail("Name", &inputs.wallet_name);
                print.detail("Key", &inputs.key_name);
                print.detail("Profile", &inputs.profile.name);
                print.detail("Address", &binding.address);
                print.detail("Funding", "Not requested; this only saves a local binding");
            },
        )? {
            print.cancelled();
            return Ok(());
        }
        created = save_native_wallet(
            &cwd,
            &inputs.profile.name,
            &inputs.wallet_name,
            &inputs.key_name,
            &binding,
        )?;
    }
    let mut funding_command = format!(
        "groundhog airdrop {} --profile={}",
        crate::terminal::render_shell_arg(&binding.address),
        crate::terminal::render_shell_arg(&inputs.profile.name)
    );
    if let Some(rpc_url) = overrides.rpc_url.as_deref() {
        funding_command.push_str(" --rpc-url=");
        funding_command.push_str(&crate::terminal::render_shell_arg(rpc_url));
    }
    if let Some(manifest) = overrides.runtime_manifest.as_ref() {
        funding_command.push_str(" --runtime-manifest=");
        funding_command.push_str(&crate::terminal::render_shell_arg(
            &manifest.to_string_lossy(),
        ));
    }
    print_native_wallet(&binding, &funding_command, dry_run, created, print);
    print.json(&CommandResult::new(
        cmd,
        WalletCreateReport {
            schema_version: WALLET_CREATE_JSON_SCHEMA_VERSION,
            name: inputs.wallet_name,
            key: inputs.key_name,
            profile: inputs.profile.name,
            wallet: binding.address,
            owner_public_key: binding.owner_public_key,
            kind: binding.kind,
            account_nonce: account.nonce,
            dry_run,
            saved: !dry_run,
            created,
            funding_command,
        },
    ))?;
    Ok(())
}

fn print_native_wallet(
    binding: &SavedWalletBinding,
    funding_command: &str,
    dry_run: bool,
    created: bool,
    print: &Print,
) {
    let outcome = if dry_run {
        crate::print::Outcome::DryRun
    } else if created {
        crate::print::Outcome::Success
    } else {
        crate::print::Outcome::Unchanged
    };
    print.group(outcome, "Native wallet", |print| {
        print.status(if dry_run {
            "Derived native wallet (not saved)."
        } else {
            "Saved native wallet. No funding was requested."
        });
        print.detail("wallet", &binding.address);
        print.detail("fund", funding_command);
    });
}

pub fn remove(
    name: &str,
    profile: Option<&str>,
    force: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if name.trim().is_empty() {
        return Err(CliError::EmptyWalletName);
    }
    let cwd = std::env::current_dir()?;
    let project_root = find_project_config(&cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);
    let prepared_removal = prepare_profile_wallet_removal(&project_root, profile, name)?;
    let profile_name = prepared_removal.profile_name().to_owned();
    let target_profile = prepared_removal.profile();
    let inherited_profile = prepared_removal.inherited_profile();
    let writes_project_profile = prepared_removal.writes_project_profile();
    let configured_default = target_profile.default_wallet.clone();
    let cleared_default = configured_default.as_deref() == Some(name.trim());
    let removes_inherited_default = writes_project_profile
        && configured_default.is_none()
        && inherited_profile.and_then(|profile| profile.default_wallet.as_deref())
            == Some(name.trim());
    if removes_inherited_default {
        return Err(CliError::InheritedDefaultWalletRemoval {
            wallet_name: name.trim().to_owned(),
            profile: profile_name,
        });
    }
    if cleared_default
        && let Some(inherited_wallet) =
            inherited_profile.and_then(|profile| profile.default_wallet.as_deref())
    {
        let remains_saved = if inherited_wallet == name.trim() {
            inherited_profile.is_some_and(|profile| profile.wallets.contains_key(inherited_wallet))
        } else {
            target_profile.wallets.contains_key(inherited_wallet)
                || inherited_profile
                    .is_some_and(|profile| profile.wallets.contains_key(inherited_wallet))
        };
        if !remains_saved {
            return Err(CliError::WalletRemovalExposesMissingDefault {
                wallet_name: name.trim().to_owned(),
                inherited_wallet: inherited_wallet.to_owned(),
                profile: profile_name,
            });
        }
    }
    let effective_default_wallet = if cleared_default {
        inherited_profile.and_then(|profile| profile.default_wallet.clone())
    } else {
        configured_default
            .or_else(|| inherited_profile.and_then(|profile| profile.default_wallet.clone()))
    };
    if !confirm_wallet_removal(name, &profile_name, force, print)? {
        print.cancelled();
        return Ok(());
    }
    remove_prepared_profile_wallet(prepared_removal)?;

    print.group(crate::print::Outcome::Success, "Wallet removed", |print| {
        print.status(format_args!("Removed wallet `{}`.", name.trim()));
        if cleared_default
            && writes_project_profile
            && let Some(wallet_name) = effective_default_wallet.as_deref()
        {
            print.warn(format_args!(
                "profile `{profile_name}` now inherits default wallet `{wallet_name}` from global config"
            ));
        }
    });
    print.json(&CommandResult::new(
        cmd,
        json!({
            "name": name.trim(),
            "profile": profile_name,
            "clearedConfiguredDefault": cleared_default,
            "effectiveDefaultWallet": effective_default_wallet,
        }),
    ))?;

    Ok(())
}

fn confirm_wallet_removal(
    name: &str,
    profile: &str,
    force: bool,
    print: &Print,
) -> Result<bool, CliError> {
    print.confirm(
        force,
        "remove a saved wallet",
        format_args!("Remove wallet `{}` from profile `{profile}`?", name.trim()),
    )
}

pub fn list(profile: Option<&str>, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let profile = resolve_profile(&cwd, profile)?;
    let default_wallet = profile.default_wallet;
    let wallets: Vec<SavedWallet> = profile
        .wallets
        .into_iter()
        .map(|(name, wallet)| SavedWallet {
            is_default: default_wallet.as_deref() == Some(&name),
            name,
            address: wallet.address,
            owner_public_key: wallet.owner_public_key,
            kind: wallet.kind,
            account_nonce: wallet.account_nonce,
        })
        .collect();

    if print.is_json() {
        print.json(&CommandResult::new(cmd, json!({ "wallets": wallets })))?;
        return Ok(());
    }

    let rows: Vec<_> = wallets
        .iter()
        .map(|wallet| {
            vec![
                if wallet.is_default {
                    format!("{} (default)", wallet.name)
                } else {
                    wallet.name.clone()
                },
                wallet.address.clone(),
                if wallet.owner_public_key.is_empty() {
                    "Not recorded".into()
                } else {
                    wallet.owner_public_key.clone()
                },
            ]
        })
        .collect();
    print.data_table(
        &format!("Wallets: {}", profile.name),
        &["Name".into(), "Address".into(), "Owner".into()],
        &rows,
        "No wallets saved.",
        wallets.iter().map(|wallet| {
            let marker = if wallet.is_default { "*" } else { " " };
            format!(
                "{marker} {} {} owner {}",
                wallet.name, wallet.address, wallet.owner_public_key
            )
        }),
    )
}

pub fn use_wallet(
    name: &str,
    profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let name = name.trim();
    let cwd = std::env::current_dir()?;
    let project_root = find_project_config(&cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);
    let resolved_profile = resolve_profile(&project_root, profile)?;
    let key_name =
        resolve_default_key_for_profile(&resolved_profile)?.ok_or(CliError::MissingSigningKey)?;
    let public_key =
        load_local_public_key(&key_name)?.ok_or_else(|| CliError::KeyNotFound(key_name.clone()))?;
    resolve_selected_wallet(&resolved_profile, &key_name, public_key, name)?;
    save_profile_default_wallet(&project_root, Some(&resolved_profile.name), name)?;

    print.group(crate::print::Outcome::Success, "Wallet selected", |print| {
        print.status(format_args!("Using wallet `{name}` by default."));
    });
    print.json(&CommandResult::new(
        cmd,
        json!({
            "name": name,
            "profile": resolved_profile.name,
        }),
    ))?;
    Ok(())
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
