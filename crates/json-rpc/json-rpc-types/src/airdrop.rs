use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hex::HexBytes;
use crate::receipt::ExecStatus;
use crate::transaction::TxHash;
use crate::{BlockHeight, ContractAddress, NetworkHash, SHA256Hash};

/// Maximum size of an airdrop idempotency nonce.
pub const MAX_CLIENT_NONCE_BYTES: usize = 32;

/// Retained transaction evidence returned when local funding cannot resolve an outcome.
/// This does not prove admission or execution. Reconcile this hash and reuse the original request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct LocalFundingErrorData {
    /// Canonical hash of the retained transaction body.
    pub tx_hash: TxHash,
    /// Exact signed bytes persisted before the first submission.
    pub signed_transaction: HexBytes,
}

impl LocalFundingErrorData {
    /// Construct retained evidence without asserting an outcome.
    pub fn new(tx_hash: TxHash, signed_transaction: HexBytes) -> Self {
        Self {
            tx_hash,
            signed_transaction,
        }
    }
}

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
        serializer.collect_str(&crate::hex::HexDisplay(&self.0))
    }
}

impl<'de> Deserialize<'de> for ClientNonce {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let HexBytes(bytes) = HexBytes::deserialize(deserializer)
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirdropRequest {
    /// Address receiving the native HOG transfer.
    pub recipient: ContractAddress,
    /// Caller-selected idempotency key for this recipient and network.
    pub client_nonce: ClientNonce,
}

/// Public parameters for local-only `groundhog_fundAccount`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FundAccountRequest {
    /// Native HOG transfer recipient.
    pub recipient: ContractAddress,
    /// Positive canonical decimal amount in raw HOG units.
    #[serde(deserialize_with = "positive_raw_amount")]
    pub raw_amount: String,
    /// Retained caller-owned idempotency nonce.
    pub client_nonce: ClientNonce,
    /// Expected random faucet identity; refuses a different chain at the same URL.
    pub faucet: ContractAddress,
}

impl FundAccountRequest {
    /// Validate an explicitly constructed request's amount too.
    pub fn amount(&self) -> Result<u128, &'static str> {
        parse_positive_raw_amount(&self.raw_amount)
    }
}

fn parse_positive_raw_amount(text: &str) -> Result<u128, &'static str> {
    match text.parse::<u128>() {
        Ok(value) if value > 0 && value.to_string() == text => Ok(value),
        _ => Err("rawAmount must be a positive canonical decimal u128"),
    }
}

fn positive_raw_amount<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_positive_raw_amount(&text).map_err(serde::de::Error::custom)?;
    Ok(text)
}

/// Authoritatively admitted (or already committed) local funding submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct FundAccountResponse {
    /// Recipient of the retained transfer.
    pub recipient: ContractAddress,
    /// Exact requested raw HOG amount.
    pub raw_amount: String,
    /// Canonical body hash for status and receipt queries.
    pub tx_hash: TxHash,
    /// Signed transaction retained before its first broadcast.
    pub signed_transaction: HexBytes,
}

impl FundAccountResponse {
    /// Construct an admitted local funding response.
    pub fn new(
        recipient: ContractAddress,
        raw_amount: String,
        tx_hash: TxHash,
        signed_transaction: HexBytes,
    ) -> Self {
        Self {
            recipient,
            raw_amount,
            tx_hash,
            signed_transaction,
        }
    }
}

/// Hosted faucet request, bound to the gateway's immutable network identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostedAirdropRequest {
    /// Address receiving the native HOG transfer.
    pub recipient: ContractAddress,
    /// Caller-selected idempotency key for this recipient and network.
    pub client_nonce: ClientNonce,
    /// Logical network identifier advertised by the gateway.
    pub network_id: String,
    /// Hash of the network name bound into transaction signatures.
    pub network_hash: NetworkHash,
    /// Genesis hash advertised by the gateway.
    pub genesis_hash: SHA256Hash,
}

/// Stable lifecycle response for hosted and local airdrop attempts.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(
    tag = "attemptStatus",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AirdropAttemptResponse {
    /// The logical attempt is in progress and may not have been submitted yet.
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

impl AirdropAttemptResponse {
    /// Recipient bound to this logical attempt.
    pub fn recipient(&self) -> &ContractAddress {
        match self {
            Self::Pending { recipient, .. } | Self::Succeeded { recipient, .. } => recipient,
        }
    }

    /// Native amount in indivisible units.
    pub fn raw_amount(&self) -> &str {
        match self {
            Self::Pending { raw_amount, .. } | Self::Succeeded { raw_amount, .. } => raw_amount,
        }
    }
}
