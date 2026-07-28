use std::path::Path;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::ContractAddress;
use groundhog_json_rpc_types::events::{
    BlockAnchor, ContractAddressList, EventFilter, EventFilterList, EventPage, EventQuery,
    EventTopicFilter, IterationOrder, MAX_TOPIC_VALUES_PER_POSITION, PaginationCursor, TopicList,
};
use groundhog_json_rpc_types::hex::Topic;
use xdr_common::VecM;

use crate::account_resolution::resolve_address_target;
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::event_spec::ValidatedEventSpec;
use crate::events_json::{
    EVENTS_LIST_JSON_SCHEMA_VERSION, EventDecodeStatus, EventFiltersReport, EventListItemReport,
    EventListReport,
};
use crate::result::CommandResult;
use crate::tx::map_rpc_error;
use crate::tx_utils::{format_tx_hash, human_safe};
use crate::{CliError, Print};

const DEFAULT_EVENT_LIMIT: u32 = 50;
const MAX_SERIALIZED_EVENT_BYTES: usize = 1024;

/// List one page of raw events emitted by a resolved contract address.
#[expect(clippy::too_many_arguments)]
pub fn list(
    contract_input: &str,
    topic0_inputs: &[String],
    topic1_inputs: &[String],
    topic2_inputs: &[String],
    limit_input: Option<&str>,
    cursor_input: Option<&str>,
    spec_path: Option<&Path>,
    selected_event: Option<&str>,
    project_start: &Path,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let mut topic0 = parse_topic_values(topic0_inputs, "topic0")?;
    let topic1 = parse_topic_values(topic1_inputs, "topic1")?;
    let topic2 = parse_topic_values(topic2_inputs, "topic2")?;
    let limit = parse_event_limit(limit_input)?;
    let cursor = parse_event_cursor(cursor_input)?;
    let continuation_requested = cursor_input.is_some();
    let event_spec = match spec_path {
        Some(path) => Some(ValidatedEventSpec::load(path)?),
        None if selected_event.is_some() => {
            return Err(CliError::InvalidEventOptions {
                message: "`--event` requires `--spec <PATH>`".into(),
            });
        }
        None => None,
    };
    if let Some(name) = selected_event {
        let signature = event_spec
            .as_ref()
            .expect("event selection must have a loaded spec")
            .selected_signature(name)?;
        if topic0.is_empty() {
            topic0.push(signature);
        } else if topic0.iter().any(|topic| topic != &signature) {
            return Err(CliError::InvalidEventOptions {
                message: format!(
                    "`--topic0` conflicts with selected event `{}`",
                    human_safe(name)
                ),
            });
        }
    }

    let profile = resolve_profile_with_overrides(project_start, profile_name, overrides)?;
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let address = resolve_address_target(&profile, contract_input)?;
    let contract_address = ContractAddress::from(address);
    let filters = EventFiltersReport {
        topic0: topic0.clone(),
        topic1: topic1.clone(),
        topic2: topic2.clone(),
    };
    let query = EventQuery {
        filters: EventFilterList(VecM(vec![EventFilter {
            contract_addresses: ContractAddressList(VecM(vec![contract_address])),
            topics: EventTopicFilter {
                topic0: TopicList(VecM(topic0)),
                topic1: TopicList(VecM(topic1)),
                topic2: TopicList(VecM(topic2)),
            },
        }])),
        order: IterationOrder::Descending,
        anchor: BlockAnchor::AtEnd,
        cursor,
        limit,
    };

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let page = rt.block_on(fetch_event_page(&rpc_url, query))?;
    let spec_report = event_spec.as_ref().map(|spec| spec.report(selected_event));
    let events = match &event_spec {
        Some(spec) => spec.decode_events(page.events),
        None => page
            .events
            .into_iter()
            .map(EventListItemReport::from)
            .collect(),
    };
    let report = EventListReport {
        schema_version: EVENTS_LIST_JSON_SCHEMA_VERSION,
        profile: profile.name,
        rpc_url,
        contract_input: contract_input.to_owned(),
        contract_address,
        filters,
        spec: spec_report,
        events,
        cursor: page.cursor,
        latest_block: page.latest_block,
        oldest_block: page.oldest_block,
    };

    if !print.is_json() {
        print_event_list(&report, continuation_requested, print);
    }
    print.json(&CommandResult::new(cmd, report))
}

