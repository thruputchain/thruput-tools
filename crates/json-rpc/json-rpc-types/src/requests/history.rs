//! History query shapes. Cursor meaning and configured limits belong to the server.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};

use super::{
    fields::{
        cursor_schema, hex_schema, limit_schema, map_object, present, read_cursor, read_hex,
        read_limit,
    },
    height_schema, read_height,
};
use crate::{
    BlockHeight, ContractAddress, events,
    hex::{HexBytes, Topic},
};

object_enum! {
/// An ascending walk starts at retained beginning or an inclusive height.
AscendingAnchor, "kind" {
    AtBeginning {},
    AtHeight {
        #[serde(
            serialize_with = "crate::decimal::serialize",
            deserialize_with = "read_height"
        )]
        #[schemars(schema_with = "height_schema")]
        block_height: BlockHeight,
    },
}
}

object_enum! {
/// A descending walk starts at retained end or an inclusive height.
DescendingAnchor, "kind" {
    AtEnd {},
    AtHeight {
        #[serde(
            serialize_with = "crate::decimal::serialize",
            deserialize_with = "read_height"
        )]
        #[schemars(schema_with = "height_schema")]
        block_height: BlockHeight,
    },
}
}

object_enum! {
/// Initial range-history position. Direction determines the valid anchors.
HistoryStart, "order" {
    Ascending { anchor: AscendingAnchor },
    Descending { anchor: DescendingAnchor },
}
}

object_request! {
/// Initial exact-block position has a direction, with no range anchor.
BlockStart {
    order: events::IterationOrder,
}
}

object_request! {
/// All topic positions must match; any listed value can match within a position.
/// Omission and empty lists are equivalent wildcards.
#[derive(Default)]
TopicFilter {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "read_topics"
    )]
    #[schemars(schema_with = "topics_schema")]
    topic0: Option<Vec<Topic>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "read_topics"
    )]
    #[schemars(schema_with = "topics_schema")]
    topic1: Option<Vec<Topic>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "read_topics"
    )]
    #[schemars(schema_with = "topics_schema")]
    topic2: Option<Vec<Topic>>,
}
}

object_request! {
/// Address membership and topic predicates for one event filter.
EventFilter {
    #[serde(deserialize_with = "read_addresses")]
    #[schemars(schema_with = "addresses_schema")]
    addresses: Vec<ContractAddress>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "TopicFilter")]
    topics: Option<TopicFilter>,
}
}

fn read_topics<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<Topic>>, D::Error> {
    #[derive(Deserialize)]
    struct TopicBytes(#[serde(deserialize_with = "read_hex")] HexBytes);
    let topics = Vec::<TopicBytes>::deserialize(d)?;
    if topics.len() > events::MAX_TOPIC_VALUES_PER_POSITION as usize
        || topics.iter().any(|topic| topic.0.0.len() != 32)
    {
        return Err(serde::de::Error::custom(
            "expected bounded list of 32-byte topics",
        ));
    }
    topics
        .into_iter()
        .map(|topic| {
            topic
                .0
                .0
                .try_into()
                .map(Topic)
                .map_err(|_| serde::de::Error::custom("topic must contain 32 bytes"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn read_addresses<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ContractAddress>, D::Error> {
    let addresses = Vec::<ContractAddress>::deserialize(d)?;
    if addresses.is_empty()
        || addresses.len() > events::MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER as usize
    {
        return Err(serde::de::Error::custom(
            "address list outside event-filter bounds",
        ));
    }
    Ok(addresses)
}

fn read_filters<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<EventFilter>, D::Error> {
    let filters = Vec::<EventFilter>::deserialize(d)?;
    if filters.is_empty() || filters.len() > events::MAX_EVENT_FILTERS_PER_QUERY as usize {
        return Err(serde::de::Error::custom(
            "filter list outside event-query bounds",
        ));
    }
    Ok(filters)
}

fn topics_schema(g: &mut SchemaGenerator) -> Schema {
    let mut item = hex_schema(g);
    item.insert("minLength".into(), 66.into());
    item.insert("maxLength".into(), 66.into());
    json_schema!({"type": "array", "items": item, "maxItems": events::MAX_TOPIC_VALUES_PER_POSITION})
}

fn addresses_schema(g: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "array", "items": g.subschema_for::<ContractAddress>(), "minItems": 1,
        "maxItems": events::MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER})
}

fn filters_schema(g: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "array", "items": g.subschema_for::<EventFilter>(), "minItems": 1,
        "maxItems": events::MAX_EVENT_FILTERS_PER_QUERY})
}

fn page_schema(schema: &mut Schema) {
    schema.insert(
        "oneOf".into(),
        serde_json::json!([
            {"required": ["start"], "not": {"required": ["cursor"]}},
            {"required": ["cursor"], "not": {"required": ["start"]}}
        ]),
    );
}

// The same field declarations generate the public schema and strict deserialization
// mirror; only the cross-field start/cursor rule needs an extra validation step.
macro_rules! page_request {
    ($(#[$doc:meta])* $name:ident, $start:ty, $(#[$attr:meta])* $field:ident: $ty:ty) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Serialize, JsonSchema)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        #[schemars(transform = page_schema)]
        pub struct $name {
            $(#[$attr])*
            pub $field: $ty,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(transform = strip_null)]
            pub start: Option<$start>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(schema_with = "cursor_schema")]
            pub cursor: Option<HexBytes>,
            /// Omission selects the endpoint default; configured caps apply later.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(schema_with = "limit_schema")]
            pub limit: Option<u32>,
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize, JsonSchema)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Fields {
                    $(#[$attr])*
                    $field: $ty,
                    #[serde(default, deserialize_with = "present")]
                    start: Option<$start>,
                    #[serde(default, deserialize_with = "read_cursor")]
                    cursor: Option<HexBytes>,
                    #[serde(default, deserialize_with = "read_limit")]
                    limit: Option<u32>,
                }
                let fields: Fields = map_object(d)?;
                if fields.start.is_some() == fields.cursor.is_some() {
                    return Err(serde::de::Error::custom("require exactly one of start or cursor"));
                }
                Ok(Self { $field: fields.$field, start: fields.start, cursor: fields.cursor, limit: fields.limit })
            }
        }
    };
}

page_request!(
    /// Match events against one or more predicates and walk a fixed historical range.
    EventQuery, HistoryStart,
    #[serde(deserialize_with = "read_filters")]
    #[schemars(schema_with = "filters_schema")]
    filters: Vec<EventFilter>
);
page_request!(
    /// Transactions whose root invocation targets this address, including wallet accounts.
    RootAddressQuery, HistoryStart,
    address: ContractAddress
);
page_request!(
    /// Transaction references for exactly one committed block.
    BlockPageRequest, BlockStart,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight
);

fn strip_null(schema: &mut Schema) {
    // Option supplies optionality; explicit null is not a native request value.
    if let Some(serde_json::Value::Array(variants)) = schema.remove("anyOf")
        && let Some(non_null) = variants
            .into_iter()
            .find(|v| v.get("type").and_then(serde_json::Value::as_str) != Some("null"))
    {
        *schema =
            Schema::try_from(non_null).expect("bug: generated start schema must be an object");
    }
}
