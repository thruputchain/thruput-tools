//! Split hogd's flat event payload into the public topics-and-data shape.
//!
//! hogd stores events as `TxEmittedEvent { contract_address, num_topics,
//! serialized_event }` where `serialized_event` is
//! `topic0 (32B) || topic1 (32B) || ... || data (remaining bytes)`.
//! `num_topics` (0–3) is the split point; the total payload is at most
//! 1024 bytes.

use groundhog_json_rpc_types::{
    events::EmittedEvent,
    hex::{Hex0x, HexBytes, Topic},
};
use groundhog_xdr::event::TxEmittedEvent;
use xdr_common::VecM;

/// Width of one event topic.
pub const TOPIC_SIZE: usize = 32;

/// Most topics an event may declare.
pub const MAX_TOPICS: u32 = 3;

/// Events parsed from one hogd payload, with the malformed ones counted.
///
/// A malformed event is dropped rather than failing the whole response, so
/// one bad record cannot hide a page of good ones. `dropped` lets a caller
/// that meters parse failures report them without re-walking the input.
#[derive(Debug, Default)]
pub struct ParsedEvents {
    /// Events that parsed cleanly, in wire order.
    pub events: Vec<EmittedEvent>,
    /// How many events were dropped as malformed.
    pub dropped: usize,
}

/// Parse a `TxEmittedEvent` into an `EmittedEvent` with split topics and data.
///
/// Returns `None` if the event is malformed — `num_topics` above
/// [`MAX_TOPICS`], or a `serialized_event` too short for the topics it
/// declares — and logs a warning naming the reason.
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

/// Parse every event in a hogd payload, counting the malformed ones.
///
/// The const bound is the payload's own cap — receipts and view results
/// declare different ones — so callers pass their wire vector as it comes.
pub fn parse_emitted_events<const MAX: u32>(raw: VecM<TxEmittedEvent, MAX>) -> ParsedEvents {
    let mut parsed = ParsedEvents {
        events: Vec::with_capacity(raw.0.len()),
        dropped: 0,
    };
    for event in raw.0 {
        match parse_emitted_event(event) {
            Some(event) => parsed.events.push(event),
            None => parsed.dropped += 1,
        }
    }
    parsed
}

#[cfg(test)]
mod tests {
    use xdr_common::BytesM;

    use super::*;

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

    #[test]
    fn malformed_events_are_counted_without_dropping_the_good_ones() {
        let good = TxEmittedEvent {
            contract_address: [1; 32].into(),
            num_topics: 1,
            serialized_event: BytesM(vec![9; TOPIC_SIZE + 4]),
        };
        let malformed = TxEmittedEvent {
            contract_address: [2; 32].into(),
            num_topics: 3,
            serialized_event: BytesM(vec![0; TOPIC_SIZE]),
        };

        let parsed = parse_emitted_events(VecM::<_>(vec![good, malformed]));

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.dropped, 1);
        assert_eq!(parsed.events[0].contract_address.0, [1; 32].into());
        assert_eq!(parsed.events[0].data.0, vec![9; 4]);
    }
}
