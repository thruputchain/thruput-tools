//! Typed base58 JSON boundaries for identifiers and cryptographic values.
//!
//! Opaque payloads, including raw topics, storage keys, and cursors, continue to
//! use the hex types. These wrappers do not change binary protocol serialization.

use std::borrow::Cow;
use std::fmt;
use std::ops::{Deref, DerefMut};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A fixed-size protocol value with a bounded base58 decoder.
///
/// Implementations must validate the decoded byte length, not just the alphabet
/// or encoded length. Do not implement this for opaque payloads merely because
/// they happen to have the same length as an identifier.
pub trait FixedIdentifier: AsRef<[u8]> + Sized {
    /// Exact decoded byte length.
    const BYTE_LENGTH: usize;

    /// Decode canonical machine text, rejecting hex and typed human inputs.
    fn decode_base58(text: &str) -> Result<Self, thruput_encoding::Error>;
}

macro_rules! fixed_identifier {
    ($ty:ty, $length:expr) => {
        impl FixedIdentifier for $ty {
            const BYTE_LENGTH: usize = $length;

            fn decode_base58(text: &str) -> Result<Self, thruput_encoding::Error> {
                thruput_encoding::decode_fixed::<$length>(text).map(Self::from)
            }
        }
    };
}

fixed_identifier!(thruput_xdr::types::SHA256Hash, 32);
fixed_identifier!(thruput_xdr::types::ContractAddress, 32);
fixed_identifier!(thruput_xdr::types::Ed25519PublicKey, 32);
fixed_identifier!(thruput_xdr::types::Ed25519Signature, 64);
fixed_identifier!(thruput_xdr::types::StorageLongHash, 2048);
fixed_identifier!(thruput_xdr::validator::ValidatorID, 32);

/// A fixed identifier represented as unprefixed Bitcoin/Solana base58 in JSON.
///
/// Human-facing parsers may accept typed Stellar input separately; this machine
/// boundary accepts only base58. Leading zero bytes are preserved.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Base58<T>(pub T);

impl<T: FixedIdentifier> Serialize for Base58<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&thruput_encoding::encode(self.0.as_ref()))
    }
}

impl<'de, T: FixedIdentifier> Deserialize<'de> for Base58<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        T::decode_base58(&text)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl<T: FixedIdentifier + JsonSchema> JsonSchema for Base58<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Base58_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("{}::Base58<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[1-9A-HJ-NP-Za-km-z]+$",
            "minLength": T::BYTE_LENGTH,
            "maxLength": thruput_encoding::max_encoded_len(T::BYTE_LENGTH),
            "description": format!(
                "Unprefixed, case-sensitive Bitcoin/Solana base58; must decode to exactly {} bytes. Encoded length and alphabet alone do not validate decoded length.",
                T::BYTE_LENGTH
            )
        })
    }
}

impl<T: FixedIdentifier> fmt::Display for Base58<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&thruput_encoding::encode(self.0.as_ref()))
    }
}

impl<T: FixedIdentifier> fmt::Debug for Base58<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Base58").field(&self.to_string()).finish()
    }
}

impl<T> Deref for Base58<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Base58<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> From<T> for Base58<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests;
