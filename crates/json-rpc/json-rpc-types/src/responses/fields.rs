//! Native result domains. Reuse the request codecs where the domains match.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serializer};

pub(super) use crate::requests::fields::{hex_schema, read_hex, u32_schema};
pub(super) use crate::requests::{height_schema, read_height};
use crate::{
    decimal::Decimal,
    hex::{HexBytes, Topic},
};

pub(super) fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// `present` for a 64-bit decimal string: a stated value, never `null`.
pub(super) fn present_decimal_u64<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<u64>, D::Error> {
    Decimal::<u64>::deserialize(d).map(|v| Some(v.0))
}

// A custom reader without `default` makes omission an error, even for Option.
pub(super) fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

pub(super) fn nullable_schema<T: JsonSchema>(g: &mut SchemaGenerator) -> Schema {
    Option::<T>::json_schema(g)
}

pub(super) fn i32_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "integer", "minimum": i32::MIN, "maximum": i32::MAX})
}

pub(super) fn read_amount<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
    let amount = Decimal::<i128>::deserialize(d)?.0;
    if amount < 0 {
        return Err(serde::de::Error::custom("amount must be nonnegative"));
    }
    Ok(amount)
}

pub(super) fn amount_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = Decimal::<i128>::json_schema(g);
    schema.insert("pattern".into(), r"^(0|[1-9][0-9]*)(?![\s\S])".into());
    schema.insert("x-integer-minimum".into(), "0".into());
    schema.insert(
        "description".into(),
        "Canonical nonnegative signed-128 decimal amount.".into(),
    );
    schema
}

pub(super) fn read_decimal_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    u32::try_from(Decimal::<u64>::deserialize(d)?.0).map_err(serde::de::Error::custom)
}

#[allow(clippy::trivially_copy_pass_by_ref)] // Serde serializer signature.
pub(super) fn write_decimal_u32<S: Serializer>(value: &u32, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(value)
}

pub(super) fn decimal_u32_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = Decimal::<u64>::json_schema(g);
    schema.insert("maxLength".into(), 10.into());
    schema.insert("x-integer-maximum".into(), u32::MAX.to_string().into());
    schema.insert(
        "description".into(),
        "Canonical unsigned-32 decimal amount.".into(),
    );
    schema
}

#[derive(Deserialize)]
struct CanonicalHex(#[serde(deserialize_with = "read_hex")] HexBytes);

pub(super) fn read_hex_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<HexBytes>, D::Error> {
    Vec::<CanonicalHex>::deserialize(d).map(|items| items.into_iter().map(|item| item.0).collect())
}

pub(super) fn hex_list_schema(g: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "array", "items": hex_schema(g)})
}

pub(super) fn read_topics<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Topic>, D::Error> {
    let items = read_hex_list(d)?;
    if items.len() > 3 {
        return Err(serde::de::Error::custom("at most three event topics"));
    }
    items
        .into_iter()
        .map(|item| {
            item.0
                .try_into()
                .map(Topic)
                .map_err(|_| serde::de::Error::custom("topic must contain 32 bytes"))
        })
        .collect()
}

pub(super) fn topics_schema(g: &mut SchemaGenerator) -> Schema {
    let mut item = hex_schema(g);
    item.insert("minLength".into(), 66.into());
    item.insert("maxLength".into(), 66.into());
    json_schema!({"type": "array", "maxItems": 3, "items": item})
}

pub(super) fn read_next_cursor<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<HexBytes>, D::Error> {
    let item = Option::<CanonicalHex>::deserialize(d)?.map(|item| item.0);
    if item.as_ref().is_some_and(|item| item.0.is_empty()) {
        return Err(serde::de::Error::custom(
            "nextCursor must be nonempty or null",
        ));
    }
    Ok(item)
}

pub(super) fn next_cursor_schema(g: &mut SchemaGenerator) -> Schema {
    let mut cursor = hex_schema(g);
    cursor.insert("minLength".into(), 4.into());
    json_schema!({"anyOf": [cursor, {"type": "null"}]})
}
