use crate::keys_json::{
    ExportedKey, ImportedKey, KEYS_EXPORT_JSON_SCHEMA_VERSION, KEYS_IMPORT_JSON_SCHEMA_VERSION,
    KeysExportReport, KeysImportReport,
};
use crate::result::CommandResult;
use crate::{CliError, Print};
use groundhog_json_rpc_types::hex::{decode_fixed_hex, prefixed_hex};
use groundhog_signing::strkey;
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

const DEFAULT_KEY_NAME: &str = "default";
const DEFAULT_KEY_ID: u32 = 0;
const MAX_SEED_INPUT_BYTES: usize = 128;

#[derive(Serialize, Deserialize)]
struct KeyFile {
    version: u32,
    scheme: String,
    keypair_hex: String,
    #[serde(default)]
    public_key_hex: Option<String>,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        self.keypair_hex.zeroize();
    }
}

#[derive(Debug, Serialize)]
struct KeyInfo {
    name: String,
    is_default: bool,
    key_id: u32,
    public_key: String,
    public_key_strkey: String,
    address: String,
    path: PathBuf,
}

/// Public summary of a stored signing key.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
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
    validate_stored_key_name(name)?;

    let path = key_path(name)?;
    if path.exists() {
        return Err(CliError::KeyExists(name.to_string()));
    }

    if !print.has_human_ui() {
        print.status(format_args!("Generating key `{name}`"));
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| CliError::Other(e.to_string()))?;
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

    print.group(crate::print::Outcome::Success, "Key generated", |print| {
        print.success(format_args!("Generated key `{name}`"));
        print.detail("public key", &info.public_key_strkey);
        print.detail("account address", &info.address);
        if fund {
            print.note("Faucet funding is not implemented yet; generated key only");
        }
        if became_default {
            print.note(format_args!("Default key is now `{name}`"));
        }
    });
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

/// Import an S `StrKey` or 32-byte hex seed from a hidden terminal prompt or piped stdin.
/// No secret command-line argument is accepted and existing keys are never overwritten.
pub fn import(name: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    validate_stored_key_name(name)?;
    if key_path(name)?.exists() {
        return Err(CliError::KeyExists(name.to_owned()));
    }
    let input = if std::io::stdin().is_terminal() {
        print.read_secret("Ed25519 secret seed (S StrKey or hex): ")?
    } else {
        // Bound untrusted piped input before allocating or decoding it.
        let mut input = Zeroizing::new(String::new());
        std::io::stdin()
            .lock()
            .take(MAX_SEED_INPUT_BYTES as u64 + 1)
            .read_to_string(&mut input)
            .map_err(|_| CliError::InvalidSecretSeed)?;
        input
    };
    if input.len() > MAX_SEED_INPUT_BYTES {
        return Err(CliError::InvalidSecretSeed);
    }
    let seed = strkey::parse_secret_seed(input.trim()).map_err(|_| CliError::InvalidSecretSeed)?;
    let signer = LocalSigner::from_secret_key(*seed);
    let info = write_key(name, &signer)?;
    let (is_default, warnings) = import_default_selection(name);
    if print.has_human_ui() {
        print.outcome_findings(
            if warnings.is_empty() {
                crate::print::Outcome::Success
            } else {
                crate::print::Outcome::Partial
            },
            "Key imported",
            &[
                ("Name".into(), name.into()),
                ("Public key".into(), info.public_key_strkey.clone()),
                ("Account address".into(), info.address.clone()),
                (
                    "Default".into(),
                    match is_default {
                        Some(true) => "Selected",
                        Some(false) => "Not selected",
                        None => "Unconfirmed",
                    }
                    .into(),
                ),
            ],
            &warnings,
        );
    } else {
        for warning in &warnings {
            print.warn(&warning.message);
            if let Some(hint) = &warning.hint {
                print.note(hint);
            }
        }
        print.success(format_args!("Imported key `{name}`"));
        print.detail("public key", &info.public_key_strkey);
        print.detail("account address", &info.address);
    }
    let report = KeysImportReport {
        schema_version: KEYS_IMPORT_JSON_SCHEMA_VERSION,
        key: ImportedKey {
            name: info.name,
            is_default,
            key_id: info.key_id,
            public_key: info.public_key,
            public_key_strkey: info.public_key_strkey,
            address: info.address,
            path: info.path,
        },
    };
    print.json(&CommandResult::with_warnings(cmd, warnings, report))?;
    Ok(())
}

fn import_default_selection(name: &str) -> (Option<bool>, Vec<crate::checks::Finding>) {
    match crate::config::save_default_key_if_unset(name) {
        Ok(selected) => (Some(selected), vec![]),
        Err(error) => {
            let selected = crate::config::load_default_key()
                .map(|current| current.as_deref() == Some(name))
                .ok();
            let finding = crate::checks::Finding {
                level: crate::checks::FindingLevel::Warn,
                code: "key_import_default_selection_failed",
                message: format!(
                    "Key `{name}` was imported, but default selection could not be confirmed ({})",
                    error.code(),
                ),
                hint: Some(
                    format!(
                        "After fixing configuration, run `thruput keys use {name}`; \
                         do not import the key again"
                    )
                    .into(),
                ),
            };
            (selected, vec![finding])
        }
    }
}

