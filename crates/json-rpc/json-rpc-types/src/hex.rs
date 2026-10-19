//! Hex-encoded byte types for the JSON-RPC wire format.
//!
//! Opaque payloads, raw topics, storage keys and cursors use `0x`-prefixed
//! lowercase hex strings (e.g., `"0xa1b2c3"`). Typed identifiers and
//! cryptographic values use the wrappers in [`crate::base58`].
//!
//! See `docs/gateway/system-design.md` "Wire Format Conventions" for the full
//! serialization table.

use std::borrow::Cow;
use std::fmt;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};

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
/// serialize through serde should use [`HexBytes`], [`Hex0x`], or
/// [`HexDisplay`] instead, which never allocate the intermediate string.
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
    const NIBBLES: [u8; 256] = {
        let mut values = [u8::MAX; 256];
        let mut i = 0u8;
        while i < 10 {
            values[(b'0' + i) as usize] = i;
            i += 1;
        }
        i = 0;
        while i < 6 {
            values[(b'a' + i) as usize] = 10 + i;
            values[(b'A' + i) as usize] = 10 + i;
            i += 1;
        }
        values
    };
    let text = strip_hex_prefix(text);
    if !text.len().is_multiple_of(2) {
        return Err(hex::FromHexError::OddLength);
    }

    // Decode directly into one allocation instead of collecting fallible
    // iterator items. Invalid input uses the reference decoder so its error
    // character, byte offset and precedence stay exactly the same.
    let mut decoded = vec![0; text.len() / 2];
    for (pair, byte) in text.as_bytes().chunks_exact(2).zip(&mut decoded) {
        let high = NIBBLES[usize::from(pair[0])];
        let low = NIBBLES[usize::from(pair[1])];
        if high | low == u8::MAX {
            return hex::decode(text);
        }
        *byte = high << 4 | low;
    }
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
// Hex0x<T> — generic 0x-prefixed hex wrapper for fixed-size XDR byte types
// ---------------------------------------------------------------------------

/// Wraps an XDR byte type (e.g., `SHA256Hash`, `ContractAddress`) and
/// serializes it as a `"0x..."` lowercase hex string.
///
/// The inner type must implement `AsRef<[u8]>` (for serialization) and
/// `From<[u8; N]>` (for deserialization). This is true for all
/// `thruput-xdr` fixed-size byte types.
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
        let bytes = decode_prefixed_hex(&s).map_err(serde::de::Error::custom)?;
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

impl_try_from_slice_for!(thruput_xdr::types::SHA256Hash, 32);
impl_try_from_slice_for!(thruput_xdr::types::ContractAddress, 32);
impl_try_from_slice_for!(thruput_xdr::types::Ed25519PublicKey, 32);
impl_try_from_slice_for!(thruput_xdr::types::PublicKeyHash, 32);
impl_try_from_slice_for!(thruput_xdr::types::StorageLongHash, 2048);
impl_try_from_slice_for!(thruput_xdr::validator::ValidatorID, 32);

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
