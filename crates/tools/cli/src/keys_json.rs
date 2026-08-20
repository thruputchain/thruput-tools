use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub(crate) const KEYS_EXPORT_JSON_SCHEMA_VERSION: u32 = 1;

/// Stable machine-readable result for `groundhog keys export`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeysExportReport {
    pub(crate) schema_version: u32,
    pub(crate) key: ExportedKey,
}

/// A stored Ed25519 key exported for use with a client signer.
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
