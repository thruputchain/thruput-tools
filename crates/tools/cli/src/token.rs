use std::path::Path;

use groundhog_erc20_if::TransferEvent;
use groundhog_json_rpc_types::events::{
    BlockAnchor, ContractAddressList, EventFilter, EventFilterList, EventQuery, EventTopicFilter,
    IterationOrder, PaginationCursor, TopicList,
};
use groundhog_json_rpc_types::hex::Topic;
use groundhog_json_rpc_types::{ContractAddress, events::IdentifiedEvent};
use xdr_common::VecM;

use crate::account_resolution::resolve_address_target;
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::events::fetch_event_page;
use crate::events_json::RawEventReport;
use crate::result::CommandResult;
use crate::terminal::terminal_safe;
use crate::token_json::{
    DecodedTokenTransferReport, TOKEN_TRANSFERS_JSON_SCHEMA_VERSION, TokenParticipantFilter,
    TokenTransferDecodeStatus, TokenTransferKind, TokenTransferRecordReport, TokenTransfersReport,
};
use crate::tx_utils::format_tx_hash;
use crate::{CliError, Print};

const ZERO_ADDRESS: [u8; 32] = [0; 32];

/// List one newest-first page of canonical ERC20 transfer events.
#[expect(clippy::too_many_arguments)]
pub fn transfers(
    token_input: &str,
    from_input: Option<&str>,
    to_input: Option<&str>,
    account_input: Option<&str>,
    limit_input: Option<&str>,
    cursor_input: Option<&str>,
    project_start: &Path,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if account_input.is_some() && (from_input.is_some() || to_input.is_some()) {
        return Err(CliError::InvalidTokenTransferOptions {
            message: "`--account` cannot be combined with `--from` or `--to`".into(),
        });
    }
    let limit = parse_token_transfer_limit(limit_input)?;
    let cursor = parse_token_transfer_cursor(cursor_input)?;
    let continuation_requested = cursor_input.is_some();
    let profile = resolve_profile_with_overrides(project_start, profile_name, overrides)?;
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let token_address = ContractAddress::from(resolve_address_target(&profile, token_input)?);
    let from = from_input
        .map(|input| resolve_address_target(&profile, input).map(ContractAddress::from))
        .transpose()?;
    let to = to_input
        .map(|input| resolve_address_target(&profile, input).map(ContractAddress::from))
        .transpose()?;
    let account = account_input
        .map(|input| resolve_address_target(&profile, input).map(ContractAddress::from))
        .transpose()?;
    let participant_filter =
        participant_report(from_input, from, to_input, to, account_input, account);
    let filters = transfer_filters(token_address, from, to, account);
    let query = EventQuery {
        filters: EventFilterList(VecM(filters)),
        order: IterationOrder::Descending,
        anchor: BlockAnchor::AtEnd,
        cursor,
        limit,
    };

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let page = rt.block_on(fetch_event_page(&rpc_url, query))?;
    let report = TokenTransfersReport {
        schema_version: TOKEN_TRANSFERS_JSON_SCHEMA_VERSION,
        profile: profile.name,
        rpc_url,
        token_input: token_input.to_owned(),
        token_address,
        participant_filter,
        records: page
            .events
            .into_iter()
            .map(decode_transfer_record)
            .collect(),
        cursor: page.cursor,
        latest_block: page.latest_block,
        oldest_block: page.oldest_block,
    };

    if !print.is_json() {
        print_transfers(&report, continuation_requested, print);
    }
    print.json(&CommandResult::new(cmd, report))
}

fn parse_token_transfer_limit(input: Option<&str>) -> Result<u32, CliError> {
    let Some(input) = input else {
        return Ok(50);
    };
    let limit = input
        .parse::<u32>()
        .map_err(|error| CliError::InvalidTokenTransferLimit {
            input: input.to_owned(),
            display_input: terminal_safe(input),
            message: error.to_string(),
        })?;
    if limit == 0 {
        return Err(CliError::InvalidTokenTransferLimit {
            input: input.to_owned(),
            display_input: terminal_safe(input),
            message: "must be greater than zero".into(),
        });
    }
    Ok(limit)
}

fn parse_token_transfer_cursor(input: Option<&str>) -> Result<PaginationCursor, CliError> {
    let Some(input) = input else {
        return Ok(PaginationCursor::default());
    };
    serde_json::from_value(serde_json::Value::String(input.to_owned())).map_err(|error| {
        CliError::InvalidTokenTransferCursor {
            input: input.to_owned(),
            display_input: terminal_safe(input),
            message: error.to_string(),
        }
    })
}

