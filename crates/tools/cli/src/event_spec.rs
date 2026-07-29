use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read as _, Take};
use std::path::Path;

use groundhog_contract_spec::{
    ContractEventDef, ContractField, ContractSpec, ContractSpecString, ContractType,
    ContractTypeDef, ContractUnionCaseType,
};
use groundhog_json_rpc_types::events::IdentifiedEvent;
use groundhog_json_rpc_types::hex::Topic;

use crate::CliError;
use crate::codec::{
    TypeDefIndex, build_type_def_index, decode_event_spec_value_from_reader, spec_string_text,
};
use crate::contract_spec::{load_contract_spec_from_xdr, load_embedded_contract_spec_from_bytes};
use crate::events_json::{
    DecodedEventReport, EventDecodeStatus, EventListItemReport, EventSpecReport, EventSpecSource,
};
use crate::terminal::terminal_safe;

const MAX_SPEC_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SPEC_MEMBERS: usize = 4_096;
const MAX_TYPE_DEPTH: usize = 64;
const MAX_EVENT_VALUES: usize = 4_096;
const MAX_VALIDATION_VISITS: usize = 65_536;

type EventNameIndex = BTreeMap<String, usize>;
type EventSignatureIndex = BTreeMap<[u8; 32], usize>;

/// A fully validated local event spec with deterministic lookup indexes.
pub(crate) struct ValidatedEventSpec {
    spec: ContractSpec,
    events_by_name: EventNameIndex,
    events_by_signature: EventSignatureIndex,
    path: String,
}

impl ValidatedEventSpec {
    /// Load and validate an explicitly selected trusted local artifact.
    pub(crate) fn load(path: &Path) -> Result<Self, CliError> {
        let path_text = path.to_string_lossy().into_owned();
        let spec = load_bounded_spec(path).map_err(|message| invalid_spec(&path_text, &message))?;
        let (events_by_name, events_by_signature) =
            validate_spec(&spec).map_err(|message| invalid_spec(&path_text, &message))?;
        Ok(Self {
            spec,
            events_by_name,
            events_by_signature,
            path: path_text,
        })
    }

    pub(crate) fn selected_signature(&self, name: &str) -> Result<Topic, CliError> {
        let index = self
            .events_by_name
            .get(name)
            .ok_or_else(|| CliError::InvalidEventOptions {
                message: format!(
                    "event `{}` is not declared by the supplied spec",
                    terminal_safe(name)
                ),
            })?;
        Ok(Topic(self.spec.events[*index].signature.into_inner()))
    }

    pub(crate) fn report(&self, selected_event: Option<&str>) -> EventSpecReport {
        EventSpecReport {
            source: EventSpecSource::UserSupplied,
            path: self.path.clone(),
            selected_event: selected_event.map(ToOwned::to_owned),
        }
    }

    pub(crate) fn decode_events(&self, events: Vec<IdentifiedEvent>) -> Vec<EventListItemReport> {
        let type_defs = build_type_def_index(&self.spec.type_defs)
            .expect("validated event spec must have unique type definitions");
        events
            .into_iter()
            .map(|event| self.decode_event(event, &type_defs))
            .collect()
    }

    fn decode_event(
        &self,
        event: IdentifiedEvent,
        type_defs: &TypeDefIndex<'_>,
    ) -> EventListItemReport {
        let raw: crate::events_json::RawEventReport = event.into();
        let Some(signature) = raw.topics.first().map(|topic| topic.0) else {
            return unknown_event(raw);
        };
        let Some(index) = self.events_by_signature.get(&signature) else {
            return unknown_event(raw);
        };
        let event_def = &self.spec.events[*index];
        let event_name = spec_string_text(&event_def.name)
            .expect("validated event name must be text")
            .to_owned();

        match decode_known_event(event_def, &raw.topics, &raw.data.0, type_defs) {
            Ok(decoded) => EventListItemReport {
                raw,
                decode_status: EventDecodeStatus::Decoded,
                decoded: Some(decoded),
                decode_error: None,
            },
            Err(error) => EventListItemReport {
                raw,
                decode_status: EventDecodeStatus::Malformed,
                decoded: None,
                decode_error: Some(terminal_safe(&format!("{event_name}: {error}"))),
            },
        }
    }
}

