//! Strict native request parsing, independent of backend dispatch.

use jsonrpsee::types::{ErrorObjectOwned, Params};
use serde::de::{DeserializeOwned, IgnoredAny};
use thruput_json_rpc_types::requests::EmptyRequest;

use crate::RpcServerError;

/// Parse a whole named request. Reject arrays before serde can interpret them as
/// positional struct fields. The request type must reject unknown properties.
pub fn parse_named<T: DeserializeOwned>(params: &Params<'_>) -> Result<T, ErrorObjectOwned> {
    if !params.is_object() {
        return Err(RpcServerError::InvalidParam("expected named params object".into()).into());
    }
    params.parse()
}

/// Accept `{}`, omitted params, `[]`, or transport-normalized null for a noarg
/// call. Reject all nonempty objects/arrays and scalar values.
pub fn parse_empty(params: &Params<'_>) -> Result<EmptyRequest, ErrorObjectOwned> {
    if params.is_object() {
        return params.parse();
    }
    params.parse::<Option<[IgnoredAny; 0]>>()?;
    Ok(EmptyRequest {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::requests::BlockQuery;

    #[test]
    fn noarg_normalizations_are_narrow() {
        for raw in [None, Some("{}"), Some("[]"), Some("[ ]"), Some("null")] {
            assert!(parse_empty(&Params::new(raw)).is_ok(), "{raw:?}");
        }
        for raw in ["{\"extra\":1}", "[1]", "[{}]", "false", "1", "\"\""] {
            assert_eq!(
                parse_empty(&Params::new(Some(raw))).unwrap_err().code(),
                -32602
            );
        }
    }

    #[test]
    fn parameterized_calls_require_objects() {
        for raw in [
            None,
            Some("null"),
            Some("[]"),
            Some("[\"812\"]"),
            Some("[{\"blockHeight\":\"812\"}]"),
            Some("{\"request\":{\"blockHeight\":\"812\"}}"),
        ] {
            assert_eq!(
                parse_named::<BlockQuery>(&Params::new(raw))
                    .unwrap_err()
                    .code(),
                -32602
            );
        }
        assert_eq!(
            *parse_named::<BlockQuery>(&Params::new(Some("{\"blockHeight\":\"812\"}")))
                .unwrap()
                .block_height,
            812
        );
    }
}
