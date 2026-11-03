//! Request objects for the native RPC surface.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{BlockHeight, ContractAddress, decimal::Decimal, hex::HexBytes, transaction::TxHash};

use fields::{hex_schema, read_hex};

pub(crate) fn read_height<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BlockHeight, D::Error> {
    let height = Decimal::<BlockHeight>::deserialize(deserializer)?.0;
    if *height < 0 {
        return Err(serde::de::Error::custom("blockHeight must be nonnegative"));
    }
    Ok(height)
}

pub(crate) fn height_schema(generator: &mut SchemaGenerator) -> Schema {
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

// serde's ordinary struct visitor also accepts positional sequences. These DTOs
// require maps even when nested inside a list or optional field.
macro_rules! object_request {
    ($(#[$meta:meta])* $name:ident { $($(#[$attr:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Serialize, JsonSchema)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name {
            $($(#[$attr])* pub $field: $ty),*
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(serde::Deserialize, JsonSchema)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Fields { $($(#[$attr])* $field: $ty),* }
                let fields: Fields = $crate::requests::fields::map_object(d)?;
                Ok(Self { $($field: fields.$field),* })
            }
        }
    };
}

// Internally tagged enums also have serde sequence paths; preserve named objects
// while deriving their variant fields and schemas from one declaration.
macro_rules! object_enum {
    ($(#[$meta:meta])* $name:ident, $tag:literal { $($variant:ident { $($(#[$attr:meta])* $field:ident: $ty:ty),* $(,)? }),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Serialize, JsonSchema)]
        #[non_exhaustive]
        #[allow(clippy::enum_variant_names)]
        #[serde(tag = $tag, rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
        pub enum $name { $($variant { $($(#[$attr])* $field: $ty),* }),* }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(serde::Deserialize, JsonSchema)]
                #[allow(clippy::enum_variant_names)]
                #[serde(tag = $tag, rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
                enum Fields { $($variant { $($(#[$attr])* $field: $ty),* }),* }
                let fields: Fields = $crate::requests::fields::map_object(d)?;
                Ok(match fields { $(Fields::$variant { $($field),* } => Self::$variant { $($field),* }),* })
            }
        }
    };
}

/// Canonical no-argument request. Transport normalization is owned by the server.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyRequest {}

/// Committed-state resolution only; transaction-local deployments are excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InvocationContext {
    Root,
    Nested,
}

object_request! {
/// Select the contract for an invocation context, without executing or authorizing it.
EffectiveContractQuery {
    address: ContractAddress,
    context: InvocationContext,
}
}

object_request! {
/// Select retained canonical contract bytes by their SHA-256 identity.
ContractArtifactQuery {
    hash: crate::SHA256Hash,
}
}

object_request! {
/// Select committed block metadata by a nonnegative signed-64 height.
BlockQuery {
    /// Canonical decimal string, from zero through `i64::MAX`.
    #[serde(
        serialize_with = "crate::decimal::serialize",
        deserialize_with = "read_height"
    )]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
}
}

pub(crate) mod fields;
pub(crate) mod funding;
mod history;
mod simulation;
pub use funding::{AirdropRequest, FundAccountRequest};
pub use history::{
    AddressHistoryQuery, AscendingAnchor, BlockPageRequest, BlockStart, DescendingAnchor,
    EventFilter, EventQuery, HistoryStart, TopicFilter,
};
pub use simulation::{
    AssumedAuth, Invocation, SimulateInvocationRequest, SimulateInvocationsRequest,
};

#[cfg(test)]
mod tests;

object_request! {
/// Address shared by balance and registered-authorization reads.
AddressQuery {
    address: ContractAddress,
}
}

object_request! {
/// Read one serialized storage key at an address.
StorageQuery {
    address: ContractAddress,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    key: HexBytes,
}
}

object_request! {
/// Transaction identity shared by status and receipt reads.
TransactionQuery {
    transaction_hash: TxHash,
}
}

object_request! {
/// Signed XDR envelope. Binary validity and admission are server concerns.
SendTransactionRequest {
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    signed_transaction: HexBytes,
}
}
