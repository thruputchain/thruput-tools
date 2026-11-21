//! Native request field validation.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

use crate::{BlockHeight, decimal::Decimal, hex::HexBytes};

pub(super) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

pub(crate) fn read_hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<HexBytes, D::Error> {
    struct CanonicalHex;

    impl serde::de::Visitor<'_> for CanonicalHex {
        type Value = HexBytes;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string")
        }

        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<HexBytes, E> {
            let invalid = || E::custom("expected lowercase 0x-prefixed complete bytes");
            let digits = text.strip_prefix("0x").ok_or_else(invalid)?;
            let (pairs, remainder) = digits.as_bytes().as_chunks::<2>();
            if !remainder.is_empty() {
                return Err(invalid());
            }
            let mut bytes = Vec::with_capacity(pairs.len());
            for &[hi, lo] in pairs {
                let hi = hex_digit(hi).ok_or_else(invalid)?;
                let lo = hex_digit(lo).ok_or_else(invalid)?;
                bytes.push((hi << 4) | lo);
            }
            Ok(HexBytes(bytes))
        }
    }

    deserializer.deserialize_str(CanonicalHex)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub(super) fn read_entrypoint<'de, D: Deserializer<'de>>(d: D) -> Result<HexBytes, D::Error> {
    let bytes = read_hex(d)?;
    if bytes.0.len() != 8 {
        return Err(serde::de::Error::custom(
            "entrypoint must contain exactly 8 bytes",
        ));
    }
    Ok(bytes)
}

pub(super) fn read_cursor<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HexBytes>, D::Error> {
    let bytes = read_hex(d)?;
    if bytes.0.is_empty() {
        return Err(serde::de::Error::custom("cursor must be nonempty"));
    }
    Ok(Some(bytes))
}

pub(super) fn read_limit<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let limit = u32::deserialize(d)?;
    if limit == 0 {
        return Err(serde::de::Error::custom("limit must be positive"));
    }
    Ok(Some(limit))
}

pub(super) fn read_gas<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let gas = Decimal::<u64>::deserialize(d)?.0;
    if gas == 0 {
        return Err(serde::de::Error::custom("gasLimit must be positive"));
    }
    Ok(Some(gas))
}

pub(super) fn read_optional_height<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<BlockHeight>, D::Error> {
    super::read_height(d).map(Some)
}

pub(crate) fn hex_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "string", "pattern": "^0x(?:[0-9a-f]{2})*(?![\\s\\S])"})
}

pub(super) fn entrypoint_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = hex_schema(g);
    schema.insert("minLength".into(), 18.into());
    schema.insert("maxLength".into(), 18.into());
    schema
}

pub(super) fn cursor_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = hex_schema(g);
    schema.insert("minLength".into(), 4.into());
    schema
}

pub(crate) fn u32_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = u32::json_schema(g);
    schema.insert("maximum".into(), u32::MAX.into());
    schema
}

pub(super) fn limit_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = u32_schema(g);
    schema.insert("minimum".into(), 1.into());
    schema
}

pub(super) fn u64_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = Decimal::<u64>::json_schema(g);
    schema.insert("pattern".into(), r"^(0|[1-9][0-9]*)(?![\s\S])".into());
    schema
}

pub(super) fn gas_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = u64_schema(g);
    schema.insert("pattern".into(), r"^[1-9][0-9]*(?![\s\S])".into());
    schema.insert("x-integer-minimum".into(), "1".into());
    schema
}

/// Forward only a map to serde's derived struct reader, never its sequence path.
pub(crate) fn map_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Object<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Object<T> {
        type Value = T;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a named object")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(Object(std::marker::PhantomData))
}