fn participant_report(
    from_input: Option<&str>,
    from: Option<ContractAddress>,
    to_input: Option<&str>,
    to: Option<ContractAddress>,
    account_input: Option<&str>,
    account: Option<ContractAddress>,
) -> Option<TokenParticipantFilter> {
    match (from_input, from, to_input, to, account_input, account) {
        (_, _, _, _, Some(input), Some(address)) => Some(TokenParticipantFilter::Account {
            input: input.to_owned(),
            address,
        }),
        (Some(from_input), Some(from), Some(to_input), Some(to), _, _) => {
            Some(TokenParticipantFilter::FromTo {
                from_input: from_input.to_owned(),
                from,
                to_input: to_input.to_owned(),
                to,
            })
        }
        (Some(input), Some(address), _, _, _, _) => Some(TokenParticipantFilter::From {
            input: input.to_owned(),
            address,
        }),
        (_, _, Some(input), Some(address), _, _) => Some(TokenParticipantFilter::To {
            input: input.to_owned(),
            address,
        }),
        _ => None,
    }
}

fn transfer_filters(
    token: ContractAddress,
    from: Option<ContractAddress>,
    to: Option<ContractAddress>,
    account: Option<ContractAddress>,
) -> Vec<EventFilter> {
    if let Some(account) = account {
        return vec![
            transfer_filter(token, Some(account), None),
            transfer_filter(token, None, Some(account)),
        ];
    }
    vec![transfer_filter(token, from, to)]
}

fn transfer_filter(
    token: ContractAddress,
    from: Option<ContractAddress>,
    to: Option<ContractAddress>,
) -> EventFilter {
    EventFilter {
        contract_addresses: ContractAddressList(VecM(vec![token])),
        topics: EventTopicFilter {
            topic0: TopicList(VecM(vec![Topic(*TransferEvent::SIGNATURE)])),
            topic1: TopicList(VecM(from.map(address_topic).into_iter().collect())),
            topic2: TopicList(VecM(to.map(address_topic).into_iter().collect())),
        },
    }
}

fn address_topic(address: ContractAddress) -> Topic {
    let bytes: [u8; 32] = address
        .0
        .as_ref()
        .try_into()
        .expect("contract address must contain 32 bytes");
    Topic(bytes)
}

fn decode_transfer_record(event: IdentifiedEvent) -> TokenTransferRecordReport {
    let raw: RawEventReport = event.into();
    match decode_transfer(&raw) {
        Ok(transfer) => TokenTransferRecordReport {
            raw,
            decode_status: TokenTransferDecodeStatus::Decoded,
            transfer: Some(transfer),
            decode_error: None,
        },
        Err(error) => TokenTransferRecordReport {
            raw,
            decode_status: TokenTransferDecodeStatus::Malformed,
            transfer: None,
            decode_error: Some(terminal_safe(&error)),
        },
    }
}

fn decode_transfer(raw: &RawEventReport) -> Result<DecodedTokenTransferReport, String> {
    if raw.topics.len() != 3 {
        return Err(format!("expected 3 topics, got {}", raw.topics.len()));
    }
    if raw.topics[0].0 != *TransferEvent::SIGNATURE {
        return Err("topic 0 is not the canonical TransferEvent signature".into());
    }
    let from = ContractAddress::from(groundhog_xdr::types::ContractAddress::from(raw.topics[1].0));
    let to = ContractAddress::from(groundhog_xdr::types::ContractAddress::from(raw.topics[2].0));
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(&raw.data.0);
    let amount: i128 = xdrs::read_xdr(&mut reader)
        .map_err(|error| format!("failed to decode transfer amount: {error}"))?;
    if reader.remaining() != 0 {
        return Err(format!(
            "{} unread byte(s) remain after decoding transfer amount",
            reader.remaining()
        ));
    }
    if amount < 0 {
        return Err("transfer amount must be nonnegative".into());
    }
    let kind = if raw.topics[1].0 == ZERO_ADDRESS {
        TokenTransferKind::Mint
    } else if raw.topics[2].0 == ZERO_ADDRESS {
        TokenTransferKind::Burn
    } else {
        TokenTransferKind::Transfer
    };
    Ok(DecodedTokenTransferReport {
        kind,
        from,
        to,
        raw_amount: amount.to_string(),
    })
}

