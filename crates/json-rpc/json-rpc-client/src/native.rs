//! Typed native calls without retry or receipt-polling policy.
//!
//! These adapters do not select a wire version or replace legacy clients. The
//! caller chooses an endpoint implementing the candidate native contract.

use jsonrpsee::core::client::{ClientT, Error as TransportError};
use jsonrpsee::types::ErrorObjectOwned;
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_types::{error::native::DecodedError, responses::DecodedResponse};

/// Native call failure. Application evidence never implies a recovery action.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativeClientError {
    /// JSON-RPC error with method-aware classification. Preserve the code,
    /// message and complete data supplied by jsonrpsee, including unknown data.
    /// Extra top-level error members discarded by that transport are unavailable.
    #[error("{source}")]
    Application {
        #[source]
        source: ErrorObjectOwned,
        data: Box<DecodedError>,
    },
    /// Transport, protocol or malformed-known-result failure. This says nothing
    /// about transaction admission or execution; preserve existing uncertainty.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl NativeClientError {
    fn from_call(method: &str, error: TransportError) -> Self {
        match error {
            TransportError::Call(source) => match serde_json::to_value(&source) {
                Ok(raw) => Self::Application {
                    source,
                    data: Box::new(DecodedError::decode(method, raw)),
                },
                Err(error) => Self::Transport(TransportError::ParseError(error)),
            },
            other => Self::Transport(other),
        }
    }
}

/// A native adapter around a caller-owned JSON-RPC transport. Calls submit
/// exactly once. Unknown result variants retain their complete raw result.
#[derive(Debug, Clone)]
pub struct NativeClient<C> {
    inner: C,
}

impl<C> NativeClient<C> {
    /// Wrap an existing transport without connecting or changing its settings.
    pub fn new(inner: C) -> Self {
        Self { inner }
    }

    /// Recover the caller's transport.
    pub fn into_inner(self) -> C {
        self.inner
    }
}

macro_rules! client_methods {
    ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
        impl<C: ClientT + Sync> NativeClient<C> {
            $(
                $(#[$doc])*
                pub async fn $name(&self, request: $request)
                    -> Result<DecodedResponse<$response>, NativeClientError>
                {
                    self.inner.request($wire, NamedParams(request)).await
                        .map_err(|error| NativeClientError::from_call($wire, error))
                }
            )*
        }
    };
}
thruput_json_rpc_api::native_methods!(client_methods);
