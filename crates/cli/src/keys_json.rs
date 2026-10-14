use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub(crate) const KEYS_EXPORT_JSON_SCHEMA_VERSION: u32 = 1;

/// Stable machine-readable result for `thruput keys export`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeysExportReport {
    pub(crate) schema_version: u32,
    pub(crate) key: ExportedKey,
}

/// A stored key's private seed, exported for use with a client signer.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportedKey {
    pub(crate) name: String,
    pub(crate) scheme: String,
    pub(crate) private_key: String,
}

impl Drop for ExportedKey {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
}

/// First version of the public import result.
pub(crate) const KEYS_IMPORT_JSON_SCHEMA_VERSION: u32 = 1;

/// Stable machine-readable result for `thruput keys import`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeysImportReport {
    pub(crate) schema_version: u32,
    pub(crate) key: ImportedKey,
}

/// Public information about the imported key; never includes the seed.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedKey {
    pub(crate) name: String,
    /// None means configuration could not be reread after a default-selection error.
    pub(crate) is_default: Option<bool>,
    pub(crate) key_id: u32,
    pub(crate) public_key: String,
    pub(crate) public_key_strkey: String,
    pub(crate) address: String,
    pub(crate) path: std::path::PathBuf,
}
