use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read as _, Take};
use std::path::Path;

use groundhog_contract_spec::{
    ContractEventDef, ContractField, ContractSpec, ContractSpecId, ContractSpecString,
    ContractType, ContractTypeDef, ContractUnionCaseType,
};
use groundhog_json_rpc_types::events::IdentifiedEvent;
use groundhog_json_rpc_types::hex::Topic;

use crate::CliError;
use crate::codec::{
    TypeDefIndex, build_type_def_index, decode_event_spec_value_from_reader, spec_string_text,
};
use crate::contract_spec::load_contract_spec_from_xdr;
use crate::events_json::{
    DecodedEventReport, EventDecodeStatus, EventListItemReport, EventSpecReport, EventSpecSource,
};
use crate::terminal::terminal_safe;

const MAX_SPEC_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SPEC_MEMBERS: usize = 4_096;
const MAX_TYPE_DEPTH: usize = 64;
const MAX_EVENT_VALUES: usize = 4_096;
const MAX_VALIDATION_VISITS: usize = 65_536;
/// Topics an event may index, excluding topic 0's signature.
const MAX_EVENT_TOPICS: usize = 2;

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
        let spec = load_bounded_spec(path)
            .map_err(|error| invalid_spec(&path_text, &error.to_string()))?;
        let (events_by_name, events_by_signature) =
            validate_spec(&spec).map_err(|error| invalid_spec(&path_text, &error.to_string()))?;
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
            .expect("bug: validated event spec must have unique type definitions");
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
        let event_name = spec_string_text(&event_def.name.declared)
            .expect("bug: validated event name must be text")
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

/// Why a contract spec could not be loaded or its event definitions validated.
///
/// Every variant is reported against the spec's path by
/// [`ValidatedEventSpec::load`], so the messages name only what is wrong
/// inside the spec.
#[derive(Debug, thiserror::Error)]
enum SpecError {
    /// An RV32 artifact was passed where its sidecar spec belongs.
    #[error("RV32 artifacts do not embed contract specs; pass the sidecar at {sidecar}")]
    ArtifactHasNoSpec {
        /// Where the sidecar would be.
        sidecar: String,
    },
    /// The path does not name a spec this can read.
    #[error("unsupported file extension; expected .xdr")]
    UnsupportedExtension,
    /// The file could not be opened.
    #[error("failed to open file: {source}")]
    Open {
        #[source]
        source: std::io::Error,
    },
    /// The file could not be read.
    #[error("failed to read file: {source}")]
    Read {
        #[source]
        source: std::io::Error,
    },
    /// The file is larger than this is willing to read.
    #[error("artifact exceeds the {limit}-byte limit")]
    TooLarge {
        /// The byte cap.
        limit: u64,
    },
    /// The bytes are not a contract spec.
    #[error("{detail}")]
    Decode {
        /// What the decoder reported.
        detail: String,
    },
    /// A name field is not readable text.
    #[error("invalid {context}: {detail}")]
    InvalidName {
        /// What was being named, such as `event name`.
        context: &'static str,
        /// What the decoder reported.
        detail: String,
    },
    /// A name field is blank.
    #[error("{context} cannot be empty")]
    EmptyName {
        /// What was being named.
        context: &'static str,
    },
    /// A name used as an object key would decode as an index.
    #[error("{context} `{name}` cannot be numeric")]
    NumericName {
        /// What was being named.
        context: &'static str,
        /// The offending name.
        name: String,
    },
    /// Two fields of the same object claim the same name.
    #[error("duplicate {context} field name `{name}`")]
    DuplicateFieldName {
        /// What holds the fields, such as `event payload`.
        context: &'static str,
        /// The colliding field name.
        name: String,
    },
    /// Two definitions claim the same name or discriminant.
    #[error("duplicate {context} `{name}`")]
    Duplicate {
        /// What collided, such as `event name` or `union discriminant`.
        context: &'static str,
        /// The colliding name or value.
        name: String,
    },
    /// An event indexes more fields than a topic list holds.
    #[error("event `{name}` declares {count} indexed fields; maximum is {MAX_EVENT_TOPICS}")]
    TooManyIndexedFields {
        /// The event.
        name: String,
        /// How many fields it indexes.
        count: usize,
    },
    /// A named type refers to itself, directly or through another definition.
    #[error("cyclic event type definition involving `{name}`")]
    CyclicType {
        /// The type the cycle passes through.
        name: String,
    },
    /// A named type has no definition in the spec.
    #[error("unresolved event type `{name}`")]
    UnresolvedType {
        /// The missing type.
        name: String,
    },
    /// The spec declares more definitions and members than this will walk.
    #[error("spec contains {count} definitions and members; maximum is {MAX_SPEC_MEMBERS}")]
    TooManyMembers {
        /// How many the spec declares.
        count: usize,
    },
    /// Validation visited more types than its budget allows.
    #[error("event type validation exceeds the complexity limit")]
    ComplexityLimit,
    /// A type nests deeper than this will walk.
    #[error("event type exceeds the maximum depth of {MAX_TYPE_DEPTH}")]
    DepthLimit,
    /// Counting what an event can materialize overflowed.
    #[error("event value expansion overflows")]
    ValueOverflow,
    /// An event can materialize more values than a decode will emit.
    #[error("event may materialize {values} values; maximum is {MAX_EVENT_VALUES}")]
    TooManyValues {
        /// How many it can materialize.
        values: usize,
    },
    /// The spec uses a construct event decoding does not support.
    #[error("unsupported {construct}")]
    Unsupported {
        /// What is unsupported, such as `contract type in event payload`.
        construct: &'static str,
    },
}

