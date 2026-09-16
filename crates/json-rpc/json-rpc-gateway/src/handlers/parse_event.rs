//! Parse hogd's `TxEmittedEvent` into the gateway's `EmittedEvent`.
//!
//! hogd stores events as `TxEmittedEvent { contract_address, num_topics,
//! serialized_event }` where `serialized_event` is:
//! `topic0 (32B) || topic1 (32B) || ... || data (remaining bytes)`.
//!
//! `num_topics` (0–3) determines the split point. Max total 1024 bytes.

use groundhog_json_rpc_types::events::EmittedEvent;
use groundhog_json_rpc_types::hex::{Hex0x, HexBytes, Topic};

use groundhog_xdr::event::TxEmittedEvent;

const TOPIC_SIZE: usize = 32;
const MAX_TOPICS: u32 = 3;

/// Parse a `TxEmittedEvent` into an `EmittedEvent` with split topics + data.
///
/// Returns `None` if the event is malformed (`num_topics` > 3, or
/// `serialized_event` too short for the declared number of topics).
pub fn parse_emitted_event(raw: TxEmittedEvent) -> Option<EmittedEvent> {
    let num_topics = raw.num_topics;
    if num_topics > MAX_TOPICS {
        tracing::warn!(num_topics, "event has too many topics, skipping");
        return None;
    }

    let mut bytes = raw.serialized_event.0;
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

    // Reuse the owned XDR buffer. Removing at most three topics shifts only
    // the data bytes; no second payload allocation is needed.
    bytes.drain(..topics_len);

    Some(EmittedEvent {
        contract_address: Hex0x(raw.contract_address),
        topics,
        data: HexBytes(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xdr_common::BytesM;

    #[test]
    fn event_payload_reuses_the_wire_buffer_for_every_topic_count() {
        for num_topics in 0..=MAX_TOPICS {
            let mut bytes = Vec::new();
            for topic in 0..num_topics {
                bytes.extend_from_slice(&[u8::try_from(topic).unwrap(); TOPIC_SIZE]);
            }
            let data: Vec<u8> = (0..=255).collect();
            bytes.extend_from_slice(&data);
            let allocation = bytes.as_ptr();
            let event = parse_emitted_event(TxEmittedEvent {
                contract_address: [7; 32].into(),
                num_topics,
                serialized_event: BytesM(bytes),
            })
            .unwrap();
            assert_eq!(event.contract_address.0, [7; 32].into());
            assert_eq!(event.data.0, data);
            assert_eq!(event.data.0.as_ptr(), allocation);
            assert_eq!(event.topics.len(), num_topics as usize);
            for (i, topic) in event.topics.iter().enumerate() {
                assert_eq!(topic.0, [u8::try_from(i).unwrap(); TOPIC_SIZE]);
            }
        }
    }

    #[test]
    fn malformed_events_are_rejected_and_empty_data_is_preserved() {
        for (num_topics, len) in [(4, 128), (3, 95), (1, 0)] {
            assert!(
                parse_emitted_event(TxEmittedEvent {
                    contract_address: [0; 32].into(),
                    num_topics,
                    serialized_event: BytesM(vec![0; len]),
                })
                .is_none()
            );
        }
        for num_topics in 0..=MAX_TOPICS {
            let event = parse_emitted_event(TxEmittedEvent {
                contract_address: [0; 32].into(),
                num_topics,
                serialized_event: BytesM(vec![0; num_topics as usize * TOPIC_SIZE]),
            })
            .unwrap();
            assert!(event.data.0.is_empty());
            assert_eq!(event.topics.len(), num_topics as usize);
        }
    }
}
