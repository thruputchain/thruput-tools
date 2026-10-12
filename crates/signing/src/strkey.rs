//! Text encodings shared by native clients. `StrKeys` encode type and checksum, not network.
//! Contract addresses, public keys and private seeds are deliberately separate operations.

use thruput_xdr::types::ContractAddress;
use zeroize::Zeroizing;

/// Invalid encoding, checksum, length or `StrKey` type. Never includes the supplied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Expected a contract address, never an Ed25519 public key.
    #[error(
        "contract address must be a C StrKey or 64 hex characters, optionally prefixed with 0x"
    )]
    ContractAddress,
    /// Expected an Ed25519 public key, never a contract address.
    #[error(
        "Ed25519 public key must be a G StrKey or 64 hex characters, optionally prefixed with 0x"
    )]
    PublicKey,
    /// Expected the 32-byte private seed, not a stored 64-byte keypair.
    #[error("expected an S StrKey or 32-byte hex Ed25519 seed")]
    SecretSeed,
}

fn decode_hex(value: &str, output: &mut [u8; 32]) -> Result<(), hex::FromHexError> {
    hex::decode_to_slice(
        value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value),
        output,
    )
}

/// Parse a checksummed C `StrKey` or legacy 32-byte hex address (with optional prefix).
pub fn parse_contract_address(value: &str) -> Result<ContractAddress, Error> {
    if let Ok(key) = value.parse::<stellar_strkey::Contract>() {
        return Ok(key.0.into());
    }
    let mut bytes = [0; 32];
    decode_hex(value, &mut bytes).map_err(|_| Error::ContractAddress)?;
    Ok(bytes.into())
}

/// Encode the same contract address bytes as a C `StrKey`; no address derivation is performed.
pub fn format_contract_address(address: &ContractAddress) -> String {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(address.as_ref());
    stellar_strkey::Contract(bytes)
        .to_string()
        .as_str()
        .to_owned()
}

/// Parse a G `StrKey` or legacy 32-byte hex public key. Does not derive an account address.
pub fn parse_public_key(value: &str) -> Result<[u8; 32], Error> {
    if let Ok(key) = value.parse::<stellar_strkey::ed25519::PublicKey>() {
        return Ok(key.0);
    }
    let mut bytes = [0; 32];
    decode_hex(value, &mut bytes).map_err(|_| Error::PublicKey)?;
    Ok(bytes)
}

/// Encode raw Ed25519 public key bytes as a G `StrKey`, not a wallet address.
pub fn format_public_key(key: &[u8; 32]) -> String {
    stellar_strkey::ed25519::PublicKey(*key)
        .to_string()
        .as_str()
        .to_owned()
}

/// Parse an S `StrKey` or 32-byte hex seed. Secret buffers are zeroed on drop.
/// The caller must also protect and clear the input string it owns.
pub fn parse_secret_seed(value: &str) -> Result<Zeroizing<[u8; 32]>, Error> {
    if let Ok(key) = value.parse::<stellar_strkey::ed25519::PrivateKey>() {
        return Ok(Zeroizing::new(key.0));
    }
    let mut bytes = Zeroizing::new([0; 32]);
    decode_hex(value, &mut bytes).map_err(|_| Error::SecretSeed)?;
    Ok(bytes)
}

/// Explicitly export a private seed as an S `StrKey`. The returned buffer is zeroed on drop.
pub fn format_secret_seed(seed: &[u8; 32]) -> Zeroizing<String> {
    let key = stellar_strkey::ed25519::PrivateKey(*seed);
    let mut encoded = Zeroizing::default();
    key.as_unredacted().write_string(&mut encoded);
    Zeroizing::new(encoded.as_str().to_owned())
}
