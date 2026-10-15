//! Text encodings shared by native clients. `StrKeys` encode type and checksum, not network.
//! Contract addresses, public keys and private seeds are deliberately separate operations.

use thruput_xdr::types::ContractAddress;
use zeroize::Zeroizing;

/// Invalid encoding, checksum, length or `StrKey` type. Never includes the supplied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Expected a contract address, never an Ed25519 public key.
    #[error("contract address must be a C StrKey or base58 encoding of exactly 32 bytes")]
    ContractAddress,
    /// Expected an Ed25519 public key, never a contract address.
    #[error("Ed25519 public key must be a G StrKey or base58 encoding of exactly 32 bytes")]
    PublicKey,
    /// Expected the 32-byte private seed, not a stored 64-byte keypair.
    #[error("expected an S StrKey or base58 encoding of a 32-byte Ed25519 seed")]
    SecretSeed,
}

/// Parse a checksummed C `StrKey` or base58 address of exactly 32 bytes.
pub fn parse_contract_address(value: &str) -> Result<ContractAddress, Error> {
    thruput_encoding::parse_address(value)
        .map(Into::into)
        .map_err(|_| Error::ContractAddress)
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

/// Parse a G `StrKey` or base58 public key. Does not derive an account address.
pub fn parse_public_key(value: &str) -> Result<[u8; 32], Error> {
    thruput_encoding::parse_public_key(value).map_err(|_| Error::PublicKey)
}

/// Encode raw Ed25519 public key bytes as a G `StrKey`, not a wallet address.
pub fn format_public_key(key: &[u8; 32]) -> String {
    stellar_strkey::ed25519::PublicKey(*key)
        .to_string()
        .as_str()
        .to_owned()
}

/// Parse an S `StrKey` or base58 seed of exactly 32 bytes. Secret buffers are zeroed on drop.
/// The caller must also protect and clear the input string it owns.
pub fn parse_secret_seed(value: &str) -> Result<Zeroizing<[u8; 32]>, Error> {
    thruput_encoding::parse_secret_seed(value).map_err(|_| Error::SecretSeed)
}

/// Explicitly export a private seed as an S `StrKey`. The returned buffer is zeroed on drop.
pub fn format_secret_seed(seed: &[u8; 32]) -> Zeroizing<String> {
    let key = stellar_strkey::ed25519::PrivateKey(*seed);
    let mut encoded = Zeroizing::default();
    key.as_unredacted().write_string(&mut encoded);
    Zeroizing::new(encoded.as_str().to_owned())
}
