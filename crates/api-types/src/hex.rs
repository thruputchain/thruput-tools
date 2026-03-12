//! Hex-encoded byte types for the JSON-RPC wire format.
//!
//! All byte values in the Groundhog RPC API are serialized as
//! `0x`-prefixed lowercase hex strings, following the Ethereum
//! convention (e.g., `"0xa1b2c3"`).
//!
//! See `docs/system-design.md` "Wire Format Conventions" for the full
//! serialization table.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
        let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(&s);
        hex::decode(s)
            .map(HexBytes)
            .map_err(serde::de::Error::custom)
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

impl Serialize for Topic {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex_str = format!("0x{}", hex::encode(self.0));
        serializer.serialize_str(&hex_str)
    }
}

impl<'de> Deserialize<'de> for Topic {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(&s);
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
}
