// Typed JSON-RPC client, generated from the shared rpc-api trait.
//
// Re-exports api-types so consumers (SDK, CLI) only need this crate.
pub use groundhog_rpc_api_types as types;

pub use groundhog_rpc_api::GroundhogApiClient;
pub use jsonrpsee::http_client::HttpClientBuilder;
