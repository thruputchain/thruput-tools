pub mod block;
pub mod error;
pub mod events;
pub mod hex;
pub mod receipt;
pub mod transaction;

// Re-export XDR types that appear in the public API unchanged.
pub use groundhog_xdr::block::BlockHeader;
pub use groundhog_xdr::types::{BlockHeight, ContractAddress, Ed25519PublicKey, SHA256Hash};
