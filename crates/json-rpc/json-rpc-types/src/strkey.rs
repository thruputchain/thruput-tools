//! Stellar `StrKey` text for addresses and public keys on the JSON-RPC wire.
//!
//! Contract and wallet addresses render as `C…` `StrKeys` and Ed25519 public
//! keys as `G…` `StrKeys`: the SEP-23 encodings Stellar uses for the same
//! 32-byte values. A `StrKey` names its kind and carries a checksum, so a
//! public key given where an address belongs, or a mistyped character, is
//! rejected instead of being read as other bytes.
//!
//! Parsing also accepts 64-character hex, with or without a `0x` prefix: the
//! form older clients, saved configuration, and hogd-dev runtime manifests
//! use.

use std::borrow::Cow;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hex::decode_fixed_hex;

/// A 32-byte XDR value with a Stellar `StrKey` text form.
pub trait StrKeyKind: Copy + From<[u8; 32]> {
    /// First character of every `StrKey` of this kind.
    const PREFIX: char;
    /// What the value is, as parse errors name it.
    const DESCRIPTION: &'static str;

    /// Write the value as its `StrKey`, honoring the formatter's width and alignment.
    fn fmt_strkey(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;

    /// Decode a `StrKey` of this kind, or `None` for any other text.
    fn parse_strkey(text: &str) -> Option<Self>;
}

impl StrKeyKind for groundhog_xdr::types::ContractAddress {
    const PREFIX: char = 'C';
    const DESCRIPTION: &'static str = "contract address";

    fn fmt_strkey(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&stellar_strkey::Contract(self.into_inner()).to_string())
    }

    fn parse_strkey(text: &str) -> Option<Self> {
        stellar_strkey::Contract::from_string(text)
            .ok()
            .map(|key| key.0.into())
    }
}

impl StrKeyKind for groundhog_xdr::types::Ed25519PublicKey {
    const PREFIX: char = 'G';
    const DESCRIPTION: &'static str = "Ed25519 public key";

    fn fmt_strkey(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&stellar_strkey::ed25519::PublicKey(self.into_inner()).to_string())
    }

    fn parse_strkey(text: &str) -> Option<Self> {
        stellar_strkey::ed25519::PublicKey::from_string(text)
            .ok()
            .map(|key| key.0.into())
    }
}

/// Wraps an address or public key so it displays and serializes as its
/// `StrKey`, for example `"CAAAA…"` for a contract address.
///
/// Parsing and deserialization accept that `StrKey` or the legacy 32-byte
/// hex form, with or without a `0x` prefix. A `StrKey` of another kind is
/// rejected.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrKey<T>(pub T);

/// Text that is neither a `StrKey` of the expected kind nor 32-byte hex.
///
/// The message never repeats the rejected text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "{description} must be a {prefix} StrKey or 64 hex characters, optionally prefixed with 0x"
)]
pub struct StrKeyError {
    prefix: char,
    description: &'static str,
}

impl<T: StrKeyKind> fmt::Display for StrKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt_strkey(f)
    }
}

impl<T: StrKeyKind> fmt::Debug for StrKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt_strkey(f)
    }
}

impl<T: StrKeyKind> FromStr for StrKey<T> {
    type Err = StrKeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(value) = T::parse_strkey(text) {
            return Ok(Self(value));
        }
        decode_fixed_hex::<32>(text)
            .map(|bytes| Self(T::from(bytes)))
            .map_err(|_| StrKeyError {
                prefix: T::PREFIX,
                description: T::DESCRIPTION,
            })
    }
}

impl<T: StrKeyKind> Serialize for StrKey<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de, T: StrKeyKind> Deserialize<'de> for StrKey<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl<T: StrKeyKind + JsonSchema> JsonSchema for StrKey<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("StrKey_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("{}::StrKey<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        let pattern = format!("^{}[A-Z2-7]{{55}}$", T::PREFIX);
        json_schema!({
            "type": "string",
            "pattern": pattern
        })
    }
}

impl<T> Deref for StrKey<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for StrKey<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> From<T> for StrKey<T> {
    fn from(inner: T) -> Self {
        StrKey(inner)
    }
}