/// List stored signing keys.
pub fn list(cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let keys = load_all_keys()?;

    if print.is_json() {
        print.json(&CommandResult::new(cmd, json!({ "keys": keys })))?;
        return Ok(());
    }

    let rows: Vec<_> = keys
        .iter()
        .map(|key| {
            vec![
                if key.is_default {
                    format!("{} (default)", key.name)
                } else {
                    key.name.clone()
                },
                key.address.clone(),
                key.public_key_strkey.clone(),
            ]
        })
        .collect();
    print.data_table(
        "Keys",
        &["Name".into(), "Account address".into(), "Public key".into()],
        &rows,
        "No keys stored.",
        keys.iter().map(|key| {
            let marker = if key.is_default { "*" } else { " " };
            format!(
                "{marker} {} public-key={} account-address={}",
                key.name, key.public_key_strkey, key.address
            )
        }),
    )
}

/// Export a stored private key after interactive confirmation or `force` authorization.
pub fn export(name: &str, force: bool, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    export_with_format(name, force, KeyExportFormat::Hex, cmd, print)
}

/// Encoding explicitly selected for private seed export. Hex remains the compatibility default.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum KeyExportFormat {
    /// A 32-byte lowercase 0x-prefixed seed.
    #[default]
    Hex,
    /// A checksummed Stellar S `StrKey` containing the same seed.
    Strkey,
}

fn seed_strkey(keypair: &[u8; 64]) -> String {
    let seed = Zeroizing::new(
        <[u8; 32]>::try_from(&keypair[..32]).expect("bug: validated keypair contains a seed"),
    );
    let mut encoded = strkey::format_secret_seed(&seed);
    std::mem::take(&mut *encoded)
}

/// Export a stored seed in a caller-selected encoding after the existing confirmation gate.
pub fn export_with_format(
    name: &str,
    force: bool,
    format: KeyExportFormat,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    validate_key_name(name)?;
    let path = key_path(name)?;
    if !path.exists() {
        return Err(CliError::KeyNotFound(name.to_string()));
    }
    if !print.confirm(
        force,
        "export a private signing key",
        format_args!("Export private key `{name}` to standard output? Treat it as a secret."),
    )? {
        print.cancelled();
        return Ok(());
    }

    // Do not load secret key material until export has been authorized.
    validate_key_name(name)?;
    let keypair_bytes = load_stored_keypair_bytes(name).map_err(|error| match error {
        CliError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            CliError::KeyNotFound(name.into())
        }
        error => error,
    })?;
    LocalSigner::from_keypair_bytes(*keypair_bytes).map_err(|error| signing_error(&error))?;
    let report = KeysExportReport {
        schema_version: KEYS_EXPORT_JSON_SCHEMA_VERSION,
        key: ExportedKey {
            name: name.to_string(),
            scheme: "ed25519".into(),
            private_key: match format {
                KeyExportFormat::Hex => prefixed_hex(&keypair_bytes[..32]),
                KeyExportFormat::Strkey => seed_strkey(&keypair_bytes),
            },
        },
    };

    if print.is_json() {
        print.json(&CommandResult::new(cmd, &report))?;
    } else {
        println!("{}", report.key.private_key);
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

    print.group(
        crate::print::Outcome::Success,
        "Default key selected",
        |print| {
            print.success(format_args!("Default key set to `{name}`"));
        },
    );
    print.json(&CommandResult::new(cmd, json!({ "key": info })))?;
    Ok(())
}

/// Remove a stored signing key after interactive confirmation or `force` authorization.
pub fn remove(name: &str, force: bool, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    validate_key_name(name)?;
    let path = key_path(name)?;
    if !path.exists() {
        return Err(CliError::KeyNotFound(name.to_string()));
    }

    if !print.confirm(
        force,
        "remove a stored signing key",
        format_args!("Remove key `{name}`? This cannot be undone."),
    )? {
        print.cancelled();
        return Ok(());
    }

    fs::remove_file(&path)?;

    let cleared_default = crate::config::load_default_key()?.as_deref() == Some(name);
    if cleared_default {
        crate::config::clear_default_key()?;
    }

    print.group(crate::print::Outcome::Success, "Key removed", |print| {
        print.success(format_args!("Removed key `{name}`"));
        if cleared_default {
            print.note("Default key selection cleared");
        }
    });
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

fn validate_stored_key_name(name: &str) -> Result<(), CliError> {
    validate_key_name(name)?;
    Ok(())
}

fn write_key(name: &str, signing_key: &LocalSigner) -> Result<KeyInfo, CliError> {
    // Validate configuration before committing a key; never reread it after the write.
    let default_key = crate::config::load_default_key()?;
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

    let contents = Zeroizing::new(
        serde_json::to_vec_pretty(&keyfile).map_err(|e| CliError::Other(e.to_string()))?,
    );

    let parent = path.parent().expect("bug: key path has a parent directory");
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&contents)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    staged.persist_noclobber(&path).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            CliError::KeyExists(name.to_owned())
        } else {
            CliError::Io(error.error)
        }
    })?;
    Ok(KeyInfo {
        name: name.to_owned(),
        is_default: default_key.as_deref() == Some(name),
        key_id: DEFAULT_KEY_ID,
        public_key: prefixed_hex(public_bytes),
        public_key_strkey: strkey::format_public_key(&public_bytes),
        address: prefixed_hex(derive_pubkey_account_address(public_bytes)),
        path,
    })
}

