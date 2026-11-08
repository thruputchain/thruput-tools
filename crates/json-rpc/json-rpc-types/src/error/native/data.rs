use crate::requests::{
    fields::{hex_schema, read_hex, u32_schema},
    height_schema, read_height,
};
use crate::responses::{SchedulerError, TransactionReceipt};
use crate::{BlockHeight, decimal::Decimal, hex::HexBytes, transaction::TxHash};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A future reason refines a known family without making that family undecodable.
/// The schema documents producer values; readers retain unfamiliar strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Reason<T> {
    Known(T),
    Unrecognized(String),
}
impl<T: JsonSchema> JsonSchema for Reason<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn schema_id() -> std::borrow::Cow<'static, str> {
        T::schema_id()
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        T::json_schema(g)
    }
}

/// Evidence for this submission attempt, never proof about an earlier attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AttemptAdmission {
    NotAttempted,
    Rejected,
    Unknown,
}

/// Evidence-backed refinements; omission conveys no finer classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum InvalidParamsReason {
    MissingField,
    UnknownField,
    InvalidType,
    InvalidFormat,
    OutOfRange,
    ConflictingFields,
    InvalidValue,
}

/// Evidence-backed refinements; omission conveys no finer classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum InvalidCursorReason {
    Malformed,
    UnsupportedVersion,
    QueryMismatch,
}

/// Evidence-backed refinements; omission conveys no finer classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum TransactionInvalidReason {
    InvalidEncoding,
    MalformedTransaction,
    InvalidAuthEnvelope,
    TransactionTooLarge,
    ExecutorBoundViolation,
}

/// Evidence-backed refinements; omission conveys no finer classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum TransactionRejectedReason {
    Expired,
    AuthenticationRejected,
}

/// Evidence-backed refinements; omission conveys no finer classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum HistoryUnavailableReason {
    OutsideRetention,
}

/// Evidence-backed refinements; omission conveys no finer classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum BackendDataInvalidReason {
    InvalidEncoding,
    InvalidCardinality,
    UnsupportedValue,
    InconsistentResult,
}

/// The selected application-data union. Use `DecodedError` for method/code and
/// evidence validation. Deserializing this union alone does not establish those
/// relationships. Additional fields are retained in the reader's raw error.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum ErrorData {
    InvalidParams {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Reason<InvalidParamsReason>")]
        reason: Option<Reason<InvalidParamsReason>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "optional_pointer"
        )]
        #[schemars(schema_with = "pointer_schema")]
        path: Option<String>,
    },
    InvalidCursor {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Reason<InvalidCursorReason>")]
        reason: Option<Reason<InvalidCursorReason>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "optional_pointer"
        )]
        #[schemars(schema_with = "pointer_schema")]
        path: Option<String>,
    },
    TransactionInvalid {
        admission: AttemptAdmission,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Reason<TransactionInvalidReason>")]
        reason: Option<Reason<TransactionInvalidReason>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "BoundDetail")]
        detail: Option<BoundDetail>,
    },
    TransactionRejected {
        admission: AttemptAdmission,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Reason<TransactionRejectedReason>")]
        reason: Option<Reason<TransactionRejectedReason>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "SchedulerError")]
        error: Option<SchedulerError>,
    },
    StateHeightUnavailable {
        #[serde(
            serialize_with = "crate::decimal::serialize",
            deserialize_with = "read_height"
        )]
        #[schemars(schema_with = "height_schema")]
        requested_state_block_height: BlockHeight,
        #[serde(
            serialize_with = "crate::decimal::serialize",
            deserialize_with = "read_height"
        )]
        #[schemars(schema_with = "height_schema")]
        observed_state_block_height: BlockHeight,
    },
    HistoryUnavailable {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Reason<HistoryUnavailableReason>")]
        reason: Option<Reason<HistoryUnavailableReason>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "crate::decimal::option::serialize",
            deserialize_with = "optional_height"
        )]
        #[schemars(schema_with = "height_schema")]
        oldest_indexed_block_height: Option<BlockHeight>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "crate::decimal::option::serialize",
            deserialize_with = "optional_height"
        )]
        #[schemars(schema_with = "height_schema")]
        latest_indexed_block_height: Option<BlockHeight>,
    },
    BackendUnavailable {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "AttemptAdmission")]
        admission: Option<AttemptAdmission>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "crate::decimal::option::serialize",
            deserialize_with = "optional_delay"
        )]
        #[schemars(with = "crate::decimal::Decimal<u64>")]
        retry_after_ms: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "SchedulerError")]
        error: Option<SchedulerError>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "ServiceCode")]
        service_code: Option<ServiceCode>,
    },
    BackendTimeout {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "AttemptAdmission")]
        admission: Option<AttemptAdmission>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "crate::decimal::option::serialize",
            deserialize_with = "optional_delay"
        )]
        #[schemars(with = "crate::decimal::Decimal<u64>")]
        retry_after_ms: Option<u64>,
    },
    Overloaded {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "AttemptAdmission")]
        admission: Option<AttemptAdmission>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "crate::decimal::option::serialize",
            deserialize_with = "optional_delay"
        )]
        #[schemars(with = "crate::decimal::Decimal<u64>")]
        retry_after_ms: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "SchedulerError")]
        error: Option<SchedulerError>,
    },
    /// The client exceeded the gateway's request budget before backend work.
    RateLimited {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "crate::decimal::option::serialize",
            deserialize_with = "optional_delay"
        )]
        #[schemars(with = "crate::decimal::Decimal<u64>")]
        retry_after_ms: Option<u64>,
    },
    BackendDataInvalid {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "AttemptAdmission")]
        admission: Option<AttemptAdmission>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Reason<BackendDataInvalidReason>")]
        reason: Option<Reason<BackendDataInvalidReason>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "ReceiptObservation")]
        observation: Option<ReceiptObservation>,
    },
    FundingAttemptStopped(FundingAttemptStoppedData),
    LocalFundingUnresolved {
        transaction_hash: TxHash,
        #[serde(deserialize_with = "read_hex")]
        #[schemars(schema_with = "hex_schema")]
        signed_transaction: HexBytes,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "FundingCause")]
        cause: Option<FundingCause>,
    },
}

