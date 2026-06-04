// Typed JSON-RPC client, generated from the shared json-rpc-api trait.
//
// Re-exports json-rpc-types so consumers (SDK, CLI) only need this crate.
pub use groundhog_json_rpc_types as types;

pub use groundhog_json_rpc_api::GroundhogApiClient;
pub use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
