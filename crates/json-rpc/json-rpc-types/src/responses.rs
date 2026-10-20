//! Native RPC result contracts. Production wire-13 projections remain separate.
//!
//! Schemas describe known producer variants. Readers accept added object fields
//! and retain unfamiliar variants; use [`DecodedResponse`] to distinguish those
//! results before interpreting them. Request-context checks (identity, snapshot,
//! page progress and batch cardinality) remain the caller's responsibility.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// A decoded value can contain unsupported data at a declared union boundary.
/// Implementations inspect typed fields only, never arbitrary added JSON fields.
pub trait ResponseData {
    /// Whether every variant is understood. This is not an execution-success,
    /// admission, retry, freshness or request-identity check.
    fn is_supported(&self) -> bool;
}

/// A complete result, or the unmodified JSON value of an unsupported result.
/// Malformed known variants and envelopes fail deserialization instead.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DecodedResponse<T> {
    /// All variants are understood. Failed execution and null lookups can still
    /// be known results; the caller must interpret their explicit semantics.
    Known(T),
    /// At least one declared variant is unsupported. Preserve the entire result
    /// so the caller retains identity, commitment and unfamiliar payload fields.
    Unsupported { raw: Value },
}

impl<'de, T: DeserializeOwned + ResponseData> Deserialize<'de> for DecodedResponse<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        // Finish typed validation before classifying support. An unfamiliar
        // nested variant must not hide a malformed known sibling.
        let value: T = serde_json::from_value(raw.clone()).map_err(serde::de::Error::custom)?;
        Ok(if value.is_supported() {
            Self::Known(value)
        } else {
            Self::Unsupported { raw }
        })
    }
}

/// Reader-only unfamiliar tagged payload. It cannot be constructed with a
/// malformed or known tag through the response deserializer.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct UnknownVariant(Value);

impl UnknownVariant {
    /// Original JSON object, including its discriminator and nested fields.
    pub fn raw(&self) -> &Value {
        &self.0
    }
}

// Declare each object's fields once for serialization, schema and map-only
// decoding. Added fields are tolerated, unlike strict request objects.
macro_rules! response_object {
    ($(#[$meta:meta])* $name:ident { $($(#[$attr:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Serialize, JsonSchema)]
        #[serde(rename_all = "camelCase")]
        pub struct $name { $($(#[$attr])* pub $field: $ty),* }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize, JsonSchema)]
                #[serde(rename_all = "camelCase")]
                struct Fields { $($(#[$attr])* $field: $ty),* }
                let fields: Fields = crate::requests::fields::map_object(d)?;
                Ok(Self { $($field: fields.$field),* })
            }
        }
        impl ResponseData for $name {
            fn is_supported(&self) -> bool { true $(&& self.$field.is_supported())* }
        }
    };
}

// Unknown tags are captured only after discriminator inspection. A permissive
// untagged fallback would also accept malformed *known* payloads.
macro_rules! response_enum {
    ($(#[$meta:meta])* $name:ident, $tag:literal {
        $($variant:ident($wire:literal, $supported:literal) {
            $($(#[$attr:meta])* $field:ident: $ty:ty),* $(,)?
        }),* $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Serialize, JsonSchema)]
        #[non_exhaustive]
        #[serde(tag = $tag, rename_all_fields = "camelCase")]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant { $($(#[$attr])* $field: $ty),* },)*
            /// Reader-only unfamiliar variant; excluded from the producer schema.
            #[serde(untagged)]
            #[schemars(skip)]
            Unsupported(UnknownVariant),
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize, JsonSchema)]
                #[serde(tag = $tag, rename_all_fields = "camelCase")]
                enum Known { $(#[serde(rename = $wire)] $variant { $($(#[$attr])* $field: $ty),* }),* }
                let raw = Value::deserialize(d)?;
                let tag = raw.as_object().and_then(|map| map.get($tag)).and_then(Value::as_str)
                    .ok_or_else(|| serde::de::Error::custom(concat!("expected object with string ", $tag)))?;
                if !matches!(tag, $($wire)|*) {
                    return Ok(Self::Unsupported(UnknownVariant(raw)));
                }
                let known = serde_json::from_value::<Known>(raw).map_err(serde::de::Error::custom)?;
                Ok(match known { $(Known::$variant { $($field),* } => Self::$variant { $($field),* }),* })
            }
        }
        impl ResponseData for $name {
            fn is_supported(&self) -> bool {
                match self {
                    $(Self::$variant { $($field),* } => $supported $(&& $field.is_supported())*,)*
                    Self::Unsupported(_) => false,
                }
            }
        }
    };
}

macro_rules! response_strings {
    ($(#[$meta:meta])* $name:ident { $($variant:ident($wire:literal, $supported:literal)),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Serialize, JsonSchema)]
        #[non_exhaustive]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant,)*
            /// Reader-only unfamiliar string; excluded from the producer schema.
            #[serde(untagged)]
            #[schemars(skip)]
            Unsupported(String),
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                Ok(match raw.as_str() { $($wire => Self::$variant,)* _ => Self::Unsupported(raw) })
            }
        }
        impl ResponseData for $name {
            fn is_supported(&self) -> bool {
                match self { $(Self::$variant => $supported,)* Self::Unsupported(_) => false }
            }
        }
    };
}

macro_rules! scalar_data {
    ($($ty:ty),* $(,)?) => { $(impl ResponseData for $ty { fn is_supported(&self) -> bool { true } })* };
}
scalar_data!(
    String,
    u32,
    i32,
    u64,
    i128,
    u128,
    crate::BlockHeight,
    crate::Protocol,
    crate::TimePoint,
    crate::hex::HexBytes,
    crate::hex::Topic,
    crate::transaction::TxHash
);
impl<T> ResponseData for crate::base58::Base58<T> {
    fn is_supported(&self) -> bool {
        true
    }
}
impl<T: ResponseData> ResponseData for Vec<T> {
    fn is_supported(&self) -> bool {
        self.iter().all(ResponseData::is_supported)
    }
}
impl<T: ResponseData> ResponseData for Option<T> {
    fn is_supported(&self) -> bool {
        self.as_ref().is_none_or(ResponseData::is_supported)
    }
}

mod discovery;
mod fields;
mod funding;
mod history;
mod simulation;
mod state;
mod transaction;
pub use discovery::*;
pub use funding::*;
pub use history::*;
pub use simulation::*;
pub use state::*;
pub use transaction::*;
