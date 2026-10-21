//! Typed native JSON-RPC calls over a caller-owned HTTP transport.
//!
//! [`NativeClient`] sends named parameters and preserves unsupported response values
//! through [`DecodedResponse`]. For transaction observation use
//! [`poll_receipt`]; it retains commitment and never submits again.
//! The separate [`receipt`] module polls explicit adapters into stable report models.
//!
//! ```no_run
//! use thruput_json_rpc_client::{DecodedResponse, HttpClientBuilder, NativeClient, NativeClientError};
//! use thruput_json_rpc_client::types::{requests::EmptyRequest, responses::ChainTip};
//!
//! async fn chain_tip() -> Result<DecodedResponse<ChainTip>, NativeClientError> {
//!     let http = HttpClientBuilder::default().build("http://127.0.0.1:4646")?;
//!     NativeClient::new(http).get_chain_tip(EmptyRequest {}).await
//! }
//! ```

pub use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
pub use native::{NativeClient, NativeClientError};
pub use thruput_json_rpc_types as types;
pub use thruput_json_rpc_types::error::native::DecodedError;
pub use thruput_json_rpc_types::responses::DecodedResponse;

pub mod native;
pub mod receipt;

pub use native::receipt::{
    KnownCommitment, ReceiptEvidence, ReceiptPollCause, ReceiptPollConfig, ReceiptPollError,
    ReceiptSource, poll_receipt,
};