fn load_bounded_spec(path: &Path) -> Result<ContractSpec, String> {
    let bytes = read_bounded(path)?;

    match path.extension().and_then(|extension| extension.to_str()) {
        Some("xdr") => load_contract_spec_from_xdr(&bytes).map_err(|error| error.to_string()),
        Some("wasm") => load_embedded_contract_spec_from_bytes(&bytes)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "WASM artifact does not contain a contract spec".into()),
        _ => Err("unsupported file extension; expected .wasm or .xdr".into()),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("failed to open file: {error}"))?;
    let mut bytes = Vec::new();
    let mut reader: Take<File> = file.take(MAX_SPEC_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read file: {error}"))?;
    if bytes.len() as u64 > MAX_SPEC_BYTES {
        return Err(format!("artifact exceeds the {MAX_SPEC_BYTES}-byte limit"));
    }
    Ok(bytes)
}

fn validate_spec(spec: &ContractSpec) -> Result<(EventNameIndex, EventSignatureIndex), String> {
    validate_member_limit(spec)?;
    let type_defs = event_type_def_index(&spec.type_defs)?;
    let mut events_by_name = BTreeMap::new();
    let mut events_by_signature = BTreeMap::new();
    let mut remaining_visits = MAX_VALIDATION_VISITS;

    for (index, event) in spec.events.iter().enumerate() {
        let name = required_name(&event.name, "event name", false)?;
        if events_by_name.insert(name.to_owned(), index).is_some() {
            return Err(format!("duplicate event name `{name}`"));
        }
        let signature = event.signature.into_inner();
        if events_by_signature.insert(signature, index).is_some() {
            return Err(format!("duplicate signature for event `{name}`"));
        }
        if event.topics.len() > 2 {
            return Err(format!(
                "event `{name}` declares {} indexed fields; maximum is 2",
                event.topics.len()
            ));
        }
        validate_fields(
            &event.topics,
            "event topic",
            &type_defs,
            &mut remaining_visits,
        )?;
        validate_fields(
            &event.fields,
            "event payload",
            &type_defs,
            &mut remaining_visits,
        )?;

        let mut values = 1usize;
        for field in event.fields.iter() {
            let mut visiting = BTreeSet::new();
            values = checked_values(
                values,
                validate_type(
                    &field.ty,
                    &type_defs,
                    &mut visiting,
                    &mut remaining_visits,
                    1,
                )?,
            )?;
        }
    }
    Ok((events_by_name, events_by_signature))
}

fn validate_member_limit(spec: &ContractSpec) -> Result<(), String> {
    let mut count = spec.events.len().saturating_add(spec.type_defs.len());
    for event in spec.events.iter() {
        count = count
            .saturating_add(event.topics.len())
            .saturating_add(event.fields.len());
    }
    for def in spec.type_defs.iter() {
        count = count.saturating_add(match def {
            ContractTypeDef::TypeDefStruct { struct_def } => struct_def.fields.len(),
            ContractTypeDef::TypeDefEnum { enum_def } => enum_def.variants.len(),
            ContractTypeDef::TypeDefUnion { union_def } => union_def
                .cases
                .iter()
                .map(|case| {
                    1 + match &case.case_type {
                        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                            struct_fields.len()
                        }
                        ContractUnionCaseType::UnionCaseTuple { tuple } => tuple.len(),
                        _ => 0,
                    }
                })
                .sum(),
            _ => return Err("unsupported contract type definition".into()),
        });
    }
    if count > MAX_SPEC_MEMBERS {
        return Err(format!(
            "spec contains {count} definitions and members; maximum is {MAX_SPEC_MEMBERS}"
        ));
    }
    Ok(())
}

fn event_type_def_index(
    definitions: &[ContractTypeDef],
) -> Result<BTreeMap<String, &ContractTypeDef>, String> {
    let mut index = BTreeMap::new();
    for definition in definitions {
        let name = match definition {
            ContractTypeDef::TypeDefStruct { struct_def } => &struct_def.name,
            ContractTypeDef::TypeDefEnum { enum_def } => &enum_def.name,
            ContractTypeDef::TypeDefUnion { union_def } => &union_def.name,
            _ => return Err("unsupported contract type definition".into()),
        };
        let name = required_name(name, "type definition name", false)?;
        if index.insert(name.to_owned(), definition).is_some() {
            return Err(format!("duplicate type definition name `{name}`"));
        }
    }
    Ok(index)
}

