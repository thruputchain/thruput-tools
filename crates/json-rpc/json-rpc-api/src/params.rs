//! Client serialization of a whole named request object.

use jsonrpsee::core::traits::ToRpcParams;
use serde::Serialize;
use serde_json::value::{RawValue, to_raw_value};

/// Emit a request object directly as `params`, without an argument-name wrapper.
///
/// Non-object values fail locally. This keeps transport dependencies out of the
/// shared request types and works with `ClientT::request`.
#[derive(Debug, Clone, Copy)]
pub struct NamedParams<T>(pub T);

impl<T: Serialize> ToRpcParams for NamedParams<T> {
    fn to_rpc_params(self) -> Result<Option<Box<RawValue>>, serde_json::Error> {
        let raw = to_raw_value(&self.0)?;
        if !raw.get().starts_with('{') {
            return Err(serde::ser::Error::custom("named params must be an object"));
        }
        Ok(Some(raw))
    }
}
