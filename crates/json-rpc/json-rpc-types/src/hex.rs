//! Hex-encoded byte types for the JSON-RPC wire format.
//!
//! All byte values in the Groundhog RPC API are serialized as
//! `0x`-prefixed lowercase hex strings, following the Ethereum
//! convention (e.g., `"0xa1b2c3"`).
//!
//! See `docs/gateway/system-design.md` "Wire Format Conventions" for the full
//! serialization table.

use std::borrow::Cow;
use std::fmt;
use std::ops::{Deref, DerefMut};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------
// Hex0x<T> — generic 0x-prefixed hex wrapper for fixed-size XDR byte types
// ---------------------------------------------------------------------------

/// Wraps an XDR byte type (e.g., `SHA256Hash`, `ContractAddress`) and
/// serializes it as a `"0x..."` lowercase hex string.
///
/// The inner type must implement `AsRef<[u8]>` (for serialization) and
/// `From<[u8; N]>` (for deserialization). This is true for all
/// `groundhog-xdr` fixed-size byte types.
///
/// ```json
/// "0xa1b2c3d4e5f6..."
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hex0x<T>(pub T);

impl<T: AsRef<[u8]>> Serialize for Hex0x<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex_str = format!("0x{}", hex::encode(self.0.as_ref()));
        serializer.serialize_str(&hex_str)
    }
}

impl<'de, T> Deserialize<'de> for Hex0x<T>
where
    T: AsRef<[u8]> + TryFromSlice,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(&s);
        let bytes = hex::decode(s).map_err(serde::de::Error::custom)?;
        let inner = T::try_from_slice(&bytes).map_err(serde::de::Error::custom)?;
        Ok(Hex0x(inner))
    }
}

/// Trait for constructing a fixed-size XDR type from a byte slice.
pub trait TryFromSlice: Sized {
    fn try_from_slice(slice: &[u8]) -> Result<Self, String>;
}

/// Implement `TryFromSlice` for a concrete XDR type with a known byte size.
macro_rules! impl_try_from_slice_for {
    ($ty:ty, $n:expr) => {
        impl TryFromSlice for $ty {
            fn try_from_slice(slice: &[u8]) -> Result<Self, String> {
                let arr: [u8; $n] = slice.try_into().map_err(|_| {
                    format!(
                        "invalid length {}, expected {} bytes ({} hex characters)",
                        slice.len(),
                        $n,
                        $n * 2
                    )
                })?;
                Ok(<$ty>::from(arr))
            }
        }
    };
}

impl_try_from_slice_for!(groundhog_xdr::types::SHA256Hash, 32);
impl_try_from_slice_for!(groundhog_xdr::types::ContractAddress, 32);
impl_try_from_slice_for!(groundhog_xdr::types::Ed25519PublicKey, 32);
impl_try_from_slice_for!(groundhog_xdr::types::DbHash, 2048);

// BlockHash wraps SHA256Hash, so deserialize as SHA256Hash then convert.
impl TryFromSlice for groundhog_xdr::types::BlockHash {
    fn try_from_slice(slice: &[u8]) -> Result<Self, String> {
        let hash = groundhog_xdr::types::SHA256Hash::try_from_slice(slice)?;
        Ok(groundhog_xdr::types::BlockHash::from(hash))
    }
}

impl<T: JsonSchema> JsonSchema for Hex0x<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Hex0x_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("{}::Hex0x<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^0x[0-9a-f]*$"
        })
    }
}

impl<T: fmt::Debug> fmt::Debug for Hex0x<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> Deref for Hex0x<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Hex0x<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> From<T> for Hex0x<T> {
    fn from(inner: T) -> Self {
        Hex0x(inner)
    }
}

// ---------------------------------------------------------------------------
// Variable-length byte types (unchanged, already 0x-aware)
// ---------------------------------------------------------------------------