fn validate_fields(
    fields: &[ContractField],
    context: &str,
    type_defs: &BTreeMap<String, &ContractTypeDef>,
    remaining_visits: &mut usize,
) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for field in fields {
        let name = required_name(&field.name, context, true)?;
        if !names.insert(name.to_owned()) {
            return Err(format!("duplicate {context} field name `{name}`"));
        }
        let mut visiting = BTreeSet::new();
        validate_type(&field.ty, type_defs, &mut visiting, remaining_visits, 1)?;
    }
    Ok(())
}

fn validate_type(
    ty: &ContractType,
    type_defs: &BTreeMap<String, &ContractTypeDef>,
    visiting: &mut BTreeSet<String>,
    remaining_visits: &mut usize,
    depth: usize,
) -> Result<usize, String> {
    *remaining_visits = remaining_visits
        .checked_sub(1)
        .ok_or_else(|| "event type validation exceeds the complexity limit".to_owned())?;
    if depth > MAX_TYPE_DEPTH {
        return Err(format!(
            "event type exceeds the maximum depth of {MAX_TYPE_DEPTH}"
        ));
    }
    match ty {
        ContractType::Void
        | ContractType::Bool
        | ContractType::U8
        | ContractType::U32
        | ContractType::I32
        | ContractType::U64
        | ContractType::I64
        | ContractType::U128
        | ContractType::I128
        | ContractType::String
        | ContractType::Bytes
        | ContractType::Address => Ok(1),
        ContractType::Option { option_inner } => validate_type(
            option_inner,
            type_defs,
            visiting,
            remaining_visits,
            depth + 1,
        ),
        ContractType::Vector { vector } => {
            if matches!(vector.element.as_ref(), ContractType::U8) {
                return Ok(1);
            }
            validate_type(
                &vector.element,
                type_defs,
                visiting,
                remaining_visits,
                depth + 1,
            )?;
            Ok(1)
        }
        ContractType::Array { array } => {
            if matches!(array.element.as_ref(), ContractType::U8) {
                return Ok(1);
            }
            let inner = validate_type(
                &array.element,
                type_defs,
                visiting,
                remaining_visits,
                depth + 1,
            )?;
            bounded_container(inner, array.len as usize)
        }
        ContractType::Tuple { tuple } => {
            let mut values = 1;
            for item in tuple.iter() {
                values = checked_values(
                    values,
                    validate_type(item, type_defs, visiting, remaining_visits, depth + 1)?,
                )?;
            }
            Ok(values)
        }
        ContractType::Named { named } => {
            let name = required_name(named, "named type", false)?;
            if !visiting.insert(name.to_owned()) {
                return Err(format!("cyclic event type definition involving `{name}`"));
            }
            let definition = type_defs
                .get(name)
                .ok_or_else(|| format!("unresolved event type `{name}`"))?;
            let result =
                validate_type_definition(definition, type_defs, visiting, remaining_visits, depth);
            visiting.remove(name);
            result
        }
        ContractType::SignedToken { .. } => {
            Err("signed-token values are not supported in event payloads".into())
        }
        _ => Err("unsupported contract type in event payload".into()),
    }
}

