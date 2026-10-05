pub mod airdrop;
pub mod balance;
pub mod block;
pub mod bootstrap_manifest;
pub mod decimal;
pub mod dev;
pub mod error;
pub mod events;
pub mod hex;
pub mod network_config;
pub mod network_descriptor;
pub mod network_info;
pub mod protocol_token;
pub mod receipt;
pub mod runtime_manifest;
pub mod server_limits;
pub mod state;
pub mod transaction;
pub mod version;
pub mod view;

use hex::Hex0x;

pub use hex::NetworkHashHex;

// Re-export XDR types that don't need 0x wrapping.
pub use groundhog_scheduler::consensus::ConsensusValue;
pub use groundhog_xdr::types::{BlockHeight, Protocol, TimePoint};

// Re-export 0x-wrapped versions of XDR byte types for the API boundary.
// These serialize/deserialize with "0x" prefix in JSON.
//
// Hash newtypes are represented as SHA256Hash at the API layer. They carry the
// same 32 bytes while retaining `0x` JSON encoding.
pub type SHA256Hash = Hex0x<groundhog_xdr::types::SHA256Hash>;
pub type ContractAddress = Hex0x<groundhog_xdr::types::ContractAddress>;
pub type BlockHash = Hex0x<groundhog_xdr::types::SHA256Hash>;
pub type NetworkHash = Hex0x<groundhog_xdr::types::SHA256Hash>;
pub type StorageHash = Hex0x<groundhog_xdr::types::SHA256Hash>;
pub type DbHash = Hex0x<groundhog_xdr::types::StorageLongHash>;
pub type Ed25519PublicKey = Hex0x<groundhog_xdr::types::Ed25519PublicKey>;
pub type ValidatorID = Hex0x<groundhog_xdr::validator::ValidatorID>;
