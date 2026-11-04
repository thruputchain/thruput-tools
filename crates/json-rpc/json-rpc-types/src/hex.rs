//! Hex-encoded byte types for the JSON-RPC wire format.
//!
//! Opaque payloads, raw topics, storage keys and cursors use `0x`-prefixed
//! lowercase hex strings (e.g., `"0xa1b2c3"`). Typed identifiers and
//! cryptographic values use the wrappers in [`crate::base58`].
//!
//! The generated [JSON-RPC reference](https://github.com/thruputchain/thruput-tools/blob/main/crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.md)
//! specifies each field's encoding.

use std::borrow::Cow;
use std::fmt;
use std::mem::MaybeUninit;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// Formats bytes as `0x`-prefixed lowercase hex without an intermediate
/// `String`.
///
/// Serializers such as `serde_json` write `collect_str` output straight into
/// their buffer, so this streams bounded chunks instead of building a complete
/// `hex::encode` string and a second allocation to prepend `0x`. It also
/// serves `format_args!` callers that only need to print the value.
#[derive(Debug)]
pub struct HexDisplay<'a>(pub &'a [u8]);

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
            // SAFETY: every initialized byte comes from DIGITS, which is ASCII.
            let text = unsafe { std::str::from_utf8_unchecked(initialized) };
            f.write_str(text)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared 0x-prefixed hex helpers
// ---------------------------------------------------------------------------

/// Encode bytes as a `0x`-prefixed lowercase hex string.
///
/// This is the one spelling of the wire convention for callers that need an
/// owned `String` (log lines, CLI output, JSON built with `json!`). Types that
/// serialize through serde should use [`HexBytes`] or [`HexDisplay`] instead, which never allocate the intermediate string.
pub fn prefixed_hex(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    // Writing to a String cannot fail.
    let _ = fmt::Write::write_fmt(&mut out, format_args!("{}", HexDisplay(bytes)));
    out
}

/// Strip an optional `0x` or `0X` prefix from a hex string.
pub fn strip_hex_prefix(text: &str) -> &str {
    text.strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text)
}

/// Decode a hex string with an optional `0x` or `0X` prefix.
pub fn decode_prefixed_hex(text: &str) -> Result<Vec<u8>, hex::FromHexError> {
    let text = strip_hex_prefix(text);
    let mut decoded = vec![0; text.len() / 2];
    hex::decode_to_slice(text, &mut decoded)?;
    Ok(decoded)
}

/// Decode a hex string with an optional `0x` or `0X` prefix into exactly
/// `N` bytes.
pub fn decode_fixed_hex<const N: usize>(text: &str) -> Result<[u8; N], FixedHexError> {
    let bytes = decode_prefixed_hex(text)?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| FixedHexError::InvalidLength { len, expected: N })
}

/// Error returned when a hex string does not decode to the expected byte length.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FixedHexError {
    /// The text is not valid hex.
    #[error("invalid hex: {0}")]
    InvalidHex(#[from] hex::FromHexError),
    /// The text decoded to the wrong number of bytes.
    #[error("invalid length {len}, expected {expected} bytes ({} hex characters)", .expected * 2)]
    InvalidLength {
        /// Decoded byte length.
        len: usize,
        /// Required byte length.
        expected: usize,
    },
}

/// Encode the SHA-256 digest of `bytes` as a `0x`-prefixed lowercase hex string.
///
/// Manifest and artifact hashes cross the wire in this form.
pub fn sha256_prefixed_hex(bytes: &[u8]) -> String {
    prefixed_hex(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Variable-length byte types
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
        decode_prefixed_hex(&s)
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
/// Topics are indexed and filterable.
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
        decode_fixed_hex(&s)
            .map(Topic)
            .map_err(|error| match error {
                FixedHexError::InvalidHex(error) => serde::de::Error::custom(error),
                FixedHexError::InvalidLength { .. } => {
                    serde::de::Error::custom("topic must be exactly 32 bytes")
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_prefixed_hex, strip_hex_prefix};

    #[test]
    fn decoding_matches_reference_for_ascii_pairs() {
        for first in 0..=127u8 {
            for second in 0..=127u8 {
                let bytes = [first, second];
                let text = std::str::from_utf8(&bytes).unwrap();
                for prefix in ["", "0x", "0X"] {
                    let input = format!("{prefix}{text}");
                    assert_eq!(
                        decode_prefixed_hex(&input),
                        hex::decode(strip_hex_prefix(&input))
                    );
                }
            }
        }
    }

    #[test]
    fn decoding_preserves_errors_and_round_trips_across_chunk_lengths() {
        for text in ["", "0", "g", "0g0", "gg", "01zz", "000x", "é", "😀", "0é0"] {
            for prefix in ["", "0x", "0X"] {
                assert_eq!(
                    decode_prefixed_hex(&format!("{prefix}{text}")),
                    hex::decode(text)
                );
            }
        }
        let bytes = (0..=u8::MAX).cycle().take(1025).collect::<Vec<_>>();
        for len in 0..=bytes.len() {
            for text in [hex::encode(&bytes[..len]), hex::encode_upper(&bytes[..len])] {
                assert_eq!(decode_prefixed_hex(&text).unwrap(), bytes[..len]);
            }
        }
    }
}