fn print_transfers(report: &TokenTransfersReport, continuation_requested: bool, print: &Print) {
    if print.is_quiet() {
        return;
    }
    let token = format_contract_address(&report.token_address.0);
    if !report.token_input.eq_ignore_ascii_case(&token) {
        print.detail("token", terminal_safe(&report.token_input));
    }
    print.detail("token address", token);
    if let Some(filter) = &report.participant_filter {
        print.detail("participant filter", format_participant_filter(filter));
    }
    print.detail("latest indexed block", *report.latest_block);
    print.detail("oldest retained block", *report.oldest_block);

    if report.records.is_empty() {
        let message = if !report.cursor.0.is_empty() {
            "no displayable events in this page; continue with cursor"
        } else if continuation_requested {
            "no events returned; no continuation cursor"
        } else {
            "no events found; stream exhausted"
        };
        print.detail("transfers", message);
    } else {
        for record in &report.records {
            print.status("");
            if let Some(transfer) = &record.transfer {
                print.status(transfer_kind_label(transfer.kind));
                print.detail("height", *record.raw.block_height);
                print.detail("sequence", record.raw.sequence_number);
                print.detail("txn", format_tx_hash(&record.raw.tx_hash));
                if transfer.kind != TokenTransferKind::Mint {
                    print.detail("from", format_contract_address(&transfer.from.0));
                }
                if transfer.kind != TokenTransferKind::Burn {
                    print.detail("to", format_contract_address(&transfer.to.0));
                }
                print.detail("raw amount", &transfer.raw_amount);
            } else {
                print.warn(format_args!(
                    "malformed transfer event at height {} sequence {}",
                    *record.raw.block_height, record.raw.sequence_number
                ));
                print.status("malformed transfer");
                print.detail("height", *record.raw.block_height);
                print.detail("sequence", record.raw.sequence_number);
                print.detail("txn", format_tx_hash(&record.raw.tx_hash));
                for (index, topic) in record.raw.topics.iter().enumerate() {
                    print.detail(
                        &format!("topic {index}"),
                        format_args!("0x{}", hex::encode(topic.0)),
                    );
                }
                print.detail(
                    "data",
                    format_args!("0x{}", hex::encode(&record.raw.data.0)),
                );
                print.detail(
                    "decode",
                    record
                        .decode_error
                        .as_deref()
                        .unwrap_or("malformed transfer event"),
                );
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

fn format_participant_filter(filter: &TokenParticipantFilter) -> String {
    match filter {
        TokenParticipantFilter::From { input, address } => format!(
            "from {} ({})",
            terminal_safe(input),
            format_contract_address(&address.0)
        ),
        TokenParticipantFilter::To { input, address } => format!(
            "to {} ({})",
            terminal_safe(input),
            format_contract_address(&address.0)
        ),
        TokenParticipantFilter::FromTo {
            from_input,
            from,
            to_input,
            to,
        } => format!(
            "from {} ({}) to {} ({})",
            terminal_safe(from_input),
            format_contract_address(&from.0),
            terminal_safe(to_input),
            format_contract_address(&to.0),
        ),
        TokenParticipantFilter::Account { input, address } => format!(
            "account {} ({})",
            terminal_safe(input),
            format_contract_address(&address.0)
        ),
    }
}

fn transfer_kind_label(kind: TokenTransferKind) -> &'static str {
    match kind {
        TokenTransferKind::Mint => "mint",
        TokenTransferKind::Burn => "burn",
        TokenTransferKind::Transfer => "transfer",
    }
}

#[cfg(test)]
mod tests {
    use groundhog_erc20_if::TransferEvent;
    use groundhog_json_rpc_types::events::{EmittedEvent, IdentifiedEvent};
    use groundhog_json_rpc_types::hex::{HexBytes, Topic};
    use groundhog_json_rpc_types::{BlockHeight, ContractAddress};

    use super::{ZERO_ADDRESS, address_topic, decode_transfer_record, transfer_filters};
    use crate::token_json::{TokenTransferDecodeStatus, TokenTransferKind};

    fn event(from: [u8; 32], to: [u8; 32], amount: i128) -> IdentifiedEvent {
        let from_address = groundhog_xdr::types::ContractAddress::from(from);
        let to_address = groundhog_xdr::types::ContractAddress::from(to);
        let transfer = TransferEvent::new(&from_address, &to_address, amount);
        IdentifiedEvent {
            tx_hash: serde_json::from_value(serde_json::Value::String(format!(
                "0x{}",
                hex::encode([0x44; 32])
            )))
            .unwrap(),
            block_height: BlockHeight::from(8),
            sequence_number: 0,
            event: EmittedEvent {
                contract_address: ContractAddress::from(
                    groundhog_xdr::types::ContractAddress::from([0x55; 32]),
                ),
                topics: vec![Topic(*TransferEvent::SIGNATURE), Topic(from), Topic(to)],
                data: HexBytes(xdrs::cxs::encode(&transfer).unwrap()),
            },
        }
    }

    #[test]
    fn classifies_transfer_mint_burn_and_zero_to_zero() {
        for (from, to, expected) in [
            ([1; 32], [2; 32], TokenTransferKind::Transfer),
            (ZERO_ADDRESS, [2; 32], TokenTransferKind::Mint),
            ([1; 32], ZERO_ADDRESS, TokenTransferKind::Burn),
            (ZERO_ADDRESS, ZERO_ADDRESS, TokenTransferKind::Mint),
        ] {
            let record = decode_transfer_record(event(from, to, 0));
            assert_eq!(record.decode_status, TokenTransferDecodeStatus::Decoded);
            assert_eq!(record.transfer.unwrap().kind, expected);
        }
    }

    #[test]
    fn self_transfer_remains_a_transfer() {
        let record = decode_transfer_record(event([1; 32], [1; 32], 0));

        assert_eq!(record.transfer.unwrap().kind, TokenTransferKind::Transfer);
    }

    #[test]
    fn negative_amount_is_malformed() {
        let record = decode_transfer_record(event([1; 32], [2; 32], -1));
        assert_eq!(record.decode_status, TokenTransferDecodeStatus::Malformed);
        assert!(record.decode_error.unwrap().contains("nonnegative"));
    }

    #[test]
    fn rejects_noncanonical_topic_shapes_and_amount_encoding() {
        let mut too_few_topics = event([1; 32], [2; 32], 1);
        too_few_topics.event.topics.pop();
        assert!(
            decode_transfer_record(too_few_topics)
                .decode_error
                .unwrap()
                .contains("expected 3 topics")
        );

        let mut wrong_signature = event([1; 32], [2; 32], 1);
        wrong_signature.event.topics[0] = Topic([0xff; 32]);
        assert!(
            decode_transfer_record(wrong_signature)
                .decode_error
                .unwrap()
                .contains("signature")
        );

        let mut trailing_data = event([1; 32], [2; 32], 1);
        trailing_data.event.data.0.push(0);
        assert!(
            decode_transfer_record(trailing_data)
                .decode_error
                .unwrap()
                .contains("unread byte")
        );

        let mut truncated_data = event([1; 32], [2; 32], 1);
        truncated_data.event.data.0.clear();
        assert!(
            decode_transfer_record(truncated_data)
                .decode_error
                .unwrap()
                .contains("decode transfer amount")
        );
    }

    #[test]
    fn builds_from_to_and_account_filters() {
        let token = ContractAddress::from(groundhog_xdr::types::ContractAddress::from([1; 32]));
        let from = ContractAddress::from(groundhog_xdr::types::ContractAddress::from([2; 32]));
        let to = ContractAddress::from(groundhog_xdr::types::ContractAddress::from([3; 32]));
        let signature = Topic(*TransferEvent::SIGNATURE);

        let from_to = transfer_filters(token, Some(from), Some(to), None);
        assert_eq!(from_to.len(), 1);
        assert_eq!(from_to[0].contract_addresses.0.0, vec![token]);
        assert_eq!(from_to[0].topics.topic0.0.0, vec![signature]);
        assert_eq!(from_to[0].topics.topic1.0.0, vec![address_topic(from)]);
        assert_eq!(from_to[0].topics.topic2.0.0, vec![address_topic(to)]);

        let account = transfer_filters(token, None, None, Some(from));
        assert_eq!(account.len(), 2);
        assert_eq!(account[0].topics.topic1.0.0, vec![address_topic(from)]);
        assert!(account[0].topics.topic2.0.0.is_empty());
        assert!(account[1].topics.topic1.0.0.is_empty());
        assert_eq!(account[1].topics.topic2.0.0, vec![address_topic(from)]);
    }
}