fn load_bounded_spec(path: &Path) -> Result<ContractSpec, SpecError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("xdr") => {
            let bytes = read_bounded(path)?;
            load_contract_spec_from_xdr(&bytes).map_err(|error| SpecError::Decode {
                detail: error.to_string(),
            })
        }
        Some("rv32a") => Err(SpecError::ArtifactHasNoSpec {
            sidecar: path.with_extension("spec.xdr").display().to_string(),
        }),
        _ => Err(SpecError::UnsupportedExtension),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, SpecError> {
    let file = File::open(path).map_err(|source| SpecError::Open { source })?;
    let mut bytes = Vec::new();
    let mut reader: Take<File> = file.take(MAX_SPEC_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| SpecError::Read { source })?;
    if bytes.len() as u64 > MAX_SPEC_BYTES {
        return Err(SpecError::TooLarge {
            limit: MAX_SPEC_BYTES,
        });
    }
    Ok(bytes)
}

fn validate_spec(spec: &ContractSpec) -> Result<(EventNameIndex, EventSignatureIndex), SpecError> {
    validate_member_limit(spec)?;
    let type_defs = event_type_def_index(&spec.type_defs)?;
    let mut events_by_name = BTreeMap::new();
    let mut events_by_signature = BTreeMap::new();
    let mut remaining_visits = MAX_VALIDATION_VISITS;

    for (index, event) in spec.events.iter().enumerate() {
        let name = required_name(&event.name.declared, "event name", false)?;
        if events_by_name.insert(name.to_owned(), index).is_some() {
            return Err(SpecError::Duplicate {
                context: "event name",
                name: name.to_owned(),
            });
        }
        let signature = event.signature.into_inner();
        if events_by_signature.insert(signature, index).is_some() {
            return Err(SpecError::Duplicate {
                context: "signature for event",
                name: name.to_owned(),
            });
        }
        if event.topics.len() > MAX_EVENT_TOPICS {
            return Err(SpecError::TooManyIndexedFields {
                name: name.to_owned(),
                count: event.topics.len(),
            });
        }
        validate_fields(
            &event.topics,
            "event topic",
            &type_defs,
            &mut remaining_visits,
            false,
        )?;
        let payload_values = validate_fields(
            &event.fields,
            "event payload",
            &type_defs,
            &mut remaining_visits,
            true,
        )?;
        checked_values(1, payload_values)?;
    }
    Ok((events_by_name, events_by_signature))
}

