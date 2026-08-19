use crate::result::CommandResult;
use crate::{CliError, Print};
use getrandom::getrandom;
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use zeroize::Zeroize;

const DEFAULT_KEY_NAME: &str = "default";
const DEFAULT_KEY_ID: u32 = 0;

#[derive(Debug, Serialize, Deserialize)]
struct KeyFile {
    version: u32,
    scheme: String,
    keypair_hex: String,
    #[serde(default)]
    public_key_hex: Option<String>,
}

#[derive(Debug, Serialize)]
struct KeyInfo {
    name: String,
    is_default: bool,
    key_id: u32,
    public_key: String,
    address: String,
    path: PathBuf,
}

/// Public summary of a stored signing key.
#[derive(Debug, Clone, Serialize)]
pub struct KeySummary {
    pub name: String,
    pub address: String,
    pub is_default: bool,
}

/// Generate a new signing keypair.
pub fn generate(
    name: &Option<String>,
    fund: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let name = name.as_deref().unwrap_or(DEFAULT_KEY_NAME);
    validate_key_name(name)?;

    let path = key_path(name)?;
    if path.exists() {
        return Err(CliError::KeyExists(name.to_string()));
    }

    let task = print.start_task(format_args!("Generating key `{name}`"));
    let mut secret = [0u8; 32];
    getrandom(&mut secret).map_err(|e| CliError::Other(e.to_string()))?;
    let signing_key = LocalSigner::from_secret_key(secret);
    secret.zeroize();
    let mut info = write_key(name, &signing_key)?;

    let default_key = crate::config::load_default_key()?;
    let became_default = if default_key.is_none() {
        crate::config::save_default_key(name)?;
        true
    } else {
        default_key.as_deref() == Some(name)
    };
    info.is_default = became_default;

    task.success(format_args!("Generated key `{name}`"));
    print.detail("address", &info.address);
    if fund {
        print.note("Faucet funding is not implemented yet; generated key only");
    }
    if became_default {
        print.note(format_args!("Default key is now `{name}`"));
    }
    print.json(&CommandResult::new(
        cmd,
        json!({
            "key": info,
            "fund_requested": fund,
            "funded": false,
        }),
    ))?;
    Ok(())
}

/// List stored signing keys.
pub fn list(cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let keys = load_all_keys()?;

    if print.is_json() {
        print.json(&CommandResult::new(cmd, json!({ "keys": keys })))?;
        return Ok(());
    }

    if keys.is_empty() {
        println!("No keys stored.");
        return Ok(());
    }

    for key in keys {
        let marker = if key.is_default { "*" } else { " " };
        println!("{marker} {} {}", key.name, key.address);
    }

    Ok(())
}

/// Load stored signing keys as lightweight public summaries.
pub fn load_key_summaries() -> Result<Vec<KeySummary>, CliError> {
    load_all_keys().map(|keys| keys.into_iter().map(Into::into).collect())
}

/// Set the default signing key.
pub fn use_key(name: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    validate_key_name(name)?;
    let path = key_path(name)?;
    if !path.exists() {
        return Err(CliError::KeyNotFound(name.to_string()));
    }
    let info = load_key_info(name)?;
    crate::config::save_default_key(name)?;
    let info = KeyInfo {
        is_default: true,
        ..info
    };

    print.success(format_args!("Default key set to `{name}`"));
    print.json(&CommandResult::new(cmd, json!({ "key": info })))?;
    Ok(())
}

/// Remove a stored signing key.
///
/// JSON and quiet callers retain the legacy non-interactive behavior. New CLI
/// dispatch should use [`remove_with_force`] so deletion authority is explicit.
pub fn remove(name: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let legacy_non_interactive_bypass = print.is_json() || print.is_quiet();
    remove_with_force(name, legacy_non_interactive_bypass, cmd, print)
}

/// Remove a stored signing key, optionally bypassing confirmation.
pub fn remove_with_force(
    name: &str,
    force: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    validate_key_name(name)?;
    let path = key_path(name)?;
    if !path.exists() {
        return Err(CliError::KeyNotFound(name.to_string()));
    }

    if !force && !print.can_prompt() {
        return Err(CliError::ConfirmationRequired {
            action: crate::error::REMOVE_KEY_CONFIRMATION_ACTION,
        });
    }

    if !force {
        print.prompt(format_args!(
            "Remove key `{name}`? This cannot be undone. [y/N] "
        ))?;
        let mut answer = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(CliError::Io)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            print.note("Cancelled");
            return Ok(());
        }
    }

    fs::remove_file(&path)?;

    let cleared_default = crate::config::load_default_key()?.as_deref() == Some(name);
    if cleared_default {
        crate::config::clear_default_key()?;
    }

    print.success(format_args!("Removed key `{name}`"));
    if cleared_default {
        print.note("Default key selection cleared");
    }
    print.json(&CommandResult::new(
        cmd,
        json!({
            "name": name,
            "cleared_default": cleared_default,
        }),
    ))?;
    Ok(())
}