fn parse_topic_values(inputs: &[String], position: &'static str) -> Result<Vec<Topic>, CliError> {
    if inputs.len() > MAX_TOPIC_VALUES_PER_POSITION as usize {
        return Err(CliError::InvalidEventFilter {
            position,
            count: inputs.len(),
            max: MAX_TOPIC_VALUES_PER_POSITION,
        });
    }
    inputs
        .iter()
        .map(|input| {
            serde_json::from_value(serde_json::Value::String(input.clone())).map_err(|error| {
                CliError::InvalidEventTopic {
                    position,
                    input: input.clone(),
                    display_input: human_safe(input),
                    message: error.to_string(),
                }
            })
        })
        .collect()
}

pub(crate) fn parse_event_limit(input: Option<&str>) -> Result<u32, CliError> {
    let Some(input) = input else {
        return Ok(DEFAULT_EVENT_LIMIT);
    };
    let limit = input
        .parse::<u32>()
        .map_err(|error| CliError::InvalidEventLimit {
            input: input.to_owned(),
            display_input: human_safe(input),
            message: error.to_string(),
        })?;
    if limit == 0 {
        return Err(CliError::InvalidEventLimit {
            input: input.to_owned(),
            display_input: human_safe(input),
            message: "must be greater than zero".into(),
        });
    }
    Ok(limit)
}

pub(crate) fn parse_event_cursor(input: Option<&str>) -> Result<PaginationCursor, CliError> {
    let Some(input) = input else {
        return Ok(PaginationCursor::default());
    };
    serde_json::from_value(serde_json::Value::String(input.to_owned())).map_err(|error| {
        CliError::InvalidEventCursor {
            input: input.to_owned(),
            display_input: human_safe(input),
            message: error.to_string(),
        }
    })
}

pub(crate) async fn fetch_event_page(
    rpc_url: &str,
    query: EventQuery,
) -> Result<EventPage, CliError> {
    let requested_limit = query.limit;
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|error| CliError::RpcConnect(human_safe(&format!("{rpc_url}: {error}"))))?;
    let page = client
        .get_events(query)
        .await
        .map_err(|error| map_rpc_error("getEvents", &error))?;
    validate_event_page(&page, requested_limit)?;
    Ok(page)
}

fn validate_event_page(page: &EventPage, requested_limit: u32) -> Result<(), CliError> {
    if u32::try_from(page.events.len()).map_or(true, |len| len > requested_limit) {
        return Err(invalid_response(format!(
            "returned {} events for requested limit {requested_limit}",
            page.events.len(),
        )));
    }
    for event in &page.events {
        let topic_bytes = event
            .event
            .topics
            .len()
            .checked_mul(32)
            .ok_or_else(|| invalid_response("event topic byte length overflow"))?;
        let serialized_bytes = topic_bytes
            .checked_add(event.event.data.0.len())
            .ok_or_else(|| invalid_response("serialized event length overflow"))?;
        if event.event.topics.len() > 3 {
            return Err(invalid_response(format!(
                "event at height {} sequence {} returned {} topics; maximum is 3",
                *event.block_height,
                event.sequence_number,
                event.event.topics.len(),
            )));
        }
        if serialized_bytes > MAX_SERIALIZED_EVENT_BYTES {
            return Err(invalid_response(format!(
                "event at height {} sequence {} is {serialized_bytes} bytes; maximum is {MAX_SERIALIZED_EVENT_BYTES}",
                *event.block_height, event.sequence_number,
            )));
        }
    }
    Ok(())
}

fn invalid_response(message: impl Into<String>) -> CliError {
    CliError::RpcMethodFailed {
        method: "getEvents",
        message: format!("invalid response: {}", message.into()),
    }
}