fn validate_member_limit(spec: &ContractSpec) -> Result<(), SpecError> {
    let mut count = spec.events.len().saturating_add(spec.type_defs.len());
    check_member_limit(count)?;
    for event in spec.events.iter() {
        add_members(&mut count, event.topics.len())?;
        add_members(&mut count, event.fields.len())?;
        for field in event.topics.iter().chain(event.fields.iter()) {
            add_nested_tuple_members(&mut count, &field.ty)?;
        }
    }
    for def in spec.type_defs.iter() {
        match def {
            ContractTypeDef::TypeDefStruct { struct_def } => {
                add_members(&mut count, struct_def.fields.len())?;
                for field in struct_def.fields.iter() {
                    add_nested_tuple_members(&mut count, &field.ty)?;
                }
            }
            ContractTypeDef::TypeDefEnum { enum_def } => {
                add_members(&mut count, enum_def.variants.len())?;
            }
            ContractTypeDef::TypeDefUnion { union_def } => {
                for case in union_def.cases.iter() {
                    add_members(&mut count, 1)?;
                    match &case.case_type {
                        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                            add_members(&mut count, struct_fields.len())?;
                            for field in struct_fields.iter() {
                                add_nested_tuple_members(&mut count, &field.ty)?;
                            }
                        }
                        ContractUnionCaseType::UnionCaseTuple { tuple } => {
                            add_members(&mut count, tuple.len())?;
                            for ty in tuple.iter() {
                                add_nested_tuple_members(&mut count, ty)?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                return Err(SpecError::Unsupported {
                    construct: "contract type definition",
                });
            }
        }
    }
    Ok(())
}

fn add_nested_tuple_members(count: &mut usize, ty: &ContractType) -> Result<(), SpecError> {
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        match ty {
            ContractType::Option { option_inner } => pending.push(option_inner),
            ContractType::Vector { vector } => pending.push(&vector.element),
            ContractType::Array { array } => pending.push(&array.element),
            ContractType::Tuple { tuple } => {
                add_members(count, tuple.len())?;
                pending.extend(tuple.iter());
            }
            _ => {}
        }
    }
    Ok(())
}

fn add_members(count: &mut usize, members: usize) -> Result<(), SpecError> {
    *count = count.saturating_add(members);
    check_member_limit(*count)
}

fn check_member_limit(count: usize) -> Result<(), SpecError> {
    if count > MAX_SPEC_MEMBERS {
        return Err(SpecError::TooManyMembers { count });
    }
    Ok(())
}

fn event_type_def_index(
    definitions: &[ContractTypeDef],
) -> Result<BTreeMap<ContractSpecId, &ContractTypeDef>, SpecError> {
    let mut index = BTreeMap::new();
    for definition in definitions {
        let name = match definition {
            ContractTypeDef::TypeDefStruct { struct_def } => &struct_def.name,
            ContractTypeDef::TypeDefEnum { enum_def } => &enum_def.name,
            ContractTypeDef::TypeDefUnion { union_def } => &union_def.name,
            _ => {
                return Err(SpecError::Unsupported {
                    construct: "contract type definition",
                });
            }
        };
        let id = required_id(name, "type definition name")?;
        if index.insert(id.clone(), definition).is_some() {
            return Err(SpecError::Duplicate {
                context: "type definition name",
                name: id.to_string(),
            });
        }
    }
    Ok(index)
}

fn validate_fields(
    fields: &[ContractField],
    context: &'static str,
    type_defs: &BTreeMap<ContractSpecId, &ContractTypeDef>,
    remaining_visits: &mut usize,
    count_values: bool,
) -> Result<usize, SpecError> {
    let mut names = BTreeSet::new();
    let mut values = 0usize;
    for field in fields {
        let name = required_name(&field.name, context, true)?;
        if !names.insert(name.to_owned()) {
            return Err(duplicate_field(context, name));
        }
        let mut visiting = BTreeSet::new();
        values = values
            .checked_add(validate_type(
                &field.ty,
                type_defs,
                &mut visiting,
                remaining_visits,
                1,
                count_values,
            )?)
            .ok_or(SpecError::ValueOverflow)?;
    }
    Ok(values)
}

fn validate_type(
    ty: &ContractType,
    type_defs: &BTreeMap<ContractSpecId, &ContractTypeDef>,
    visiting: &mut BTreeSet<ContractSpecId>,
    remaining_visits: &mut usize,
    depth: usize,
    count_values: bool,
) -> Result<usize, SpecError> {
    validate_type_limits(remaining_visits, depth)?;
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
        | ContractType::U256
        | ContractType::I256
        | ContractType::String
        | ContractType::Bytes
        | ContractType::Address => Ok(usize::from(count_values)),
        ContractType::Option { option_inner } => validate_type(
            option_inner,
            type_defs,
            visiting,
            remaining_visits,
            depth + 1,
            count_values,
        ),
        ContractType::Vector { vector } => {
            if matches!(vector.element.as_ref(), ContractType::U8) {
                return Ok(usize::from(count_values));
            }
            validate_type(
                &vector.element,
                type_defs,
                visiting,
                remaining_visits,
                depth + 1,
                count_values,
            )?;
            Ok(usize::from(count_values))
        }
        ContractType::Array { array } => {
            if matches!(array.element.as_ref(), ContractType::U8) {
                return Ok(usize::from(count_values));
            }
            let inner = validate_type(
                &array.element,
                type_defs,
                visiting,
                remaining_visits,
                depth + 1,
                count_values,
            )?;
            if count_values {
                bounded_container(inner, array.len as usize)
            } else {
                Ok(0)
            }
        }
        ContractType::Tuple { tuple } => {
            let mut values = usize::from(count_values);
            for item in tuple.iter() {
                values = checked_values(
                    values,
                    validate_type(
                        item,
                        type_defs,
                        visiting,
                        remaining_visits,
                        depth + 1,
                        count_values,
                    )?,
                )?;
            }
            Ok(values)
        }
        ContractType::Named { named } => validate_named_type(
            named,
            type_defs,
            visiting,
            remaining_visits,
            depth,
            count_values,
        ),
        ContractType::SignedToken { .. } => Err(SpecError::Unsupported {
            construct: "signed-token values in event payloads",
        }),
        _ => Err(SpecError::Unsupported {
            construct: "contract type in event payload",
        }),
    }
}

/// Resolve a named type and validate its definition, refusing a cycle.
///
/// `visiting` holds the names on the current path; the name is removed again
/// on the way out so a type reached twice through sibling fields still passes.
fn validate_named_type(
    named: &ContractSpecId,
    type_defs: &BTreeMap<ContractSpecId, &ContractTypeDef>,
    visiting: &mut BTreeSet<ContractSpecId>,
    remaining_visits: &mut usize,
    depth: usize,
    count_values: bool,
) -> Result<usize, SpecError> {
    let id = required_id(named, "named type")?;
    if !visiting.insert(id.clone()) {
        return Err(SpecError::CyclicType {
            name: id.to_string(),
        });
    }
    let definition = type_defs.get(id).ok_or_else(|| SpecError::UnresolvedType {
        name: id.to_string(),
    })?;
    let result = validate_type_definition(
        definition,
        type_defs,
        visiting,
        remaining_visits,
        depth,
        count_values,
    );
    visiting.remove(id);
    result
}

fn validate_type_limits(remaining_visits: &mut usize, depth: usize) -> Result<(), SpecError> {
    *remaining_visits = remaining_visits
        .checked_sub(1)
        .ok_or(SpecError::ComplexityLimit)?;
    if depth > MAX_TYPE_DEPTH {
        return Err(SpecError::DepthLimit);
    }
    Ok(())
}

fn validate_type_definition(
    definition: &ContractTypeDef,
    type_defs: &BTreeMap<ContractSpecId, &ContractTypeDef>,
    visiting: &mut BTreeSet<ContractSpecId>,
    remaining_visits: &mut usize,
    depth: usize,
    count_values: bool,
) -> Result<usize, SpecError> {
    match definition {
        ContractTypeDef::TypeDefStruct { struct_def } => validate_named_fields(
            &struct_def.fields,
            "struct",
            type_defs,
            visiting,
            remaining_visits,
            depth,
            count_values,
        ),
        ContractTypeDef::TypeDefEnum { enum_def } => {
            let mut names = BTreeSet::new();
            let mut discriminants = BTreeSet::new();
            for variant in enum_def.variants.iter() {
                let name = required_name(&variant.name, "enum variant", false)?;
                if !names.insert(name) {
                    return Err(SpecError::Duplicate {
                        context: "enum variant name",
                        name: name.to_owned(),
                    });
                }
                if !discriminants.insert(variant.value) {
                    return Err(SpecError::Duplicate {
                        context: "enum discriminant",
                        name: variant.value.to_string(),
                    });
                }
            }
            Ok(usize::from(count_values))
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            let mut names = BTreeSet::new();
            let mut discriminants = BTreeSet::new();
            let mut max_values = if count_values { 2 } else { 0 };
            for case in union_def.cases.iter() {
                let name = required_name(&case.name, "union case", false)?;
                if !names.insert(name) {
                    return Err(SpecError::Duplicate {
                        context: "union case name",
                        name: name.to_owned(),
                    });
                }
                if !discriminants.insert(case.discriminant) {
                    return Err(SpecError::Duplicate {
                        context: "union discriminant",
                        name: case.discriminant.to_string(),
                    });
                }
                let values = match &case.case_type {
                    ContractUnionCaseType::UnionCaseVoid => {
                        if count_values {
                            2
                        } else {
                            0
                        }
                    }
                    ContractUnionCaseType::UnionCaseTuple { tuple } => {
                        let mut values = if count_values { 3 } else { 0 };
                        for ty in tuple.iter() {
                            values = checked_values(
                                values,
                                validate_type(
                                    ty,
                                    type_defs,
                                    visiting,
                                    remaining_visits,
                                    depth + 1,
                                    count_values,
                                )?,
                            )?;
                        }
                        values
                    }
                    ContractUnionCaseType::UnionCaseStruct { struct_fields } => checked_values(
                        if count_values { 2 } else { 0 },
                        validate_named_fields(
                            struct_fields,
                            "union struct",
                            type_defs,
                            visiting,
                            remaining_visits,
                            depth,
                            count_values,
                        )?,
                    )?,
                    _ => {
                        return Err(SpecError::Unsupported {
                            construct: "union case type in event payload",
                        });
                    }
                };
                max_values = max_values.max(values);
            }
            Ok(max_values)
        }
        _ => Err(SpecError::Unsupported {
            construct: "contract type definition",
        }),
    }
}

fn validate_named_fields(
    fields: &[ContractField],
    context: &'static str,
    type_defs: &BTreeMap<ContractSpecId, &ContractTypeDef>,
    visiting: &mut BTreeSet<ContractSpecId>,
    remaining_visits: &mut usize,
    depth: usize,
    count_values: bool,
) -> Result<usize, SpecError> {
    let mut names = BTreeSet::new();
    let mut values = usize::from(count_values);
    for field in fields {
        let name = required_name(&field.name, context, true)?;
        if !names.insert(name) {
            return Err(duplicate_field(context, name));
        }
        values = checked_values(
            values,
            validate_type(
                &field.ty,
                type_defs,
                visiting,
                remaining_visits,
                depth + 1,
                count_values,
            )?,
        )?;
    }
    Ok(values)
}

/// Validate a spec id and hand back the id itself.
///
/// Keyed lookups take the whole id -- a declared name alone is not an
/// identity, so two types that share one in different modules are different
/// types and must not collide in an index.
fn required_id<'a>(
    id: &'a ContractSpecId,
    context: &'static str,
) -> Result<&'a ContractSpecId, SpecError> {
    for segment in [&id.crate_name, &id.major, &id.module_path] {
        spec_string_text(segment).map_err(|error| SpecError::InvalidName {
            context,
            detail: error.to_string(),
        })?;
    }
    required_name(&id.declared, context, false)?;
    Ok(id)
}

