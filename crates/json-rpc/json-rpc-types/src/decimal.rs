//! Lossless JSON representation of wide integers.
//!
//! Wide integers are always canonical decimal strings, including small values.
//! JSON numbers are deliberately rejected: a JavaScript caller may already have
//! rounded them before they reach this boundary. Binary protocol codecs are unchanged.

use std::{borrow::Cow, fmt};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An integer encoded as a canonical decimal string at a JSON boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Decimal<T>(pub T);

impl<T> From<T> for Decimal<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

/// Integer types supported by the decimal JSON adapter.
pub trait DecimalInteger: Sized {
    /// Name used by generated schemas.
    const NAME: &'static str;
    /// Canonical decimal grammar; bounds are checked during deserialization.
    const PATTERN: &'static str;
    /// Smallest accepted value, expressed exactly.
    const MIN: &'static str;
    /// Largest accepted value, expressed exactly.
    const MAX: &'static str;
    /// Format the integer without a prefix or leading zeros.
    fn decimal(&self) -> String;
    /// Parse a canonical, in-range decimal integer.
    fn parse_decimal(value: &str) -> Option<Self>;
}

macro_rules! primitive {
    ($ty:ty, $name:literal, $min:literal, $max:literal, $pattern:literal) => {
        impl DecimalInteger for $ty {
            const NAME: &'static str = $name;
            const MIN: &'static str = $min;
            const MAX: &'static str = $max;
            const PATTERN: &'static str = $pattern;

            fn decimal(&self) -> String {
                self.to_string()
            }

            fn parse_decimal(value: &str) -> Option<Self> {
                let parsed: Self = value.parse().ok()?;
                (parsed.to_string() == value).then_some(parsed)
            }
        }
    };
}

primitive!(
    u64,
    "DecimalU64",
    "0",
    "18446744073709551615",
    r"^(0|[1-9][0-9]*)(?![\s\S])"
);
#[cfg(target_pointer_width = "64")]
primitive!(
    usize,
    "DecimalSize",
    "0",
    "18446744073709551615",
    r"^(0|[1-9][0-9]*)(?![\s\S])"
);
#[cfg(target_pointer_width = "32")]
primitive!(
    usize,
    "DecimalSize",
    "0",
    "4294967295",
    r"^(0|[1-9][0-9]*)(?![\s\S])"
);
primitive!(
    i64,
    "DecimalI64",
    "-9223372036854775808",
    "9223372036854775807",
    r"^(0|-?[1-9][0-9]*)(?![\s\S])"
);
primitive!(
    u128,
    "DecimalU128",
    "0",
    "340282366920938463463374607431768211455",
    r"^(0|[1-9][0-9]*)(?![\s\S])"
);
primitive!(
    i128,
    "DecimalI128",
    "-170141183460469231731687303715884105728",
    "170141183460469231731687303715884105727",
    r"^(0|-?[1-9][0-9]*)(?![\s\S])"
);

macro_rules! protocol_integer {
    ($ty:ty, $inner:ty, $name:literal) => {
        impl DecimalInteger for $ty {
            const NAME: &'static str = $name;
            const PATTERN: &'static str = <$inner as DecimalInteger>::PATTERN;
            const MIN: &'static str = <$inner as DecimalInteger>::MIN;
            const MAX: &'static str = <$inner as DecimalInteger>::MAX;

            fn decimal(&self) -> String {
                (**self).to_string()
            }

            fn parse_decimal(value: &str) -> Option<Self> {
                <$inner as DecimalInteger>::parse_decimal(value).map(Self::from)
            }
        }
    };
}

protocol_integer!(thruput_xdr::types::BlockHeight, i64, "DecimalBlockHeight");
protocol_integer!(thruput_xdr::types::TimePoint, u64, "DecimalTimePoint");

impl<T: DecimalInteger> fmt::Display for Decimal<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.decimal())
    }
}

impl<T: DecimalInteger> Serialize for Decimal<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize(&self.0, serializer)
    }
}

impl<'de, T: DecimalInteger> Deserialize<'de> for Decimal<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize(deserializer).map(Self)
    }
}

impl<T: DecimalInteger> JsonSchema for Decimal<T> {
    fn schema_name() -> Cow<'static, str> {
        T::NAME.into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": T::PATTERN,
            "maxLength": T::MIN.len().max(T::MAX.len()),
            "description": format!("Canonical decimal integer in the inclusive range {} to {}.", T::MIN, T::MAX),
            "x-integer-minimum": T::MIN,
            "x-integer-maximum": T::MAX
        })
    }
}

/// Serialize a wide integer field as a decimal string.
pub fn serialize<T: DecimalInteger, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.decimal())
}

/// Deserialize a canonical decimal string, rejecting numeric JSON and overflow.
pub fn deserialize<'de, T: DecimalInteger, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    let value = String::deserialize(deserializer)?;
    T::parse_decimal(&value).ok_or_else(|| {
        serde::de::Error::custom(format!(
            "expected canonical decimal integer in range {} to {}",
            T::MIN,
            T::MAX
        ))
    })
}

/// Adapter for optional wide-integer fields. Use `serde(default)` for omitted fields.
pub mod option {
    use super::{Decimal, DecimalInteger};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize a present integer as a string and an absent value as null.
    pub fn serialize<T: DecimalInteger, S: Serializer>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer.serialize_some(&value.decimal()),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialize null or a canonical decimal string.
    pub fn deserialize<'de, T: DecimalInteger, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<Decimal<T>>::deserialize(deserializer).map(|value| value.map(|value| value.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_exact_wide_integer_boundaries() {
        for value in [0, 1, (1 << 53) - 1, 1 << 53, (1 << 53) + 1, u64::MAX] {
            let json = serde_json::to_string(&Decimal(value)).unwrap();
            assert_eq!(json, format!("\"{value}\""));
            assert_eq!(
                serde_json::from_str::<Decimal<u64>>(&json).unwrap().0,
                value
            );
        }
        for value in [i128::MIN, -1, 0, i128::MAX] {
            let json = serde_json::to_string(&Decimal(value)).unwrap();
            assert_eq!(
                serde_json::from_str::<Decimal<i128>>(&json).unwrap().0,
                value
            );
        }
    }

    #[test]
    fn rejects_numbers_noncanonical_strings_and_overflow() {
        for json in [
            "0",
            "9007199254740993",
            "null",
            "true",
            "\"\"",
            "\"00\"",
            "\"+1\"",
            "\"-0\"",
            "\" 1\"",
            "\"1e3\"",
            "\"1.0\"",
            "\"18446744073709551616\"",
        ] {
            assert!(
                serde_json::from_str::<Decimal<u64>>(json).is_err(),
                "{json}"
            );
        }
        assert!(serde_json::from_str::<Decimal<i64>>("\"9223372036854775808\"").is_err());
        assert!(serde_json::from_str::<Decimal<i64>>("\"-9223372036854775809\"").is_err());
    }
}
