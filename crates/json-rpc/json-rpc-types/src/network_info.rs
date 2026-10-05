use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::NetworkHash;

/// Immutable protocol identity reported by a JSON-RPC server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfo {
    /// Hash of the network name, bound into every transaction signature.
    pub network_hash: NetworkHash,
}

impl NetworkInfo {
    /// Describe a network by the hash its signatures are bound to.
    #[must_use]
    pub fn new(network_hash: NetworkHash) -> Self {
        Self { network_hash }
    }
}
