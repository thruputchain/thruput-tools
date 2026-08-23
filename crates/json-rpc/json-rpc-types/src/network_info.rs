use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::NetworkHash;

/// Fixed identity of the network an endpoint serves.
///
/// Every field here is settled when the chain is created and never changes, so
/// clients may cache this response against an endpoint indefinitely. Anything
/// that moves as the chain advances belongs on `chainTip` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfo {
    /// Hash of the network name, bound into the preimage of every transaction
    /// signature. Signing for the wrong network produces transactions this
    /// network rejects, so clients must read it from the endpoint they submit to.
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
    use super::NetworkInfo;
    use crate::NetworkHash;
    use serde_json::json;

    #[test]
    fn network_info_serializes_the_hash_as_prefixed_hex() {
        let info = NetworkInfo::new(NetworkHash::from(groundhog_xdr::types::SHA256Hash::new(
            [0x42; 32],
        )));

        assert_eq!(
            serde_json::to_value(info).unwrap(),
            json!({ "networkHash": format!("0x{}", "42".repeat(32)) })
        );
    }

    #[test]
    fn network_info_round_trips_through_json() {
        let info = NetworkInfo::new(NetworkHash::from(groundhog_xdr::types::SHA256Hash::new(
            [0x07; 32],
        )));
        let encoded = serde_json::to_value(&info).unwrap();

        assert_eq!(
            serde_json::from_value::<NetworkInfo>(encoded).unwrap(),
            info
        );
    }
}
