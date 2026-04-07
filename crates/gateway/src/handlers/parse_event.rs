//! Parse hogd's `TxEmittedEvent` into the gateway's `EmittedEvent`.
//!
//! hogd stores events as `TxEmittedEvent { contract_address, num_topics,
//! serialized_event }` where `serialized_event` is:
//! `topic0 (32B) || topic1 (32B) || ... || data (remaining bytes)`.
//!
//! `num_topics` (0–3) determines the split point. Max total 1024 bytes.

use groundhog_rpc_api_types::events::EmittedEvent;
use groundhog_rpc_api_types::hex::{Hex0x, HexBytes, Topic};

use groundhog_xdr::event::TxEmittedEvent;

const TOPIC_SIZE: usize = 32;
const MAX_TOPICS: u32 = 3;

/// Parse a `TxEmittedEvent` into an `EmittedEvent` with split topics + data.
///
/// Returns `None` if the event is malformed (num_topics > 3, or
/// serialized_event too short for the declared number of topics).
pub fn parse_emitted_event(raw: &TxEmittedEvent) -> Option<EmittedEvent> {
    let num_topics = raw.num_topics;
    if num_topics > MAX_TOPICS {
        tracing::warn!(num_topics, "event has too many topics, skipping");
        return None;
    }

    let bytes: &[u8] = raw.serialized_event.as_ref();
    let topics_len = num_topics as usize * TOPIC_SIZE;
    if bytes.len() < topics_len {
        tracing::warn!(
            num_topics,
            event_len = bytes.len(),
            "serialized_event too short for declared topics, skipping"
        );
        return None;
    }

    let mut topics = Vec::with_capacity(num_topics as usize);
    for i in 0..num_topics as usize {
        let start = i * TOPIC_SIZE;
        let mut topic = [0u8; TOPIC_SIZE];
        topic.copy_from_slice(&bytes[start..start + TOPIC_SIZE]);
        topics.push(Topic(topic));
    }

    let data = bytes[topics_len..].to_vec();

    Some(EmittedEvent {
        contract_address: Hex0x(raw.contract_address),
        topics,
        data: HexBytes(data),
    })
}
