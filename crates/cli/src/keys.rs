use crate::keys_json::{
    ExportedKey, ImportedKey, KEYS_EXPORT_JSON_SCHEMA_VERSION, KEYS_IMPORT_JSON_SCHEMA_VERSION,
    KeysExportReport, KeysImportReport,
};
use crate::result::CommandResult;
use crate::{CliError, Print};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use thruput_json_rpc_types::hex::{decode_fixed_hex, prefixed_hex};
use thruput_signing::strkey;
use thruput_signing::{
    DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, MLDSA65_PUBLIC_KEY_LEN, MLDSA65_SEED_LEN,
    MlDsa65Signer, PubkeyAccount, SchemeSigner, Signer as _,
};
use thruput_xdr::types::{Ed25519PublicKey, MlDsa65PublicKey, PublicKey};
use zeroize::{Zeroize, Zeroizing};

const DEFAULT_KEY_NAME: &str = "default";
const DEFAULT_KEY_ID: u32 = 0;
const MAX_SEED_INPUT_BYTES: usize = 128;

const ED25519_SCHEME: &str = "ed25519";
const MLDSA65_SCHEME: &str = "mldsa65";

/// Signature scheme of a stored key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum KeyScheme {
    /// Ed25519; the key file stores the 64-byte keypair.
    #[default]
    Ed25519,
    /// ML-DSA-65 (FIPS 204); the key file stores the 32-byte key-generation seed.
    #[value(name = "mldsa65")]
    MlDsa65,
}

/// On-disk key file. `scheme` selects which secret field is present: Ed25519
/// files hold `keypair_hex`, ML-DSA-65 files hold the FIPS 204 `seed_hex`,
/// never an expanded key. Files are written with mode 0600.
#[derive(Serialize, Deserialize)]
struct KeyFile {
    version: u32,
    scheme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keypair_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed_hex: Option<String>,
    #[serde(default)]
    public_key_hex: Option<String>,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        self.keypair_hex.zeroize();
        self.seed_hex.zeroize();
    }
}

#[derive(Debug, Serialize)]
struct KeyInfo {
    name: String,
    is_default: bool,
    key_id: u32,
    scheme: &'static str,
    public_key: String,
    /// G `StrKey`; Ed25519 only, as no `StrKey` format exists for ML-DSA keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    public_key_strkey: Option<String>,
    /// Hash naming the key in its account's address and auth claim.
    public_key_hash: String,
    address: String,
    path: PathBuf,
}