/// Variable-length byte array. Serializes as `"0x..."` hex string.
///
/// Used for event data and other opaque byte fields in the RPC API.
///
/// ```json
/// "0x48656c6c6f"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexBytes(pub Vec<u8>);

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex_str = format!("0x{}", hex::encode(&self.0));
        serializer.serialize_str(&hex_str)
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(&s);
        hex::decode(s)
            .map(HexBytes)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for HexBytes {
    fn schema_name() -> Cow<'static, str> {
        "HexBytes".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::HexBytes").into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^0x[0-9a-f]*$"
        })
    }
}

impl From<Vec<u8>> for HexBytes {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<HexBytes> for Vec<u8> {
    fn from(h: HexBytes) -> Self {
        h.0
    }
}

impl AsRef<[u8]> for HexBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Fixed-size 32-byte event topic. Serializes as `"0x..."` hex string.
///
/// Hogd supports up to 3 topics per event, each exactly 32 bytes.
/// Topics are indexed and will be filterable in Phase 4.
///
/// ```json
/// "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Topic(pub [u8; 32]);

impl JsonSchema for Topic {
    fn schema_name() -> Cow<'static, str> {
        "Topic".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::Topic").into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^0x[0-9a-f]{64}$"
        })
    }
}

impl Serialize for Topic {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex_str = format!("0x{}", hex::encode(self.0));
        serializer.serialize_str(&hex_str)
    }
}

impl<'de> Deserialize<'de> for Topic {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(&s);
        let bytes = hex::decode(s).map_err(serde::de::Error::custom)?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("topic must be exactly 32 bytes"))?;
        Ok(Topic(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_bytes_roundtrip() {
        let original = HexBytes(vec![0xca, 0xfe, 0xba, 0xbe]);
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#""0xcafebabe""#);

        let decoded: HexBytes = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hex_bytes_empty() {
        let original = HexBytes(vec![]);
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#""0x""#);

        let decoded: HexBytes = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hex_bytes_accepts_no_prefix() {
        let decoded: HexBytes = serde_json::from_str(r#""cafebabe""#).unwrap();
        assert_eq!(decoded.0, vec![0xca, 0xfe, 0xba, 0xbe]);
    }

    #[test]
    fn topic_roundtrip() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xde;
        bytes[1] = 0xad;
        bytes[31] = 0xff;
        let original = Topic(bytes);

        let json = serde_json::to_string(&original).unwrap();
        assert!(json.starts_with(r#""0xdead"#));
        assert!(json.ends_with(r#"ff""#));
        assert_eq!(json.len(), 2 + 64 + 2); // quotes + 0x + 64 hex chars

        let decoded: Topic = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn topic_wrong_length_fails() {
        let result: Result<Topic, _> = serde_json::from_str(r#""0xcafe""#);
        assert!(result.is_err());
    }

    #[test]
    fn hex0x_sha256_roundtrip() {
        use groundhog_xdr::types::SHA256Hash;

        let mut bytes = [0u8; 32];
        bytes[0] = 0xab;
        bytes[31] = 0xcd;
        let original: Hex0x<SHA256Hash> = Hex0x(SHA256Hash::from(bytes));

        let json = serde_json::to_string(&original).unwrap();
        assert!(json.starts_with(r#""0xab"#));
        assert!(json.ends_with(r#"cd""#));
        assert_eq!(json.len(), 2 + 64 + 2); // quotes + 0x + 64 hex chars

        let decoded: Hex0x<SHA256Hash> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hex0x_accepts_no_prefix() {
        use groundhog_xdr::types::SHA256Hash;

        let hex_str = format!(r#""{}""#, "ab".to_owned() + &"00".repeat(30) + "cd");
        let decoded: Hex0x<SHA256Hash> = serde_json::from_str(&hex_str).unwrap();
        assert_eq!(decoded.0.as_ref()[0], 0xab);
        assert_eq!(decoded.0.as_ref()[31], 0xcd);
    }

    #[test]
    fn hex0x_wrong_length_fails() {
        use groundhog_xdr::types::SHA256Hash;

        let result: Result<Hex0x<SHA256Hash>, _> = serde_json::from_str(r#""0xcafe""#);
        assert!(result.is_err());
    }
}
