//! Request objects for the native RPC surface. Registration is migrated separately.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{BlockHeight, decimal::Decimal};

/// Canonical no-argument request. Transport normalization is owned by the server.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyRequest {}

/// Select committed block metadata by a nonnegative signed-64 height.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockQuery {
    /// Canonical decimal string, from zero through `i64::MAX`.
    #[serde(
        serialize_with = "crate::decimal::serialize",
        deserialize_with = "read_height"
    )]
    #[schemars(schema_with = "height_schema")]
    pub block_height: BlockHeight,
}

fn read_height<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BlockHeight, D::Error> {
    let height = Decimal::<BlockHeight>::deserialize(deserializer)?.0;
    if *height < 0 {
        return Err(serde::de::Error::custom("blockHeight must be nonnegative"));
    }
    Ok(height)
}

fn height_schema(generator: &mut SchemaGenerator) -> Schema {
    let mut schema = Decimal::<BlockHeight>::json_schema(generator);
    schema.insert("pattern".into(), r"^(0|[1-9][0-9]*)(?![\s\S])".into());
    schema.insert("maxLength".into(), 19.into());
    schema.insert("x-integer-minimum".into(), "0".into());
    schema.insert(
        "description".into(),
        "Canonical decimal block height in the inclusive range 0 to 9223372036854775807.".into(),
    );
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn height_requests_preserve_precision_and_reject_invalid_objects() {
        for height in [0, 812, 9_007_199_254_740_993, i64::MAX] {
            let value = json!({"blockHeight": height.to_string()});
            let request: BlockQuery = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(*request.block_height, height);
            assert_eq!(serde_json::to_value(request).unwrap(), value);
        }
        for value in [
            json!({}),
            json!({"blockHeight": null}),
            json!({"blockHeight": 812}),
            json!({"blockHeight": "1", "extra": true}),
            json!({"block_height": "1"}),
        ] {
            assert!(serde_json::from_value::<BlockQuery>(value).is_err());
        }
        for text in [
            "-1",
            "-0",
            "+1",
            "01",
            "1.0",
            "1e3",
            " 1",
            "1\n",
            "9223372036854775808",
        ] {
            assert!(
                serde_json::from_value::<BlockQuery>(json!({"blockHeight": text})).is_err(),
                "{text:?}"
            );
        }
    }
}
