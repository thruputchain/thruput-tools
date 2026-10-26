//! Shared JSON-RPC contracts. Production endpoints use [`native::NativeApi`].

pub use thruput_json_rpc_types as types;
pub mod params;

/// Wire-format `SemVer` for the JSON-RPC surface.
///
/// See the "`OpenRPC` Wire Version" section in `crates/json-rpc/AGENTS.md`
/// for bump rules.
pub const WIRE_FORMAT_VERSION: &str = "14.1.1";

pub mod native;