fn load_key_info(name: &str) -> Result<KeyInfo, CliError> {
    let path = key_path(name)?;
    let keyfile = read_keyfile(name)?;

    let public_key = keyfile_public_key(&keyfile)?;

    let address = derive_pubkey_account_address(public_key);
    let default_key = crate::config::load_default_key()?;

    Ok(KeyInfo {
        name: name.to_string(),
        is_default: default_key.as_deref() == Some(name),
        key_id: DEFAULT_KEY_ID,
        public_key: prefixed_hex(public_key),
        public_key_strkey: strkey::format_public_key(&public_key),
        address: prefixed_hex(address),
        path,
    })
}

/// Load a named key's public metadata, returning `None` for an absent or invalid name.
/// Legacy files without cached public metadata derive it from their stored keypair.
pub(crate) fn load_local_public_key(name: &str) -> Result<Option<[u8; 32]>, CliError> {
    if validate_key_name(name).is_err() {
        return Ok(None);
    }
    let keyfile = match read_keyfile(name) {
        Ok(keyfile) => keyfile,
        Err(CliError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    keyfile_public_key(&keyfile).map(Some)
}

fn keyfile_public_key(keyfile: &KeyFile) -> Result<[u8; 32], CliError> {
    if keyfile.scheme != "ed25519" {
        return Err(CliError::Other(format!(
            "unsupported key scheme: {}",
            keyfile.scheme
        )));
    }

    if let Some(ref pk_hex) = keyfile.public_key_hex {
        parse_stored_hex::<32>(pk_hex)
    } else {
        // Legacy key without public_key_hex — must load keypair.
        let keypair_bytes = zeroize::Zeroizing::new(parse_stored_hex::<64>(&keyfile.keypair_hex)?);
        let signing_key = LocalSigner::from_keypair_bytes(*keypair_bytes)
            .map_err(|error| signing_error(&error))?;
        Ok(signing_key.verifying_key_bytes())
    }
}

/// Load a stored signing key as a [`LocalSigner`].
///
/// Private key material is zeroized from the temporary keypair buffer
/// after construction. Used by deploy and call to sign transactions.
pub fn load_local_signer(name: &str) -> Result<LocalSigner, CliError> {
    validate_key_name(name)?;
    let keypair_bytes = load_stored_keypair_bytes(name).map_err(|error| match error {
        CliError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            CliError::KeyNotFound(name.into())
        }
        error => error,
    })?;
    LocalSigner::from_keypair_bytes(*keypair_bytes).map_err(|error| signing_error(&error))
}

fn load_stored_keypair_bytes(name: &str) -> Result<Zeroizing<[u8; 64]>, CliError> {
    let keyfile = read_keyfile(name)?;
    if keyfile.scheme != "ed25519" {
        return Err(CliError::Other(format!(
            "unsupported key scheme: {}",
            keyfile.scheme
        )));
    }

    Ok(Zeroizing::new(parse_stored_hex::<64>(
        &keyfile.keypair_hex,
    )?))
}

/// Write an isolated harness-owned key without changing user configuration.
pub(crate) fn write_fixture_key(
    directory: &Path,
    name: &str,
    signer: &LocalSigner,
) -> Result<(), CliError> {
    validate_key_name(name)?;
    fs::create_dir_all(directory)?;
    let keypair = Zeroizing::new(signer.to_keypair_bytes());
    let key = KeyFile {
        version: 1,
        scheme: "ed25519".into(),
        keypair_hex: hex::encode(keypair.as_ref()),
        public_key_hex: Some(hex::encode(signer.verifying_key_bytes())),
    };
    let contents = Zeroizing::new(
        serde_json::to_vec(&key)
            .map_err(|error| CliError::LocalFundingAttempt(error.to_string()))?,
    );
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(directory.join(format!("{name}.json")))
        .map_err(|error| CliError::Io(error.error))?;
    Ok(())
}

fn read_keyfile(name: &str) -> Result<KeyFile, CliError> {
    let contents = Zeroizing::new(fs::read_to_string(key_path(name)?)?);
    serde_json::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))
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

fn derive_pubkey_account_address(public_key: [u8; 32]) -> [u8; 32] {
    let address =
        PubkeyAccount::from_public_key_bytes(public_key, DEFAULT_PUBKEY_ACCOUNT_NONCE).address();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(address.as_ref());
    bytes
}

fn parse_stored_hex<const N: usize>(input: &str) -> Result<[u8; N], CliError> {
    decode_fixed_hex(input).map_err(|error| CliError::Other(format!("invalid stored key: {error}")))
}

fn signing_error(error: &groundhog_signing::SigningError) -> CliError {
    CliError::Other(error.to_string())
}
