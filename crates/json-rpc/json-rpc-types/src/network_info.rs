use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{NetworkHash, SHA256Hash};

/// Immutable protocol and genesis identity reported by a JSON-RPC server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfo {
    /// Hash of the network name, bound into every transaction signature.
    pub network_hash: NetworkHash,
    /// Hash of the materialized genesis state.
    pub genesis_hash: SHA256Hash,
}

impl NetworkInfo {
    /// Describe a network by its protocol and genesis hashes.
    #[must_use]
    pub fn new(network_hash: NetworkHash, genesis_hash: SHA256Hash) -> Self {
        Self {
            network_hash,
            genesis_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use groundhog_xdr::types::SHA256Hash;
    use serde_json::json;

    use crate::{NetworkHash, hex::Hex0x};

    use super::NetworkInfo;

    #[test]
    fn network_info_round_trips_with_camel_case_fields() {
        let network_info = NetworkInfo::new(
            NetworkHash::from(SHA256Hash::from([0x07; 32])),
            Hex0x(SHA256Hash::from([0x42; 32])),
        );
        let expected = json!({
            "networkHash": format!("0x{}", "07".repeat(32)),
            "genesisHash": format!("0x{}", "42".repeat(32)),
        });

        let encoded = serde_json::to_value(&network_info).expect("bug: NetworkInfo serializes");
        assert_eq!(encoded, expected);

        let decoded: NetworkInfo =
            serde_json::from_value(encoded).expect("bug: NetworkInfo deserializes");
        assert_eq!(decoded, network_info);
    }
}
