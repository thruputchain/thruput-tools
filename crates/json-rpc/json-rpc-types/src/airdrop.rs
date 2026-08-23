use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::receipt::ExecStatus;
use crate::transaction::TxHash;
use crate::{BlockHeight, ContractAddress, SHA256Hash};

/// Maximum size of an airdrop idempotency nonce.
pub const MAX_CLIENT_NONCE_BYTES: usize = 32;

/// Opaque idempotency nonce supplied by an airdrop caller.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientNonce(Vec<u8>);

impl ClientNonce {
    /// Validate and construct a client nonce.
    pub fn new(bytes: Vec<u8>) -> Result<Self, ClientNonceError> {
        match bytes.len() {
            0 => Err(ClientNonceError::Empty),
            len if len > MAX_CLIENT_NONCE_BYTES => Err(ClientNonceError::TooLong),
            _ => Ok(Self(bytes)),
        }
    }

    /// Borrow the nonce bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for ClientNonce {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("0x{}", hex::encode(&self.0)))
    }
}

impl<'de> Deserialize<'de> for ClientNonce {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        let encoded = encoded.strip_prefix("0x").ok_or_else(|| {
            serde::de::Error::custom("clientNonce must use a lowercase 0x prefix")
        })?;
        if !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom(
                "clientNonce must use lowercase hexadecimal characters",
            ));
        }
        let bytes = hex::decode(encoded)
            .map_err(|_| serde::de::Error::custom("clientNonce must contain complete bytes"))?;
        Self::new(bytes).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ClientNonce {
    fn schema_name() -> Cow<'static, str> {
        "ClientNonce".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ClientNonce").into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^0x(?:[0-9a-f]{2}){1,32}$"
        })
    }
}

/// Invalid airdrop idempotency nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClientNonceError {
    /// The nonce contains no bytes.
    #[error("clientNonce must not be empty")]
    Empty,
    /// The nonce exceeds the public resource bound.
    #[error("clientNonce must not exceed 32 bytes")]
    TooLong,
}

/// Public parameters for `groundhog_requestAirdrop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AirdropRequest {
    /// Address receiving the native HOG transfer.
    #[serde(deserialize_with = "deserialize_canonical_contract_address")]
    pub recipient: ContractAddress,
    /// Caller-selected idempotency key for this recipient and network.
    #[serde(deserialize_with = "deserialize_client_nonce")]
    pub client_nonce: ClientNonce,
}

/// Hosted faucet request, bound to the gateway's immutable network identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostedAirdropRequest {
    /// Address receiving the native HOG transfer.
    #[serde(deserialize_with = "deserialize_canonical_contract_address")]
    pub recipient: ContractAddress,
    /// Caller-selected idempotency key for this recipient and network.
    #[serde(deserialize_with = "deserialize_client_nonce")]
    pub client_nonce: ClientNonce,
    /// Logical network identifier advertised by the gateway.
    pub network_id: String,
    /// Genesis hash advertised by the gateway.
    pub genesis_hash: SHA256Hash,
}

fn deserialize_client_nonce<'de, D>(deserializer: D) -> Result<ClientNonce, D::Error>
where
    D: Deserializer<'de>,
{
    ClientNonce::deserialize(deserializer)
        .map_err(|error| serde::de::Error::custom(format!("clientNonce: {error}")))
}

fn deserialize_canonical_contract_address<'de, D>(
    deserializer: D,
) -> Result<ContractAddress, D::Error>
where
    D: Deserializer<'de>,
{
    let encoded = String::deserialize(deserializer)?;
    let Some(hex) = encoded.strip_prefix("0x") else {
        return Err(serde::de::Error::custom(
            "recipient must use a lowercase 0x prefix",
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(serde::de::Error::custom(
            "recipient must be exactly 32 lowercase hexadecimal bytes",
        ));
    }
    let bytes: [u8; 32] = hex::decode(hex)
        .map_err(serde::de::Error::custom)?
        .try_into()
        .map_err(|_| serde::de::Error::custom("recipient must be exactly 32 bytes"))?;
    Ok(crate::hex::Hex0x(
        groundhog_xdr::types::ContractAddress::from(bytes),
    ))
}

/// Stable lifecycle response for hosted and local airdrop attempts.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "attemptStatus",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AirdropAttemptResponse {
    /// The exact signed transaction is submitted but not yet confirmed.
    Pending {
        /// Address receiving the native HOG transfer.
        recipient: ContractAddress,
        /// Native amount in indivisible units.
        raw_amount: String,
        /// Canonical transaction hash, once the transaction is accepted.
        #[serde(skip_serializing_if = "Option::is_none")]
        tx_hash: Option<TxHash>,
        /// Suggested delay before retrying the same logical attempt.
        retry_after_seconds: u64,
    },
    /// The transaction committed successfully.
    Succeeded {
        /// Address receiving the native HOG transfer.
        recipient: ContractAddress,
        /// Canonical transaction hash.
        tx_hash: TxHash,
        /// Native amount in indivisible units.
        raw_amount: String,
        /// Committed block height.
        height: BlockHeight,
        /// Committed execution result.
        status: ExecStatus,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ClientNonce, ClientNonceError, MAX_CLIENT_NONCE_BYTES};

    #[test]
    fn client_nonce_roundtrips() {
        let nonce = ClientNonce::new(vec![0x01, 0xab]).expect("nonce should be valid");
        let encoded = serde_json::to_value(&nonce).expect("nonce should serialize");
        assert_eq!(encoded, json!("0x01ab"));
        assert_eq!(
            serde_json::from_value::<ClientNonce>(encoded).expect("nonce should deserialize"),
            nonce
        );
    }

    #[test]
    fn client_nonce_enforces_resource_bounds() {
        assert_eq!(ClientNonce::new(Vec::new()), Err(ClientNonceError::Empty));
        assert_eq!(
            ClientNonce::new(vec![0; MAX_CLIENT_NONCE_BYTES + 1]),
            Err(ClientNonceError::TooLong)
        );
    }

    #[test]
    fn client_nonce_requires_canonical_hex() {
        for encoded in [r#""01""#, r#""0X01""#, r#""0xAB""#] {
            assert!(serde_json::from_str::<ClientNonce>(encoded).is_err());
        }
    }

    #[test]
    fn airdrop_request_requires_canonical_recipient() {
        for recipient in ["11".repeat(32), format!("0X{}", "11".repeat(32))] {
            let request = serde_json::json!({
                "recipient": recipient,
                "clientNonce": "0x01"
            });
            assert!(serde_json::from_value::<super::AirdropRequest>(request).is_err());
        }
    }

    #[test]
    fn airdrop_response_accepts_future_additive_fields() {
        let response = serde_json::json!({
            "attemptStatus": "pending",
            "recipient": format!("0x{}", "11".repeat(32)),
            "rawAmount": "100000000",
            "retryAfterSeconds": 5,
            "futureField": true
        });

        assert!(serde_json::from_value::<super::AirdropAttemptResponse>(response).is_ok());
    }
}