/// Retained funding evidence. A stop does not establish nonexecution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FundingAttemptStoppedData {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "ServiceCode")]
    pub service_code: Option<ServiceCode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "TxHash")]
    pub transaction_hash: Option<TxHash>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_hex"
    )]
    #[schemars(schema_with = "hex_schema")]
    pub signed_transaction: Option<HexBytes>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "TransactionReceipt")]
    pub receipt: Option<TransactionReceipt>,
}

/// Recognized stop retained inside unresolved local funding evidence.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum FundingCause {
    FundingAttemptStopped(FundingAttemptStoppedData),
}

/// Independently validated commitment identity; no partial receipt payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptObservation {
    pub transaction_hash: TxHash,
    #[serde(serialize_with = "crate::decimal::serialize")]
    #[schemars(schema_with = "height_schema")]
    pub block_height: BlockHeight,
}
impl<'de> Deserialize<'de> for ReceiptObservation {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fields {
            transaction_hash: TxHash,
            #[serde(deserialize_with = "read_height")]
            block_height: BlockHeight,
        }
        let value: Fields = crate::requests::fields::map_object(d)?;
        Ok(Self {
            transaction_hash: value.transaction_hash,
            block_height: value.block_height,
        })
    }
}

/// Bounded service diagnostic, not a message or a recovery decision.
/// The native contract limits producer codes to 128 Unicode scalar values.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ServiceCode(#[schemars(length(min = 1, max = 128))] String);
/// A service diagnostic must contain 1..128 Unicode scalar values.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("serviceCode must contain 1..128 characters")]
pub struct InvalidServiceCode;

impl TryFrom<String> for ServiceCode {
    type Error = InvalidServiceCode;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.chars().count() > 128 {
            return Err(InvalidServiceCode);
        }
        Ok(Self(value))
    }
}
impl<'de> Deserialize<'de> for ServiceCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl ServiceCode {
    /// Original diagnostic code without truncation or message parsing.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
fn optional_height<'de, D: Deserializer<'de>>(d: D) -> Result<Option<BlockHeight>, D::Error> {
    read_height(d).map(Some)
}
fn optional_delay<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Decimal::<u64>::deserialize(d).map(|v| Some(v.0))
}
fn optional_hex<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HexBytes>, D::Error> {
    read_hex(d).map(Some)
}

/// Existing executor-bound diagnostic fields, with native numeric constraints.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "reason",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum ExecutorBoundDetail {
    TooManyContracts {
        #[schemars(schema_with = "u32_schema")]
        attached: u32,
        #[schemars(schema_with = "u32_schema")]
        max: u32,
    },
    ContractTooLarge {
        #[schemars(schema_with = "u32_schema")]
        index: u32,
        #[schemars(schema_with = "u32_schema")]
        size_bytes: u32,
        #[schemars(schema_with = "u32_schema")]
        max_bytes: u32,
    },
    DuplicateContract {
        #[schemars(schema_with = "u32_schema")]
        index: u32,
        #[schemars(schema_with = "u32_schema")]
        first_index: u32,
    },
    Rv32MemoryTooLarge {
        #[schemars(schema_with = "u32_schema")]
        index: u32,
        #[schemars(schema_with = "u32_schema")]
        memory_bytes: u32,
        #[schemars(schema_with = "u32_schema")]
        max_bytes: u32,
    },
    Rv32CodeTooLarge {
        #[schemars(schema_with = "u32_schema")]
        index: u32,
        #[schemars(schema_with = "u32_schema")]
        code_bytes: u32,
        #[schemars(schema_with = "u32_schema")]
        max_bytes: u32,
    },
    GasLimitTooHigh {
        #[serde(with = "crate::decimal")]
        #[schemars(with = "Decimal<u64>")]
        gas_limit: u64,
        #[serde(with = "crate::decimal")]
        #[schemars(with = "Decimal<u64>")]
        max_gas: u64,
    },
}

/// A known bound detail or its complete future refinement. Malformed known
/// details fail decoding rather than falling through to the future case.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum BoundDetail {
    Known(ExecutorBoundDetail),
    Unrecognized(Value),
}
impl<'de> Deserialize<'de> for BoundDetail {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        let tag = raw
            .as_object()
            .and_then(|o| o.get("reason"))
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("expected bound detail with string reason"))?;
        if matches!(
            tag,
            "tooManyContracts"
                | "contractTooLarge"
                | "duplicateContract"
                | "rv32MemoryTooLarge"
                | "rv32CodeTooLarge"
                | "gasLimitTooHigh"
        ) {
            serde_json::from_value(raw)
                .map(Self::Known)
                .map_err(serde::de::Error::custom)
        } else {
            Ok(Self::Unrecognized(raw))
        }
    }
}
impl JsonSchema for BoundDetail {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ExecutorBoundDetail".into()
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        ExecutorBoundDetail::json_schema(g)
    }
}

fn optional_pointer<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let value = String::deserialize(d)?;
    if !value.is_empty() && !value.starts_with('/') {
        return Err(serde::de::Error::custom("path must be a JSON Pointer"));
    }
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err(serde::de::Error::custom("invalid JSON Pointer escape"));
        }
    }
    Ok(Some(value))
}
fn pointer_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type": "string", "pattern": "^(?:/(?:[^~/]|~[01])*)*(?![\\s\\S])"})
}
