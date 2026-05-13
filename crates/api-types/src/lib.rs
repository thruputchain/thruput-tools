pub mod block;
pub mod dev;
pub mod error;
pub mod events;
pub mod hex;
pub mod receipt;
pub mod state;
pub mod transaction;
pub mod version;
pub mod view;

use hex::Hex0x;

// Re-export XDR types that don't need 0x wrapping.
pub use groundhog_xdr::block::BlockHeader;
pub use groundhog_xdr::types::BlockHeight;

// Re-export 0x-wrapped versions of XDR byte types for the API boundary.
// These serialize/deserialize with "0x" prefix in JSON.
//
// BlockHash is represented as SHA256Hash at the API layer — they're the
// same 32 bytes, and this avoids needing AsRef<[u8]> on the BlockHash
// newtype.
pub type SHA256Hash = Hex0x<groundhog_xdr::types::SHA256Hash>;
pub type ContractAddress = Hex0x<groundhog_xdr::types::ContractAddress>;
pub type BlockHash = Hex0x<groundhog_xdr::types::SHA256Hash>;
pub type DbHash = Hex0x<groundhog_xdr::types::DbHash>;
pub type Ed25519PublicKey = Hex0x<groundhog_xdr::types::Ed25519PublicKey>;

#[cfg(test)]
mod tests;
