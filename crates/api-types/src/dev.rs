use serde::{Deserialize, Serialize};

use crate::ContractAddress;
use crate::hex::HexBytes;

/// A pre-funded dev account, returned by `groundhog_dev_getAccounts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevAccount {
    /// Contract address derived from the public key.
    pub address: ContractAddress,
    /// Ed25519 public key bytes.
    pub public_key: HexBytes,
    /// Ed25519 secret key bytes. Included because these are
    /// well-known dev-only keys.
    pub private_key: HexBytes,
    /// Storage tokens granted at node startup.
    pub storage_tokens: u64,
}
