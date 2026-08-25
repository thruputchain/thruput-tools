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

#[cfg(test)]
mod tests {
    use groundhog_xdr::types::SHA256Hash;
    use serde_json::json;

    use crate::NetworkHash;

    use super::NetworkInfo;

    #[test]
    fn network_info_round_trips_with_camel_case_fields() {
        let network_info = NetworkInfo::new(NetworkHash::from(SHA256Hash::from([0x07; 32])));
        let expected = json!({
            "networkHash": format!("0x{}", "07".repeat(32)),
        });

        let encoded = serde_json::to_value(&network_info).expect("bug: NetworkInfo serializes");
        assert_eq!(encoded, expected);

        let decoded: NetworkInfo =
            serde_json::from_value(encoded).expect("bug: NetworkInfo deserializes");
        assert_eq!(decoded, network_info);
    }
}
