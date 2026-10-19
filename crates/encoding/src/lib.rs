//! Byte-preserving text codecs. Network identity and account derivation are separate.
//!
//! Machine interfaces use base58 only. The typed human-input helpers also accept
//! Stellar `StrKeys`. No function guesses that an arbitrary byte array is an address.

use zeroize::Zeroizing;

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

/// Conservative maximum base58 text length for a decoded byte limit.
///
/// Saturates rather than wrapping for limits near `usize::MAX`; empty bytes have
/// exactly one representation, the empty string.
pub const fn max_encoded_len(max_bytes: usize) -> usize {
    if max_bytes == 0 {
        return 0;
    }
    // Decompose multiplication so valid bounds do not overflow unnecessarily.
    (max_bytes / 100)
        .saturating_mul(138)
        .saturating_add((max_bytes % 100) * 138 / 100)
        .saturating_add(1)
}

/// Encode using the case-sensitive Bitcoin/Solana alphabet, preserving leading zeros.
pub fn encode(bytes: &[u8]) -> String {
    bs58::encode(bytes).into_string()
}

fn check_length(text: &str, max_bytes: usize) -> Result<(), Error> {
    if text.len() > max_encoded_len(max_bytes) {
        return Err(Error::TooLong { max_bytes });
    }
    Ok(())
}

fn decode_into(text: &str, output: &mut [u8]) -> Result<usize, Error> {
    let max_bytes = output.len();
    bs58::decode(text)
        .onto(output)
        .map_err(|error| match error {
            bs58::decode::Error::BufferTooSmall => Error::TooLong { max_bytes },
            _ => Error::InvalidBase58,
        })
}

/// Decode a machine value with a required maximum byte length.
///
/// Checks encoded length before conversion, bounds allocation by the input and
/// decoded limit, and rejects excess decoded bytes. Whitespace is not trimmed.
/// The caller must select a field-appropriate limit; this is not a CPU time limit.
pub fn decode(text: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
    check_length(text, max_bytes)?;
    // A base58 character contributes at most one decoded byte (including zeros).
    let mut output = vec![0; text.len().min(max_bytes)];
    let len = decode_into(text, &mut output)?;
    output.truncate(len);
    Ok(output)
}

/// Decode exactly N bytes without a heap-allocated intermediate byte buffer.
pub fn decode_fixed<const N: usize>(text: &str) -> Result<[u8; N], Error> {
    let mut output = [0; N];
    decode_fixed_into(text, &mut output)?;
    Ok(output)
}

fn decode_fixed_into(text: &str, output: &mut [u8]) -> Result<(), Error> {
    check_length(text, output.len())?;
    let actual = decode_into(text, output)?;
    if actual != output.len() {
        return Err(Error::InvalidLength {
            expected: output.len(),
            actual,
        });
    }
    Ok(())
}

/// Parse a Thruput address from base58 or C `StrKey`, without deriving an account.
pub fn parse_address(text: &str) -> Result<[u8; 32], Error> {
    if let Ok(key) = text.parse::<stellar_strkey::Contract>() {
        return Ok(key.0);
    }
    decode_fixed(text).map_err(|_| Error::InvalidTypedValue { prefix: 'C' })
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

/// Format the same address bytes as a C `StrKey`. No network is encoded.
pub fn address_strkey(bytes: &[u8; 32]) -> String {
    stellar_strkey::Contract(*bytes)
        .to_string()
        .as_str()
        .to_owned()
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