fn validate_type_definition(
    definition: &ContractTypeDef,
    type_defs: &BTreeMap<String, &ContractTypeDef>,
    visiting: &mut BTreeSet<String>,
    remaining_visits: &mut usize,
    depth: usize,
) -> Result<usize, String> {
    match definition {
        ContractTypeDef::TypeDefStruct { struct_def } => validate_named_fields(
            &struct_def.fields,
            "struct",
            type_defs,
            visiting,
            remaining_visits,
            depth,
        ),
        ContractTypeDef::TypeDefEnum { enum_def } => {
            let mut names = BTreeSet::new();
            let mut discriminants = BTreeSet::new();
            for variant in enum_def.variants.iter() {
                let name = required_name(&variant.name, "enum variant", false)?;
                if !names.insert(name) {
                    return Err(format!("duplicate enum variant name `{name}`"));
                }
                if !discriminants.insert(variant.value) {
                    return Err(format!("duplicate enum discriminant `{}`", variant.value));
                }
            }
            Ok(1)
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            let mut names = BTreeSet::new();
            let mut discriminants = BTreeSet::new();
            let mut max_values = 2;
            for case in union_def.cases.iter() {
                let name = required_name(&case.name, "union case", false)?;
                if !names.insert(name) {
                    return Err(format!("duplicate union case name `{name}`"));
                }
                if !discriminants.insert(case.discriminant) {
                    return Err(format!(
                        "duplicate union discriminant `{}`",
                        case.discriminant
                    ));
                }
                let values = match &case.case_type {
                    ContractUnionCaseType::UnionCaseVoid => 2,
                    ContractUnionCaseType::UnionCaseTuple { tuple } => {
                        let mut values = 3;
                        for ty in tuple.iter() {
                            values = checked_values(
                                values,
                                validate_type(
                                    ty,
                                    type_defs,
                                    visiting,
                                    remaining_visits,
                                    depth + 1,
                                )?,
                            )?;
                        }
                        values
                    }
                    ContractUnionCaseType::UnionCaseStruct { struct_fields } => checked_values(
                        2,
                        validate_named_fields(
                            struct_fields,
                            "union struct",
                            type_defs,
                            visiting,
                            remaining_visits,
                            depth,
                        )?,
                    )?,
                    _ => return Err("unsupported union case type in event payload".into()),
                };
                max_values = max_values.max(values);
            }
            Ok(max_values)
        }
        _ => Err("unsupported contract type definition".into()),
    }
}

fn validate_named_fields(
    fields: &[ContractField],
    context: &str,
    type_defs: &BTreeMap<String, &ContractTypeDef>,
    visiting: &mut BTreeSet<String>,
    remaining_visits: &mut usize,
    depth: usize,
) -> Result<usize, String> {
    let mut names = BTreeSet::new();
    let mut values = 1;
    for field in fields {
        let name = required_name(&field.name, context, true)?;
        if !names.insert(name) {
            return Err(format!("duplicate {context} field name `{name}`"));
        }
        values = checked_values(
            values,
            validate_type(&field.ty, type_defs, visiting, remaining_visits, depth + 1)?,
        )?;
    }
    Ok(values)
}

fn required_name<'a>(
    name: &'a ContractSpecString,
    context: &str,
    object_key: bool,
) -> Result<&'a str, String> {
    let name = spec_string_text(name).map_err(|error| format!("invalid {context}: {error}"))?;
    if name.is_empty() {
        return Err(format!("{context} cannot be empty"));
    }
    if object_key && name.parse::<usize>().is_ok() {
        return Err(format!("{context} `{name}` cannot be numeric"));
    }
    Ok(name)
}

fn bounded_container(inner: usize, len: usize) -> Result<usize, String> {
    let children = inner
        .checked_mul(len)
        .ok_or_else(|| "event value expansion overflows".to_owned())?;
    checked_values(1, children)
}

fn checked_values(left: usize, right: usize) -> Result<usize, String> {
    let values = left
        .checked_add(right)
        .ok_or_else(|| "event value expansion overflows".to_owned())?;
    if values > MAX_EVENT_VALUES {
        return Err(format!(
            "event may materialize {values} values; maximum is {MAX_EVENT_VALUES}"
        ));
    }
    Ok(values)
}

fn decode_known_event(
    event: &ContractEventDef,
    topics: &[Topic],
    data: &[u8],
    type_defs: &TypeDefIndex<'_>,
) -> Result<DecodedEventReport, CliError> {
    let name = spec_string_text(&event.name)?.to_owned();
    let expected_topics = event.topics.len() + 1;
    if topics.len() != expected_topics {
        return Err(CliError::CallDecode {
            path: "topics".into(),
            message: format!("expected {expected_topics} topics, got {}", topics.len()),
        });
    }

    let named_topics = event
        .topics
        .iter()
        .zip(&topics[1..])
        .map(|(field, topic)| Ok((spec_string_text(&field.name)?.to_owned(), *topic)))
        .collect::<Result<BTreeMap<_, _>, CliError>>()?;
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(data);
    let mut fields = BTreeMap::new();
    let mut remaining_values = MAX_EVENT_VALUES - 1;
    for field in event.fields.iter() {
        let field_name = spec_string_text(&field.name)?;
        let path = format!("fields.{field_name}");
        fields.insert(
            field_name.to_owned(),
            decode_event_spec_value_from_reader(
                &field.ty,
                &mut reader,
                type_defs,
                &path,
                &mut remaining_values,
            )?,
        );
    }
    if reader.remaining() != 0 {
        return Err(CliError::CallDecode {
            path: "fields".into(),
            message: format!(
                "{} unread byte(s) remain after decoding",
                reader.remaining()
            ),
        });
    }
    Ok(DecodedEventReport {
        name,
        topics: named_topics,
        fields,
    })
}

