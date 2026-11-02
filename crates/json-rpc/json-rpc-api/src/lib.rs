//! Shared JSON-RPC contracts. Production endpoints use [`native::NativeApi`].

pub use thruput_json_rpc_types as types;
pub mod params;

/// Wire-format `SemVer` for the JSON-RPC surface.
///
/// It is versioned independently of the Cargo workspace. Bump major for
/// breaking method, parameter, result, or error changes; minor for
/// additions; patch for documentation or example refinements.
pub const WIRE_FORMAT_VERSION: &str = "14.3.0";

pub mod native;
