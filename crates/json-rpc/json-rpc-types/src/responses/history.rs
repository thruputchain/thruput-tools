use super::{
    Deserialize, Deserializer, JsonSchema, ResponseData, Serialize,
    fields::{
        height_schema, hex_schema, next_cursor_schema, read_height, read_hex, read_next_cursor,
        read_topics, topics_schema, u32_schema,
    },
};
use crate::{
    BlockHeight, ContractAddress,
    hex::{HexBytes, Topic},
    transaction::TxHash,
};

response_object! {
/// Parsed event with a typed emitter address and raw topic/data bytes.
EmittedEvent {
    address: ContractAddress,
    #[serde(deserialize_with = "read_topics")]
    #[schemars(schema_with = "topics_schema")]
    topics: Vec<Topic>,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    data: HexBytes,
}
}

response_object! {
/// Event identity within one chain instance.
IdentifiedEvent {
    transaction_hash: TxHash,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
    #[schemars(schema_with = "u32_schema")]
    sequence_number: u32,
    event: EmittedEvent,
}
}

response_object! {
/// Event page. Empty nonterminal pages are valid; bounds are not checkpoints.
EventPage {
    events: Vec<IdentifiedEvent>,
    #[serde(deserialize_with = "read_next_cursor")]
    #[schemars(schema_with = "next_cursor_schema")]
    next_cursor: Option<HexBytes>,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    oldest_indexed_block_height: BlockHeight,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    latest_indexed_block_height: BlockHeight,
}
}

response_object! {
/// Committed transaction reference; contains neither its body nor execution.
TransactionRef {
    transaction_hash: TxHash,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
}
}

response_object! {
/// References for root-address history or one exact block. Request context
/// determines ordering, membership and the meaning of the serving bounds.
TransactionRefPage {
    transactions: Vec<TransactionRef>,
    #[serde(deserialize_with = "read_next_cursor")]
    #[schemars(schema_with = "next_cursor_schema")]
    next_cursor: Option<HexBytes>,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    oldest_indexed_block_height: BlockHeight,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    latest_indexed_block_height: BlockHeight,
}
}

/// Exact-block pages share the reference shape, not range-walk checkpoints.
pub type BlockTransactionPage = TransactionRefPage;