fn unknown_event(raw: crate::events_json::RawEventReport) -> EventListItemReport {
    EventListItemReport {
        raw,
        decode_status: EventDecodeStatus::UnknownSignature,
        decoded: None,
        decode_error: None,
    }
}

fn invalid_spec(path: &str, message: &str) -> CliError {
    CliError::InvalidEventSpec {
        path: path.to_owned(),
        display_path: terminal_safe(path),
        message: terminal_safe(message),
    }
}

#[cfg(test)]
mod tests {
    use groundhog_contract_spec::{
        ContractEnumDef, ContractEnumVariant, ContractEventDef, ContractField, ContractSpec,
        ContractSpecString, ContractStructDef, ContractType, ContractTypeDef,
    };
    use groundhog_json_rpc_types::events::{EmittedEvent, IdentifiedEvent};
    use groundhog_json_rpc_types::hex::{HexBytes, Topic};
    use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
    use xdr_common::VecM;

    use super::{
        MAX_EVENT_VALUES, MAX_SPEC_BYTES, MAX_SPEC_MEMBERS, ValidatedEventSpec, read_bounded,
        validate_spec,
    };
    use crate::events_json::EventDecodeStatus;

    fn spec_string(value: &str) -> ContractSpecString {
        ContractSpecString::try_from(value).expect("fixture spec string should fit")
    }

    fn field(name: &str, ty: ContractType) -> ContractField {
        ContractField {
            name: spec_string(name),
            ty,
        }
    }

    fn event_spec(field_type: ContractType) -> ContractSpec {
        ContractSpec {
            interfaces: VecM(vec![]),
            type_defs: VecM(vec![]),
            events: VecM(vec![ContractEventDef {
                name: spec_string("PingEvent"),
                signature: [0x11; 32].into(),
                topics: VecM(vec![field("tag", ContractType::U64)]),
                fields: VecM(vec![field("value", field_type)]),
            }]),
        }
    }

    fn named_chain(definitions: usize) -> ContractSpec {
        let mut spec = event_spec(ContractType::Named {
            named: spec_string("Type0"),
        });
        spec.type_defs = VecM(
            (0..definitions)
                .map(|index| ContractTypeDef::TypeDefStruct {
                    struct_def: ContractStructDef {
                        name: spec_string(&format!("Type{index}")),
                        fields: VecM(vec![field(
                            "value",
                            if index + 1 == definitions {
                                ContractType::U64
                            } else {
                                ContractType::Named {
                                    named: spec_string(&format!("Type{}", index + 1)),
                                }
                            },
                        )]),
                    },
                })
                .collect(),
        );
        spec
    }

    fn identified_event(signature: u8, topics: usize, data: Vec<u8>) -> IdentifiedEvent {
        IdentifiedEvent {
            tx_hash: serde_json::from_value(serde_json::Value::String(format!(
                "0x{}",
                hex::encode([0x22; 32])
            )))
            .expect("fixture hash should parse"),
            block_height: BlockHeight::from(42),
            sequence_number: 3,
            event: EmittedEvent {
                contract_address: ContractAddress::from(
                    groundhog_xdr::types::ContractAddress::from([0x33; 32]),
                ),
                topics: (0..topics)
                    .map(|index| Topic([if index == 0 { signature } else { 0x44 }; 32]))
                    .collect(),
                data: HexBytes(data),
            },
        }
    }

    fn validated(spec: ContractSpec) -> ValidatedEventSpec {
        let (events_by_name, events_by_signature) = validate_spec(&spec).unwrap();
        ValidatedEventSpec {
            spec,
            events_by_name,
            events_by_signature,
            path: "fixture.xdr".into(),
        }
    }