impl KeyInfo {
    fn new(name: &str, is_default: bool, public_key: &PublicKey, path: PathBuf) -> Self {
        let account = PubkeyAccount::new(public_key.clone(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
        let (scheme, public_key_bytes) = public_key_scheme_and_bytes(public_key);
        let public_key_strkey =
            ed25519_public_key_bytes(public_key).map(|bytes| strkey::format_public_key(&bytes));
        Self {
            name: name.to_owned(),
            is_default,
            key_id: DEFAULT_KEY_ID,
            scheme,
            public_key: prefixed_hex(public_key_bytes),
            public_key_strkey,
            public_key_hash: prefixed_hex(account.public_key_hash().as_ref()),
            address: prefixed_hex(account.address().as_ref()),
            path,
        }
    }

    /// Short public identity for human output: the G `StrKey` for Ed25519, or
    /// the scheme and key hash for keys without a `StrKey` format.
    fn public_key_label(&self) -> String {
        self.public_key_strkey
            .clone()
            .unwrap_or_else(|| format!("{} key hash {}", self.scheme, self.public_key_hash))
    }
}

/// Public summary of a stored signing key.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySummary {
    pub name: String,
    pub address: String,
    pub is_default: bool,
}

/// Generate a new signing key of the given scheme.
pub fn generate(
    name: &Option<String>,
    scheme: KeyScheme,
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
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(secret.as_mut()).map_err(|e| CliError::Other(e.to_string()))?;
    let signing_key = match scheme {
        KeyScheme::Ed25519 => SchemeSigner::from(LocalSigner::from_secret_key(*secret)),
        KeyScheme::MlDsa65 => SchemeSigner::from(MlDsa65Signer::from_seed(&secret)),
    };
    drop(secret);
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
        print.success(format_args!("Generated {} key `{name}`", info.scheme));
        print.detail("public key", info.public_key_label());
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
    let signer = SchemeSigner::from(LocalSigner::from_secret_key(*seed));
    let info = write_key(name, &signer)?;
    let public_key_strkey = info
        .public_key_strkey
        .clone()
        .expect("bug: imported keys are Ed25519 and have a G StrKey");
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
                ("Public key".into(), public_key_strkey.clone()),
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
        print.detail("public key", &public_key_strkey);
        print.detail("account address", &info.address);
    }
    let report = KeysImportReport {
        schema_version: KEYS_IMPORT_JSON_SCHEMA_VERSION,
        key: ImportedKey {
            name: info.name,
            is_default,
            key_id: info.key_id,
            public_key: info.public_key,
            public_key_strkey,
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
                key.public_key_label(),
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
                key.name,
                key.public_key_label(),
                key.address
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

fn seed_strkey(seed: &[u8; 32]) -> String {
    let mut encoded = strkey::format_secret_seed(seed);
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
    let signer = load_local_signer(name)?;
    let (scheme, seed) = match &signer {
        SchemeSigner::Ed25519(signer) => {
            let keypair = Zeroizing::new(signer.to_keypair_bytes());
            let mut seed = Zeroizing::new([0; 32]);
            seed.copy_from_slice(&keypair[..32]);
            (ED25519_SCHEME, seed)
        }
        SchemeSigner::MlDsa65(signer) => (MLDSA65_SCHEME, signer.to_seed()),
        _ => unreachable!("bug: key files only hold supported schemes"),
    };
    let private_key = match format {
        KeyExportFormat::Hex => prefixed_hex(seed.as_ref()),
        KeyExportFormat::Strkey if scheme == ED25519_SCHEME => seed_strkey(&seed),
        KeyExportFormat::Strkey => {
            return Err(CliError::KeySchemeUnsupported {
                key_name: name.to_owned(),
                scheme: scheme.to_owned(),
                operation: "S StrKey export".to_owned(),
            });
        }
    };
    let report = KeysExportReport {
        schema_version: KEYS_EXPORT_JSON_SCHEMA_VERSION,
        key: ExportedKey {
            name: name.to_string(),
            scheme: scheme.into(),
            private_key,
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

fn write_key(name: &str, signing_key: &SchemeSigner) -> Result<KeyInfo, CliError> {
    // Validate configuration before committing a key; never reread it after the write.
    let default_key = crate::config::load_default_key()?;
    let path = key_path(name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let keyfile = keyfile_for(signing_key);
    let contents = Zeroizing::new(
        serde_json::to_vec_pretty(&keyfile).map_err(|e| CliError::Other(e.to_string()))?,
    );

    let parent = path.parent().expect("bug: key path has a parent directory");
    // Temporary files are created with mode 0600 and keep it when persisted.
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
    Ok(KeyInfo::new(
        name,
        default_key.as_deref() == Some(name),
        &signing_key.public_key(),
        path,
    ))
}

/// Serialize a signer's secret in its scheme's stored form, with cached public metadata.
fn keyfile_for(signing_key: &SchemeSigner) -> KeyFile {
    match signing_key {
        SchemeSigner::Ed25519(signer) => ed25519_keyfile(signer),
        SchemeSigner::MlDsa65(signer) => KeyFile {
            version: 2,
            scheme: MLDSA65_SCHEME.into(),
            keypair_hex: None,
            seed_hex: Some(hex::encode(signer.to_seed().as_ref())),
            public_key_hex: Some(hex::encode(signer.public_key_bytes())),
        },
        _ => unreachable!("bug: key files only hold supported schemes"),
    }
}

fn ed25519_keyfile(signer: &LocalSigner) -> KeyFile {
    let keypair_bytes = Zeroizing::new(signer.to_keypair_bytes());
    KeyFile {
        version: 1,
        scheme: ED25519_SCHEME.into(),
        keypair_hex: Some(hex::encode(keypair_bytes.as_ref())),
        seed_hex: None,
        public_key_hex: Some(hex::encode(signer.verifying_key_bytes())),
    }
}

fn load_key_info(name: &str) -> Result<KeyInfo, CliError> {
    let path = key_path(name)?;
    let keyfile = read_keyfile(name)?;
    let public_key = keyfile_public_key(&keyfile)?;
    let default_key = crate::config::load_default_key()?;
    Ok(KeyInfo::new(
        name,
        default_key.as_deref() == Some(name),
        &public_key,
        path,
    ))
}

/// Load a named key's public key, returning `None` for an absent or invalid name.
/// Legacy files without cached public metadata derive it from their stored secret.
pub(crate) fn load_local_public_key(name: &str) -> Result<Option<PublicKey>, CliError> {
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

/// The stored scheme name and raw encoded bytes of a public key.
pub(crate) fn public_key_scheme_and_bytes(public_key: &PublicKey) -> (&'static str, &[u8]) {
    match public_key {
        PublicKey::Ed25519 { ed25519 } => (ED25519_SCHEME, ed25519.as_ref()),
        PublicKey::MlDsa65 { mldsa65 } => (MLDSA65_SCHEME, mldsa65.as_ref().as_ref()),
        _ => unreachable!("bug: key files only hold supported schemes"),
    }
}

/// The raw Ed25519 key bytes, or `None` for keys of other schemes.
///
/// Saved wallet bindings and genesis allocations name owners by Ed25519 key.
pub(crate) fn ed25519_public_key_bytes(public_key: &PublicKey) -> Option<[u8; 32]> {
    match public_key {
        PublicKey::Ed25519 { ed25519 } => Some(ed25519.into_inner()),
        _ => None,
    }
}

fn keyfile_public_key(keyfile: &KeyFile) -> Result<PublicKey, CliError> {
    match (keyfile.scheme.as_str(), &keyfile.public_key_hex) {
        (ED25519_SCHEME, Some(pk_hex)) => Ok(PublicKey::Ed25519 {
            ed25519: Ed25519PublicKey::new(parse_stored_hex::<32>(pk_hex)?),
        }),
        (MLDSA65_SCHEME, Some(pk_hex)) => Ok(PublicKey::MlDsa65 {
            mldsa65: MlDsa65PublicKey::new(parse_stored_hex::<MLDSA65_PUBLIC_KEY_LEN>(pk_hex)?)
                .into(),
        }),
        // Without cached public metadata the secret must be loaded.
        _ => Ok(signer_from_keyfile(keyfile)?.public_key()),
    }
}

/// Load a stored signing key of any supported scheme.
///
/// Private key material is zeroized from temporary buffers after
/// construction. Used by deploy, call, and transfer to sign transactions.
pub fn load_local_signer(name: &str) -> Result<SchemeSigner, CliError> {
    validate_key_name(name)?;
    let keyfile = read_keyfile(name).map_err(|error| match error {
        CliError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            CliError::KeyNotFound(name.into())
        }
        error => error,
    })?;
    signer_from_keyfile(&keyfile)
}

fn signer_from_keyfile(keyfile: &KeyFile) -> Result<SchemeSigner, CliError> {
    let missing = || CliError::Other("invalid stored key: missing secret".into());
    let signer = match keyfile.scheme.as_str() {
        ED25519_SCHEME => {
            let keypair_hex = keyfile.keypair_hex.as_deref().ok_or_else(missing)?;
            let keypair_bytes = Zeroizing::new(parse_stored_hex::<64>(keypair_hex)?);
            SchemeSigner::from(
                LocalSigner::from_keypair_bytes(*keypair_bytes)
                    .map_err(|error| signing_error(&error))?,
            )
        }
        MLDSA65_SCHEME => {
            let seed_hex = keyfile.seed_hex.as_deref().ok_or_else(missing)?;
            let seed = Zeroizing::new(parse_stored_hex::<MLDSA65_SEED_LEN>(seed_hex)?);
            SchemeSigner::from(MlDsa65Signer::from_seed(&seed))
        }
        other => return Err(CliError::Other(format!("unsupported key scheme: {other}"))),
    };
    Ok(signer)
}

/// Write an isolated harness-owned key without changing user configuration.
pub(crate) fn write_fixture_key(
    directory: &Path,
    name: &str,
    signer: &LocalSigner,
) -> Result<(), CliError> {
    validate_key_name(name)?;
    fs::create_dir_all(directory)?;
    let key = ed25519_keyfile(signer);
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

fn parse_stored_hex<const N: usize>(input: &str) -> Result<[u8; N], CliError> {
    decode_fixed_hex(input).map_err(|error| CliError::Other(format!("invalid stored key: {error}")))
}

fn signing_error(error: &thruput_signing::SigningError) -> CliError {
    CliError::Other(error.to_string())
}
