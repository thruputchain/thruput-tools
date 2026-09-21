use groundhog_xdr::event::TxEmittedEvent;
use xdr_common::VecM;

use xdr_common::BytesM;

use groundhog_hogd_backend::event::*;

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
