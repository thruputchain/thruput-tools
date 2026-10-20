use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ContractAddress, airdrop::ClientNonce, decimal::Decimal};

use super::fields::{hex_schema, read_hex};

object_request! {
/// One optional faucet attempt, bound to the recipient and caller-owned nonce.
AirdropRequest {
    recipient: ContractAddress,
    #[serde(deserialize_with = "read_nonce")]
    #[schemars(schema_with = "nonce_schema")]
    client_nonce: ClientNonce,
}
}

object_request! {
/// Local funding request, including the expected local faucet identity.
FundAccountRequest {
    recipient: ContractAddress,
    /// Positive canonical decimal amount in raw HOG units.
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_positive_amount")]
    #[schemars(schema_with = "positive_amount_schema")]
    raw_amount: u128,
    #[serde(deserialize_with = "read_nonce")]
    #[schemars(schema_with = "nonce_schema")]
    client_nonce: ClientNonce,
    faucet: ContractAddress,
}
}

fn read_nonce<'de, D: Deserializer<'de>>(d: D) -> Result<ClientNonce, D::Error> {
    ClientNonce::new(read_hex(d)?.0).map_err(serde::de::Error::custom)
}

fn nonce_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = hex_schema(g);
    schema.insert("minLength".into(), 4.into());
    schema.insert("maxLength".into(), 66.into());
    schema
}

pub(crate) fn read_positive_amount<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
    let value = Decimal::<u128>::deserialize(d)?.0;
    if value == 0 {
        return Err(serde::de::Error::custom("rawAmount must be positive"));
    }
    Ok(value)
}

pub(crate) fn positive_amount_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = Decimal::<u128>::json_schema(g);
    schema.insert("pattern".into(), r"^[1-9][0-9]*(?![\s\S])".into());
    schema.insert("x-integer-minimum".into(), "1".into());
    schema.insert(
        "description".into(),
        "Positive canonical decimal raw amount, at most 340282366920938463463374607431768211455."
            .into(),
    );
    schema
}

#[cfg(test)]
mod tests;