fn print_event_list(report: &EventListReport, continuation_requested: bool, print: &Print) {
    if print.is_quiet() {
        return;
    }
    let address = format_contract_address(&report.contract_address.0);
    if !report.contract_input.eq_ignore_ascii_case(&address) {
        print.detail("contract", human_safe(&report.contract_input));
    }
    print.detail("emitter", &address);
    if let Some(spec) = &report.spec {
        print.detail("spec (user-supplied)", human_safe(&spec.path));
        if let Some(selected_event) = &spec.selected_event {
            print.detail("selected event", human_safe(selected_event));
        }
    }
    print.detail("latest indexed block", *report.latest_block);
    print.detail("oldest retained block", *report.oldest_block);

    if report.events.is_empty() {
        let message = if !report.cursor.0.is_empty() {
            "no displayable events in this page; continue with cursor"
        } else if continuation_requested {
            "no events returned; no continuation cursor"
        } else {
            "no events found; stream exhausted"
        };
        print.detail("events", message);
    } else {
        for item in &report.events {
            print.status("");
            print.status("event");
            print.detail("height", *item.raw.block_height);
            print.detail("sequence", item.raw.sequence_number);
            print.detail("txn", format_tx_hash(&item.raw.tx_hash));
            if item.raw.contract_address != report.contract_address {
                print.detail(
                    "event emitter",
                    format_contract_address(&item.raw.contract_address.0),
                );
            }
            let decoded = item.decoded.as_ref();
            if let Some(decoded) = decoded {
                print.detail("decoded event", human_safe(&decoded.name));
                if let Some(signature) = item.raw.topics.first() {
                    print.detail("signature", format_args!("0x{}", hex::encode(signature.0)));
                }
                for (name, topic) in &decoded.topics {
                    print.detail(
                        &human_safe(name),
                        format_args!("0x{}", hex::encode(topic.0)),
                    );
                }
            } else {
                for (topic_index, topic) in item.raw.topics.iter().enumerate() {
                    print.detail(
                        &format!("topic {topic_index}"),
                        format_args!("0x{}", hex::encode(topic.0)),
                    );
                }
            }
            print.detail("data", format_args!("0x{}", hex::encode(&item.raw.data.0)));
            match item.decode_status {
                EventDecodeStatus::NotRequested => {}
                EventDecodeStatus::UnknownSignature => {
                    print.detail("decode", "signature not declared by supplied spec");
                }
                EventDecodeStatus::Malformed => {
                    print.detail(
                        "decode",
                        item.decode_error
                            .as_deref()
                            .unwrap_or("malformed known event"),
                    );
                }
                EventDecodeStatus::Decoded => {
                    let decoded = decoded.expect("decoded status must include decoded event data");
                    for (name, value) in &decoded.fields {
                        print.detail(&human_safe(name), human_safe(&value.to_string()));
                    }
                }
            }
        }
    }
    if !report.cursor.0.is_empty() {
        print.detail(
            "next cursor",
            format_args!("0x{}", hex::encode(&report.cursor.0)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_event_cursor, parse_event_limit, parse_topic_values};

    #[test]
    fn event_limit_defaults_and_accepts_nonzero_u32() {
        assert_eq!(parse_event_limit(None).unwrap(), 50);
        assert_eq!(parse_event_limit(Some("1")).unwrap(), 1);
        assert_eq!(
            parse_event_limit(Some(&u32::MAX.to_string())).unwrap(),
            u32::MAX
        );
    }

    #[test]
    fn event_limit_rejects_invalid_values() {
        for input in ["0", "-1", "one", "4294967296"] {
            let error = parse_event_limit(Some(input)).expect_err("invalid limit should fail");
            assert_eq!(error.code(), "invalid_event_limit");
        }
    }

    #[test]
    fn event_cursor_uses_shared_hex_contract() {
        for (input, expected) in [
            (None, vec![]),
            (Some("0x"), vec![]),
            (Some("0XAbCd"), vec![0xab, 0xcd]),
        ] {
            let cursor = parse_event_cursor(input).expect("shared cursor form should parse");
            assert_eq!(cursor.0, expected);
        }
    }

    #[test]
    fn event_cursor_rejects_malformed_hex() {
        for input in ["0xabc", "0xgg"] {
            let error = parse_event_cursor(Some(input)).expect_err("invalid cursor should fail");
            assert_eq!(error.code(), "invalid_event_cursor");
        }
    }

    #[test]
    fn event_topics_accept_shared_fixed_hex_forms() {
        for input in [
            format!("0x{}", "ab".repeat(32)),
            format!("0X{}", "AB".repeat(32)),
            "aB".repeat(32),
        ] {
            let topics = parse_topic_values(&[input], "topic0").expect("topic should parse");
            assert_eq!(topics[0].0, [0xab; 32]);
        }
    }

    #[test]
    fn event_topics_reject_malformed_values() {
        for input in [
            "0xdeadbeef".to_owned(),
            format!("0x{}", "ab".repeat(33)),
            "not-hex".to_owned(),
        ] {
            let error = parse_topic_values(&[input], "topic0").expect_err("topic should fail");
            assert_eq!(error.code(), "invalid_event_topic");
        }
    }

    #[test]
    fn event_topics_reject_more_than_position_limit() {
        let inputs = vec![format!("0x{}", "ab".repeat(32)); 17];
        let error = parse_topic_values(&inputs, "topic1").expect_err("topic list should fail");
        assert_eq!(error.code(), "invalid_event_filter");
    }
}
