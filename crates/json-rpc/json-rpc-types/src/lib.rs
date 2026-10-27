//! Shared JSON-RPC wire types for Thruput.
//!
//! [`requests`], [`responses`] and [`error::native`] define the wire contract
//! that the server, the typed client and the `OpenRPC` generator share. The
//! remaining modules hold the backend-side projections that server adapters
//! build those wire types from, plus the shared `base58`, `hex` and `decimal`
//! codecs.

pub mod airdrop;
pub mod base58;
pub mod block;
pub mod bootstrap_manifest;
pub mod decimal;
pub mod error;
pub mod events;
pub mod hex;
pub mod network_config;
pub mod network_descriptor;
pub mod network_info;
pub mod protocol_token;
pub mod receipt;
pub mod requests;
pub mod responses;
pub mod runtime_manifest;
pub mod transaction;
pub mod view;

use base58::Base58;

pub use base58::NetworkHashText;

// Re-export non-byte XDR types.
pub use thruput_node_rpc::consensus::ConsensusValue;
pub use thruput_xdr::types::{BlockHeight, Protocol, TimePoint};

// Typed identifiers use unprefixed base58 at the API boundary.
//
// Hash newtypes are represented as SHA256Hash at the API layer. They carry the
// same 32 bytes with base58 JSON encoding.
pub type SHA256Hash = Base58<thruput_xdr::types::SHA256Hash>;
pub type ContractAddress = Base58<thruput_xdr::types::ContractAddress>;
pub type BlockHash = Base58<thruput_xdr::types::SHA256Hash>;
pub type NetworkHash = Base58<thruput_xdr::types::SHA256Hash>;
pub type StorageHash = Base58<thruput_xdr::types::SHA256Hash>;
pub type Ed25519PublicKey = Base58<thruput_xdr::types::Ed25519PublicKey>;
pub type PublicKeyHash = Base58<thruput_xdr::types::PublicKeyHash>;
pub type ValidatorID = Base58<thruput_xdr::validator::ValidatorID>;