    #[test]
    fn decoding_preserves_mixed_raw_events_and_item_statuses() {
        let spec = validated(event_spec(ContractType::U64));
        let encoded = xdrs::cxs::encode(&7_u64).unwrap();
        let mut trailing = encoded.clone();
        trailing.extend([0, 0, 0, 0]);

        let items = spec.decode_events(vec![
            identified_event(0x11, 2, encoded),
            identified_event(0x99, 1, vec![]),
            identified_event(0x11, 2, trailing.clone()),
        ]);

        assert_eq!(items[0].decode_status, EventDecodeStatus::Decoded);
        assert_eq!(items[0].decoded.as_ref().unwrap().fields["value"], 7);
        assert_eq!(items[1].decode_status, EventDecodeStatus::UnknownSignature);
        assert!(items[1].decode_error.is_none());
        assert_eq!(items[2].decode_status, EventDecodeStatus::Malformed);
        assert!(items[2].decode_error.as_deref().unwrap().contains("unread"));
        assert_eq!(items[2].raw.data.0, trailing);
    }

    #[test]
    fn event_decoding_rejects_unknown_enum_but_call_codec_remains_permissive() {
        let enum_type = ContractType::Named {
            named: spec_string("Mode"),
        };
        let mut contract_spec = event_spec(enum_type.clone());
        contract_spec.type_defs = VecM(vec![ContractTypeDef::TypeDefEnum {
            enum_def: ContractEnumDef {
                name: spec_string("Mode"),
                variants: VecM(vec![ContractEnumVariant {
                    name: spec_string("Ready"),
                    value: 1,
                }]),
            },
        }]);
        let spec = validated(contract_spec);
        let encoded = xdrs::cxs::encode(&9_i32).unwrap();

        let item = spec
            .decode_events(vec![identified_event(0x11, 2, encoded.clone())])
            .remove(0);
        assert_eq!(item.decode_status, EventDecodeStatus::Malformed);
        assert!(
            item.decode_error
                .as_deref()
                .unwrap()
                .contains("unknown enum discriminant")
        );

        let type_defs = crate::codec::build_type_def_index(&spec.spec.type_defs).unwrap();
        assert_eq!(
            crate::codec::decode_spec_value(&enum_type, &encoded, &type_defs).unwrap(),
            serde_json::json!(9)
        );
    }

    #[test]
    fn validation_rejects_ambiguous_and_fixed_unbounded_event_shapes() {
        let mut duplicate = event_spec(ContractType::U64);
        duplicate.events.0.push(duplicate.events[0].clone());
        assert!(
            validate_spec(&duplicate)
                .unwrap_err()
                .contains("duplicate event name")
        );

        let array = ContractType::Array {
            array: groundhog_contract_spec::ContractArrayType {
                element: Box::new(ContractType::U64),
                len: u32::try_from(MAX_EVENT_VALUES).unwrap(),
            },
        };
        assert!(
            validate_spec(&event_spec(array))
                .unwrap_err()
                .contains("may materialize")
        );
    }

    #[test]
    fn validation_rejects_duplicate_signatures_and_excess_indexed_fields() {
        let mut duplicate_signature = event_spec(ContractType::U64);
        let mut second_event = duplicate_signature.events[0].clone();
        second_event.name = spec_string("PongEvent");
        duplicate_signature.events.0.push(second_event);
        assert!(
            validate_spec(&duplicate_signature)
                .unwrap_err()
                .contains("duplicate signature")
        );

        let mut excess_topics = event_spec(ContractType::U64);
        excess_topics.events.0[0].topics.0.extend([
            field("second", ContractType::U64),
            field("third", ContractType::U64),
        ]);
        assert!(
            validate_spec(&excess_topics)
                .unwrap_err()
                .contains("maximum is 2")
        );
    }

    #[test]
    fn validation_rejects_unresolved_types_and_duplicate_field_names() {
        let unresolved = event_spec(ContractType::Named {
            named: spec_string("Missing"),
        });
        assert!(
            validate_spec(&unresolved)
                .unwrap_err()
                .contains("unresolved event type `Missing`")
        );

        let mut duplicate_field = event_spec(ContractType::U64);
        duplicate_field.events.0[0]
            .fields
            .0
            .push(field("value", ContractType::Bool));
        assert!(
            validate_spec(&duplicate_field)
                .unwrap_err()
                .contains("duplicate event payload field name `value`")
        );
    }

