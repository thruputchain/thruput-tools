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
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};

use groundhog_xdr::types::{NetworkHash, SHA256Hash};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A canonical lowercase, unprefixed 32-byte network hash.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetworkHashHex(String);

impl NetworkHashHex {
    /// Construct a network hash from its raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }

    /// Borrow the canonical wire representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The protocol network hash this wire value encodes.
    ///
    /// Callers signing transactions need this: the network hash is part of
    /// every signature preimage.
    pub fn to_network_hash(&self) -> NetworkHash {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(&self.0, &mut bytes)
            .expect("bug: NetworkHashHex is validated as 64 lowercase hex digits on construction");
        NetworkHash::new(SHA256Hash::new(bytes))
    }
}

impl TryFrom<String> for NetworkHashHex {
    type Error = NetworkHashHexError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(Self(value));
        }
        Err(NetworkHashHexError)
    }
}

impl TryFrom<&str> for NetworkHashHex {
    type Error = NetworkHashHexError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl From<[u8; 32]> for NetworkHashHex {
    fn from(value: [u8; 32]) -> Self {
        Self::from_bytes(value)
    }
}

impl AsRef<str> for NetworkHashHex {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for NetworkHashHex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for NetworkHashHex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("NetworkHashHex").field(&self.0).finish()
    }
}

impl Serialize for NetworkHashHex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NetworkHashHex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for NetworkHashHex {
    fn schema_name() -> Cow<'static, str> {
        "NetworkHashHex".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::NetworkHashHex").into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$"
        })
    }
}

/// Error returned for a non-canonical network hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("networkHash must be exactly 64 lowercase hexadecimal characters without a 0x prefix")]
pub struct NetworkHashHexError;

// Stream bounded chunks to serializers such as serde_json, whose collect_str
// implementation writes directly into the output buffer. Avoid both a complete
// hex::encode String and the second allocation used to prepend "0x".
struct HexDisplay<'a>(&'a [u8]);

impl fmt::Display for HexDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        f.write_str("0x")?;
        let mut encoded = [MaybeUninit::<u8>::uninit(); 512];
        for chunk in self.0.chunks(encoded.len() / 2) {
            let output = &mut encoded[..chunk.len() * 2];
            for (&byte, pair) in chunk.iter().zip(output.chunks_exact_mut(2)) {
                pair[0].write(DIGITS[usize::from(byte >> 4)]);
                pair[1].write(DIGITS[usize::from(byte & 0x0f)]);
            }
            // SAFETY: output has exactly two slots per input byte. The loop
            // above writes both slots of every pair before this reference is
            // created. MaybeUninit<u8> has u8's layout and alignment, and the
            // slice excludes the unused tail of encoded. No mutation occurs
            // while these bytes are borrowed by the formatter.
            let initialized =
                unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<u8>(), output.len()) };
            let text = std::str::from_utf8(initialized)
                .expect("bug: hex encoding contains only ASCII digits");
            f.write_str(text)?;
        }
        Ok(())
    }
}

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
        serializer.collect_str(&HexDisplay(self.0.as_ref()))
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

/// Fixed byte length of an XDR type represented by [`Hex0x`].
pub trait FixedHexLength {
    /// Number of bytes in the canonical representation.
    const BYTE_LENGTH: usize;
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

        impl FixedHexLength for $ty {
            const BYTE_LENGTH: usize = $n;
        }
    };
}

impl_try_from_slice_for!(groundhog_xdr::types::SHA256Hash, 32);
impl_try_from_slice_for!(groundhog_xdr::types::ContractAddress, 32);
impl_try_from_slice_for!(groundhog_xdr::types::Ed25519PublicKey, 32);
impl_try_from_slice_for!(groundhog_xdr::types::StorageLongHash, 2048);
impl_try_from_slice_for!(groundhog_xdr::validator::ValidatorID, 32);

impl<T: FixedHexLength + JsonSchema> JsonSchema for Hex0x<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Hex0x_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("{}::Hex0x<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        let pattern = format!("^0x[0-9a-f]{{{}}}$", T::BYTE_LENGTH * 2);
        json_schema!({
            "type": "string",
            "pattern": pattern
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
        serializer.collect_str(&HexDisplay(&self.0))
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
        serializer.collect_str(&HexDisplay(&self.0))
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
    fn hex_serialization_matches_reference_across_chunk_boundaries() {
        for len in (0..=257).chain([511, 512, 513, 2048, 65536]) {
            let bytes: Vec<u8> = (0..=255).cycle().take(len).collect();
            let expected = format!("\"0x{}\"", hex::encode(&bytes));
            assert_eq!(
                serde_json::to_string(&HexBytes(bytes.clone())).unwrap(),
                expected
            );
            assert_eq!(serde_json::to_string(&Hex0x(bytes)).unwrap(), expected);
        }
        let bytes = std::array::from_fn(|i| u8::try_from(i * 8).unwrap());
        assert_eq!(
            serde_json::to_string(&Topic(bytes)).unwrap(),
            format!("\"0x{}\"", hex::encode(bytes))
        );
    }

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