fn required_name<'a>(
    name: &'a ContractSpecString,
    context: &'static str,
    object_key: bool,
) -> Result<&'a str, SpecError> {
    let name = spec_string_text(name).map_err(|error| SpecError::InvalidName {
        context,
        detail: error.to_string(),
    })?;
    if name.is_empty() {
        return Err(SpecError::EmptyName { context });
    }
    if object_key && name.parse::<usize>().is_ok() {
        return Err(SpecError::NumericName {
            context,
            name: name.to_owned(),
        });
    }
    Ok(name)
}

fn duplicate_field(context: &'static str, name: &str) -> SpecError {
    SpecError::DuplicateFieldName {
        context,
        name: name.to_owned(),
    }
}

fn bounded_container(inner: usize, len: usize) -> Result<usize, SpecError> {
    let children = inner.checked_mul(len).ok_or(SpecError::ValueOverflow)?;
    checked_values(1, children)
}

fn checked_values(left: usize, right: usize) -> Result<usize, SpecError> {
    let values = left.checked_add(right).ok_or(SpecError::ValueOverflow)?;
    if values > MAX_EVENT_VALUES {
        return Err(SpecError::TooManyValues { values });
    }
    Ok(values)
}

fn decode_known_event(
    event: &ContractEventDef,
    topics: &[Topic],
    data: &[u8],
    type_defs: &TypeDefIndex<'_>,
) -> Result<DecodedEventReport, CliError> {
    let name = spec_string_text(&event.name.declared)?.to_owned();
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
        ContractSpecId, ContractSpecString, ContractStructDef, ContractType, ContractTypeDef,
    };
    use groundhog_json_rpc_types::events::{EmittedEvent, IdentifiedEvent};
    use groundhog_json_rpc_types::hex::{HexBytes, Topic};
    use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
    use xdr_common::VecM;

    use super::{
        MAX_EVENT_VALUES, MAX_SPEC_BYTES, MAX_SPEC_MEMBERS, MAX_TYPE_DEPTH, MAX_VALIDATION_VISITS,
        SpecError, ValidatedEventSpec, read_bounded, validate_spec,
    };
    use crate::events_json::EventDecodeStatus;

    fn spec_string(value: &str) -> ContractSpecString {
        ContractSpecString::try_from(value).expect("fixture spec string should fit")
    }

    /// A fixture id: one crate, one module, so the declared name is what
    /// tells these types apart -- which is what these tests turn on.
    fn spec_id(declared: &str) -> ContractSpecId {
        ContractSpecId {
            crate_name: spec_string("demo"),
            major: spec_string("1"),
            module_path: spec_string("demo"),
            declared: spec_string(declared),
        }
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
                name: spec_id("PingEvent"),
                signature: [0x11; 32].into(),
                topics: VecM(vec![field("tag", ContractType::U64)]),
                fields: VecM(vec![field("value", field_type)]),
            }]),
        }
    }

    fn named_chain(definitions: usize) -> ContractSpec {
        let mut spec = event_spec(ContractType::Named {
            named: spec_id("Type0"),
        });
        spec.type_defs = VecM(
            (0..definitions)
                .map(|index| ContractTypeDef::TypeDefStruct {
                    struct_def: ContractStructDef {
                        name: spec_id(&format!("Type{index}")),
                        fields: VecM(vec![field(
                            "value",
                            if index + 1 == definitions {
                                ContractType::U64
                            } else {
                                ContractType::Named {
                                    named: spec_id(&format!("Type{}", index + 1)),
                                }
                            },
                        )]),
                    },
                })
                .collect(),
        );
        spec
    }

    fn nested_vectors(depth: usize) -> ContractType {
        let mut ty = ContractType::U64;
        for _ in 1..depth {
            ty = ContractType::Vector {
                vector: groundhog_contract_spec::ContractVectorType {
                    element: Box::new(ty),
                    max_len: 1,
                },
            };
        }
        ty
    }

    fn validation_visit_limit_spec() -> ContractSpec {
        assert_eq!(MAX_VALIDATION_VISITS % MAX_TYPE_DEPTH, 0);
        let field_count = MAX_VALIDATION_VISITS / MAX_TYPE_DEPTH;
        let mut spec = event_spec(ContractType::Void);
        spec.events.0[0].topics = VecM(vec![]);
        spec.events.0[0].fields = VecM(
            (0..field_count)
                .map(|index| field(&format!("field_{index}"), nested_vectors(MAX_TYPE_DEPTH)))
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
        assert_eq!(items[0].decoded.as_ref().unwrap().fields["value"], "7");
        assert_eq!(items[1].decode_status, EventDecodeStatus::UnknownSignature);
        assert!(items[1].decode_error.is_none());
        assert_eq!(items[2].decode_status, EventDecodeStatus::Malformed);
        assert!(items[2].decode_error.as_deref().unwrap().contains("unread"));
        assert_eq!(items[2].raw.data.0, trailing);
    }

    #[test]
    fn validation_and_decoding_accept_native_256_event_fields() {
        let field_type = ContractType::Tuple {
            tuple: vec![ContractType::U256, ContractType::I256]
                .try_into()
                .unwrap(),
        };
        let mut contract_spec = event_spec(field_type);
        contract_spec.events.0[0].topics = VecM(vec![
            field("unsigned", ContractType::U256),
            field("signed", ContractType::I256),
        ]);
        let spec = validated(contract_spec);
        let encoded = xdrs::cxs::encode(&(sdk::U256::MAX, sdk::I256::MIN)).unwrap();

        let item = spec
            .decode_events(vec![identified_event(0x11, 3, encoded)])
            .remove(0);

        assert_eq!(item.decode_status, EventDecodeStatus::Decoded);
        assert_eq!(
            item.decoded.unwrap().fields["value"],
            serde_json::json!([sdk::U256::MAX.to_string(), sdk::I256::MIN.to_string()])
        );
    }

    #[test]
    fn event_decoding_rejects_unknown_enum_but_call_codec_remains_permissive() {
        let enum_type = ContractType::Named {
            named: spec_id("Mode"),
        };
        let mut contract_spec = event_spec(enum_type.clone());
        contract_spec.type_defs = VecM(vec![ContractTypeDef::TypeDefEnum {
            enum_def: ContractEnumDef {
                name: spec_id("Mode"),
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
        assert!(matches!(
            validate_spec(&duplicate).unwrap_err(),
            SpecError::Duplicate {
                context: "event name",
                ..
            }
        ));

        let array = ContractType::Array {
            array: groundhog_contract_spec::ContractArrayType {
                element: Box::new(ContractType::U64),
                len: u32::try_from(MAX_EVENT_VALUES).unwrap(),
            },
        };
        assert!(matches!(
            validate_spec(&event_spec(array)).unwrap_err(),
            SpecError::TooManyValues { .. }
        ));
    }

    #[test]
    fn indexed_topics_ignore_payload_materialization_limit() {
        let large_array = || ContractType::Array {
            array: groundhog_contract_spec::ContractArrayType {
                element: Box::new(ContractType::U64),
                len: u32::try_from(MAX_EVENT_VALUES).unwrap(),
            },
        };
        let mut indexed = event_spec(ContractType::Void);
        indexed.events.0[0].topics = VecM(vec![field("large", large_array())]);

        validate_spec(&indexed).expect("indexed values should not use the payload value budget");
        assert!(matches!(
            validate_spec(&event_spec(large_array())).unwrap_err(),
            SpecError::TooManyValues { .. }
        ));
    }

    #[test]
    fn validation_rejects_duplicate_signatures_and_excess_indexed_fields() {
        let mut duplicate_signature = event_spec(ContractType::U64);
        let mut second_event = duplicate_signature.events[0].clone();
        second_event.name = spec_id("PongEvent");
        duplicate_signature.events.0.push(second_event);
        assert!(matches!(
            validate_spec(&duplicate_signature).unwrap_err(),
            SpecError::Duplicate {
                context: "signature for event",
                ..
            }
        ));

        let mut excess_topics = event_spec(ContractType::U64);
        excess_topics.events.0[0].topics.0.extend([
            field("second", ContractType::U64),
            field("third", ContractType::U64),
        ]);
        assert!(matches!(
            validate_spec(&excess_topics).unwrap_err(),
            SpecError::TooManyIndexedFields { count: 3, .. }
        ));
    }

    #[test]
    fn validation_rejects_unresolved_types_and_duplicate_field_names() {
        let unresolved = event_spec(ContractType::Named {
            named: spec_id("Missing"),
        });
        assert!(matches!(
            validate_spec(&unresolved).unwrap_err(),
            SpecError::UnresolvedType { name } if name == "demo::Missing (demo@1)"
        ));

        let mut duplicate_field = event_spec(ContractType::U64);
        duplicate_field.events.0[0]
            .fields
            .0
            .push(field("value", ContractType::Bool));
        assert_eq!(
            validate_spec(&duplicate_field).unwrap_err().to_string(),
            "duplicate event payload field name `value`"
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
        assert!(matches!(
            validate_spec(&at_member_limit).unwrap_err(),
            SpecError::TooManyMembers { .. }
        ));

        let nested_tuple = |members| ContractType::Option {
            option_inner: Box::new(ContractType::Tuple {
                tuple: VecM(vec![ContractType::Void; members]),
            }),
        };
        validate_spec(&event_spec(nested_tuple(MAX_SPEC_MEMBERS - 3)))
            .expect("nested tuple at exact member limit should be accepted");
        assert!(matches!(
            validate_spec(&event_spec(nested_tuple(MAX_SPEC_MEMBERS - 2))).unwrap_err(),
            SpecError::TooManyMembers { .. }
        ));

        let array = |len| ContractType::Array {
            array: groundhog_contract_spec::ContractArrayType {
                element: Box::new(ContractType::Void),
                len: u32::try_from(len).unwrap(),
            },
        };
        validate_spec(&event_spec(array(MAX_EVENT_VALUES - 2)))
            .expect("exact materialized value limit should be accepted");
        assert!(matches!(
            validate_spec(&event_spec(array(MAX_EVENT_VALUES - 1))).unwrap_err(),
            SpecError::TooManyValues { .. }
        ));
    }

    #[test]
    fn validation_visit_budget_counts_each_payload_type_once() {
        let mut at_limit = validation_visit_limit_spec();
        validate_spec(&at_limit).expect("exact validation visit limit should be accepted");

        at_limit.events.0[0]
            .fields
            .0
            .push(field("one_visit_too_many", ContractType::U64));
        assert!(matches!(
            validate_spec(&at_limit).unwrap_err(),
            SpecError::ComplexityLimit
        ));
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
        assert!(matches!(
            validate_spec(&named_chain(64)).unwrap_err(),
            SpecError::DepthLimit
        ));
    }

    #[test]
    fn validation_rejects_reachable_named_type_cycles() {
        let mut spec = named_chain(2);
        let ContractTypeDef::TypeDefStruct { struct_def } = &mut spec.type_defs.0[1] else {
            unreachable!()
        };
        struct_def.fields.0[0].ty = ContractType::Named {
            named: spec_id("Type0"),
        };

        assert!(matches!(
            validate_spec(&spec).unwrap_err(),
            SpecError::CyclicType { .. }
        ));
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
        assert!(matches!(
            read_bounded(&path).unwrap_err(),
            SpecError::TooLarge { .. }
        ));
    }
}