    #[test]
    fn validation_enforces_member_and_materialized_value_boundaries() {
        let mut at_member_limit = event_spec(ContractType::Void);
        at_member_limit.events.0[0].fields = VecM(
            (0..MAX_SPEC_MEMBERS - 2)
                .map(|index| field(&format!("field_{index}"), ContractType::Void))
                .collect(),
        );
        validate_spec(&at_member_limit).expect("exact member limit should be accepted");
        at_member_limit.events.0[0]
            .fields
            .0
            .push(field("one_too_many", ContractType::Void));
        assert!(
            validate_spec(&at_member_limit)
                .unwrap_err()
                .contains("definitions and members")
        );

        let array = |len| ContractType::Array {
            array: groundhog_contract_spec::ContractArrayType {
                element: Box::new(ContractType::Void),
                len: u32::try_from(len).unwrap(),
            },
        };
        validate_spec(&event_spec(array(MAX_EVENT_VALUES - 2)))
            .expect("exact materialized value limit should be accepted");
        assert!(
            validate_spec(&event_spec(array(MAX_EVENT_VALUES - 1)))
                .unwrap_err()
                .contains("may materialize")
        );
    }

    #[test]
    fn dynamic_vector_exhaustion_is_an_item_level_decode_failure() {
        let vector = ContractType::Vector {
            vector: groundhog_contract_spec::ContractVectorType {
                element: Box::new(ContractType::Void),
                max_len: u32::try_from(MAX_EVENT_VALUES + 1).unwrap(),
            },
        };
        let spec = validated(event_spec(vector));
        let encoded = xdrs::cxs::encode(&u32::try_from(MAX_EVENT_VALUES).unwrap()).unwrap();

        let item = spec
            .decode_events(vec![identified_event(0x11, 2, encoded)])
            .remove(0);

        assert_eq!(item.decode_status, EventDecodeStatus::Malformed);
        assert!(
            item.decode_error
                .as_deref()
                .unwrap()
                .contains("value limit")
        );
    }

    #[test]
    fn present_option_uses_only_its_materialized_inner_value_budget() {
        let field_type = ContractType::Option {
            option_inner: Box::new(ContractType::Array {
                array: groundhog_contract_spec::ContractArrayType {
                    element: Box::new(ContractType::Void),
                    len: u32::try_from(MAX_EVENT_VALUES - 2).unwrap(),
                },
            }),
        };
        let spec = validated(event_spec(field_type));
        let encoded = xdrs::cxs::encode(&true).unwrap();

        let item = spec
            .decode_events(vec![identified_event(0x11, 2, encoded)])
            .remove(0);

        assert_eq!(item.decode_status, EventDecodeStatus::Decoded);
        assert_eq!(
            item.decoded.unwrap().fields["value"]
                .as_array()
                .unwrap()
                .len(),
            MAX_EVENT_VALUES - 2
        );
    }

    #[test]
    fn type_depth_accepts_64_levels_and_rejects_65() {
        validate_spec(&named_chain(63)).expect("63 named types plus one scalar is 64 levels");
        assert!(
            validate_spec(&named_chain(64))
                .unwrap_err()
                .contains("maximum depth")
        );
    }

    #[test]
    fn validation_rejects_reachable_named_type_cycles() {
        let mut spec = named_chain(2);
        let ContractTypeDef::TypeDefStruct { struct_def } = &mut spec.type_defs.0[1] else {
            unreachable!()
        };
        struct_def.fields.0[0].ty = ContractType::Named {
            named: spec_string("Type0"),
        };

        assert!(validate_spec(&spec).unwrap_err().contains("cyclic"));
    }

    #[test]
    fn selected_event_returns_its_signature() {
        let spec = validated(event_spec(ContractType::U64));
        assert_eq!(
            spec.selected_signature("PingEvent").unwrap(),
            Topic([0x11; 32])
        );
        assert_eq!(
            spec.selected_signature("pingevent").unwrap_err().code(),
            "invalid_event_options"
        );
    }

    #[test]
    fn artifact_read_accepts_exact_limit_and_rejects_one_extra_byte() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.spec.xdr");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_SPEC_BYTES).unwrap();
        assert_eq!(read_bounded(&path).unwrap().len() as u64, MAX_SPEC_BYTES);

        file.set_len(MAX_SPEC_BYTES + 1).unwrap();
        assert!(read_bounded(&path).unwrap_err().contains("exceeds"));
    }
}
