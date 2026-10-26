//! Byte-preserving text codecs. Network identity and account derivation are separate.
//!
//! Machine interfaces use base58 only. The typed human-input helpers also accept
//! Thruput Z addresses and Stellar G/S `StrKeys`. No function guesses that an
//! arbitrary byte array is an address. Z identifies Thruput, not a specific network.

use thruput_xdr::base58;
use zeroize::Zeroizing;

pub use base58::max_encoded_len;

// Top five bits select Z in RFC 4648 base32; the low three bits are zero.
const ADDRESS_STRKEY_VERSION: u8 = 25 << 3;

/// Invalid text or a violated byte-length limit. Never contains the supplied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The input contains a character outside the Bitcoin/Solana alphabet.
    #[error("invalid base58 encoding")]
    InvalidBase58,
    /// Conversion would exceed the caller's decoded byte limit.
    #[error("base58 value exceeds the {max_bytes}-byte limit")]
    TooLong {
        /// Caller-specified decoded byte limit.
        max_bytes: usize,
    },
    /// A fixed-size field decoded to an unexpected byte length.
    #[error("expected {expected} bytes, received {actual}")]
    InvalidLength {
        /// Field's required byte length.
        expected: usize,
        /// Actual decoded length.
        actual: usize,
    },
    /// A typed human input was neither the expected `StrKey` nor a base58 value.
    #[error("expected a 32-byte base58 value or a valid {prefix} StrKey")]
    InvalidTypedValue {
        /// Expected `StrKey` prefix.
        prefix: char,
    },
}

/// Encode using the case-sensitive Bitcoin/Solana alphabet, preserving leading zeros.
pub fn encode(bytes: &[u8]) -> String {
    let mut output = vec![0; max_encoded_len(bytes.len())];
    let len = base58::encode_into(bytes, &mut output)
        .expect("bug: base58 encoded length exceeds its bound");
    output.truncate(len);
    String::from_utf8(output).expect("bug: base58 encoding is not ASCII")
}

fn decode_error(error: base58::Error, max_bytes: usize) -> Error {
    match error {
        base58::Error::BufferTooSmall => Error::TooLong { max_bytes },
        base58::Error::InvalidLength { expected, actual } => {
            Error::InvalidLength { expected, actual }
        }
        _ => Error::InvalidBase58,
    }
}

/// Decode a machine value with a required maximum byte length.
///
/// Checks encoded length before conversion, bounds allocation by the input and
/// decoded limit, and rejects excess decoded bytes. Whitespace is not trimmed.
/// The caller must select a field-appropriate limit; this is not a CPU time limit.
pub fn decode(text: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
    if text.len() > max_encoded_len(max_bytes) {
        return Err(Error::TooLong { max_bytes });
    }
    // A base58 character contributes at most one decoded byte (including zeros).
    let mut output = vec![0; text.len().min(max_bytes)];
    let len =
        base58::decode_into(text, &mut output).map_err(|error| decode_error(error, max_bytes))?;
    output.truncate(len);
    Ok(output)
}

/// Decode exactly N bytes without a heap-allocated intermediate byte buffer.
pub fn decode_fixed<const N: usize>(text: &str) -> Result<[u8; N], Error> {
    base58::decode_fixed(text).map_err(|error| decode_error(error, N))
}

fn decode_fixed_into(text: &str, output: &mut [u8]) -> Result<(), Error> {
    let actual =
        base58::decode_into(text, output).map_err(|error| decode_error(error, output.len()))?;
    if actual != output.len() {
        return Err(Error::InvalidLength {
            expected: output.len(),
            actual,
        });
    }
    Ok(())
}

/// Parse a Thruput address from base58 or Z `StrKey`, without deriving an account.
///
/// Stellar C addresses are rejected, even when they encode the same bytes.
pub fn parse_address(text: &str) -> Result<[u8; 32], Error> {
    if text.len() == 56
        && let Ok((version, payload)) = stellar_strkey::convert::decode::<32, 35>(text.as_bytes())
        && version == ADDRESS_STRKEY_VERSION
        && let Ok(bytes) = <[u8; 32]>::try_from(payload.as_slice())
        && address_strkey(&bytes) == text
    {
        return Ok(bytes);
    }
    decode_fixed(text).map_err(|_| Error::InvalidTypedValue { prefix: 'Z' })
}

/// Parse an Ed25519 public key from base58 or G `StrKey`, never a wallet address.
pub fn parse_public_key(text: &str) -> Result<[u8; 32], Error> {
    if let Ok(key) = text.parse::<stellar_strkey::ed25519::PublicKey>() {
        return Ok(key.0);
    }
    decode_fixed(text).map_err(|_| Error::InvalidTypedValue { prefix: 'G' })
}

/// Parse a 32-byte Ed25519 seed from base58 or S `StrKey`.
///
/// Decoding buffers are zeroed on drop. Callers must also protect their input text.
pub fn parse_secret_seed(text: &str) -> Result<Zeroizing<[u8; 32]>, Error> {
    if let Ok(key) = text.parse::<stellar_strkey::ed25519::PrivateKey>() {
        return Ok(Zeroizing::new(key.0));
    }
    let mut output = Zeroizing::new([0; 32]);
    decode_fixed_into(text, output.as_mut_slice())
        .map_err(|_| Error::InvalidTypedValue { prefix: 'S' })?;
    Ok(output)
}

/// Format the same address bytes as a Thruput Z `StrKey`.
///
/// The prefix does not distinguish local, test, and production networks.
pub fn address_strkey(bytes: &[u8; 32]) -> String {
    stellar_strkey::convert::encode::<32, 35, 56>(ADDRESS_STRKEY_VERSION, bytes).to_string()
}

/// Format an Ed25519 key as a G `StrKey`. No wallet derivation is performed.
pub fn public_key_strkey(bytes: &[u8; 32]) -> String {
    stellar_strkey::ed25519::PublicKey(*bytes)
        .to_string()
        .as_str()
        .to_owned()
}

/// Explicitly export an Ed25519 seed as an S `StrKey` in a zeroing buffer.
pub fn secret_seed_strkey(bytes: &[u8; 32]) -> Zeroizing<String> {
    let key = stellar_strkey::ed25519::PrivateKey(*bytes);
    let mut output = Zeroizing::default();
    key.as_unredacted().write_string(&mut output);
    Zeroizing::new(output.as_str().to_owned())
}

#[cfg(test)]
mod tests;