fn validate_key_name(name: &str) -> Result<(), CliError> {
    if name.is_empty() {
        return Err(CliError::KeyNameEmpty);
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CliError::KeyNameInvalid);
    }

    Ok(())
}

fn write_key(name: &str, signing_key: &LocalSigner) -> Result<KeyInfo, CliError> {
    let path = key_path(name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let keypair_bytes = zeroize::Zeroizing::new(signing_key.to_keypair_bytes());
    let public_bytes = signing_key.verifying_key_bytes();
    let keyfile = KeyFile {
        version: 1,
        scheme: "ed25519".into(),
        keypair_hex: hex::encode(keypair_bytes.as_ref()),
        public_key_hex: Some(hex::encode(public_bytes)),
    };

    let contents =
        serde_json::to_vec_pretty(&keyfile).map_err(|e| CliError::Other(e.to_string()))?;

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts.open(&path)?;
    file.write_all(&contents)?;
    file.write_all(b"\n")?;

    load_key_info(name)
}

fn load_key_info(name: &str) -> Result<KeyInfo, CliError> {
    let path = key_path(name)?;
    let contents = fs::read_to_string(&path)?;
    let keyfile: KeyFile =
        serde_json::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;

    if keyfile.scheme != "ed25519" {
        return Err(CliError::Other(format!(
            "unsupported key scheme: {}",
            keyfile.scheme
        )));
    }

    let public_key = if let Some(ref pk_hex) = keyfile.public_key_hex {
        parse_hex_32(pk_hex)?
    } else {
        // Legacy key without public_key_hex — must load keypair.
        let keypair_bytes = zeroize::Zeroizing::new(parse_hex_64(&keyfile.keypair_hex)?);
        let signing_key = LocalSigner::from_keypair_bytes(*keypair_bytes)
            .map_err(|error| signing_error(&error))?;
        signing_key.verifying_key_bytes()
    };

    let address = derive_pubkey_account_address(public_key)?;
    let default_key = crate::config::load_default_key()?;

    Ok(KeyInfo {
        name: name.to_string(),
        is_default: default_key.as_deref() == Some(name),
        key_id: DEFAULT_KEY_ID,
        public_key: hex_prefixed(&public_key),
        address: hex_prefixed(&address),
        path,
    })
}

/// Load a stored signing key as a [`LocalSigner`].
///
/// Private key material is zeroized from the temporary keypair buffer
/// after construction. Used by deploy and call to sign transactions.
pub fn load_local_signer(name: &str) -> Result<LocalSigner, CliError> {
    if crate::dev_keys::is_dev_key(name) {
        return crate::dev_keys::load_dev_signer(name);
    }

    let contents = fs::read_to_string(key_path(name)?)?;
    let keyfile: KeyFile =
        serde_json::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;

    if keyfile.scheme != "ed25519" {
        return Err(CliError::Other(format!(
            "unsupported key scheme: {}",
            keyfile.scheme
        )));
    }

    let keypair_bytes = zeroize::Zeroizing::new(parse_hex_64(&keyfile.keypair_hex)?);
    LocalSigner::from_keypair_bytes(*keypair_bytes).map_err(|error| signing_error(&error))
}

fn load_all_keys() -> Result<Vec<KeyInfo>, CliError> {
    let dir = crate::config::keys_dir()?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.into()),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) {
            names.push(name.to_string());
        }
    }
    names.sort();

    names.into_iter().map(|name| load_key_info(&name)).collect()
}

impl From<KeyInfo> for KeySummary {
    fn from(info: KeyInfo) -> Self {
        Self {
            name: info.name,
            address: info.address,
            is_default: info.is_default,
        }
    }
}

fn key_path(name: &str) -> Result<PathBuf, CliError> {
    Ok(crate::config::keys_dir()?.join(format!("{name}.json")))
}

fn derive_pubkey_account_address(public_key: [u8; 32]) -> Result<[u8; 32], CliError> {
    let address = PubkeyAccount::from_public_key_bytes(public_key, DEFAULT_PUBKEY_ACCOUNT_NONCE)
        .address()
        .map_err(|error| signing_error(&error))?;
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(address.as_ref());
    Ok(bytes)
}

fn parse_hex_32(input: &str) -> Result<[u8; 32], CliError> {
    let stripped = input.strip_prefix("0x").unwrap_or(input);
    let bytes = hex::decode(stripped).map_err(|e| CliError::Other(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| CliError::Other("invalid stored public key length".into()))
}

fn parse_hex_64(input: &str) -> Result<[u8; 64], CliError> {
    let stripped = input.strip_prefix("0x").unwrap_or(input);
    let bytes = hex::decode(stripped).map_err(|e| CliError::Other(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| CliError::Other("invalid stored key length".into()))
}

fn hex_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn signing_error(error: &groundhog_signing::SigningError) -> CliError {
    CliError::Other(error.to_string())
}
