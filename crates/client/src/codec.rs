//! Call argument and return-value codecs for contract specs and local source metadata.

use groundhog_json_rpc_types::hex::{FixedHexError, decode_fixed_hex, prefixed_hex};
use std::collections::BTreeMap;
use std::path::PathBuf;

use groundhog_contract_spec::{
    ContractArrayType, ContractEnumDef, ContractExceptionType, ContractField, ContractSpecMethod,
    ContractSpecString, ContractStructDef, ContractType, ContractTypeDef, ContractUnionCase,
    ContractUnionCaseType, ContractUnionDef, ContractVectorType,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
use serde_json::Value;
use xdrs::{Reader as _, Writer as _};

/// Contract metadata, argument encoding, or value decoding failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("contract spec finalization failed: {0}")]
    SpecFailed(String),
    #[error("invalid {argument}: {message}")]
    InvalidArgument { argument: String, message: String },
    #[error("{path}: {message}")]
    CallEncode { path: String, message: String },
    #[error("{path}: {message}")]
    CallDecode { path: String, message: String },
    #[error("{0}")]
    Other(String),
}

/// Derive the canonical method selector from interface identity and method name.
pub fn method_entrypoint_id(
    interface_name: &str,
    version: &str,
    method: &str,
) -> ContractEntrypointID {
    let sig = groundhog_contract_util::NormalizedMethodSignature::new(method);
    let id = groundhog_contract_util::contract_entrypoint_id(Some(interface_name), version, &sig);
    let bytes: [u8; 8] = id.as_ref()[..8]
        .try_into()
        .expect("bug: entrypoint ID is always 8 bytes");
    ContractEntrypointID::from(bytes)
}

/// Resolve a method from a supplied spec, rejecting ambiguous interface matches.
/// Returns `None` when the method is absent; does not read files or discover projects.
pub fn resolve_spec_method(
    spec: &groundhog_contract_spec::ContractSpec,
    method: &str,
) -> Result<Option<SpecMethodMetadata>, Error> {
    let mut interface_matches = Vec::new();

    for interface in spec.interfaces.iter() {
        let matched_methods = interface
            .methods
            .iter()
            .filter_map(|candidate| match spec_string_text(&candidate.name) {
                Ok(name) if name == method => Some(Ok(candidate.clone())),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            })
            .collect::<Result<Vec<_>, Error>>()?;
        if matched_methods.is_empty() {
            continue;
        }

        interface_matches.push((
            (
                spec_string_text(&interface.name)?.to_owned(),
                spec_string_text(&interface.vers)?.to_owned(),
            ),
            interface.throws.iter().cloned().collect::<Vec<_>>(),
            matched_methods,
        ));
    }

    if interface_matches.is_empty() {
        return Ok(None);
    }
    interface_matches.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    if interface_matches.len() > 1 {
        let descriptions = interface_matches
            .iter()
            .map(|(candidate, _, _)| format!("{} v{}", candidate.0, candidate.1))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Other(format!(
            "method `{method}` matches multiple interfaces in the contract spec: \
             {descriptions}"
        )));
    }

    let (matched, throws, mut methods) = interface_matches
        .into_iter()
        .next()
        .expect("bug: interface match count should be non-zero");
    if methods.len() > 1 {
        return Err(Error::SpecFailed(format!(
            "contract spec defines `{}` v{}::{method} multiple times",
            matched.0, matched.1
        )));
    }

    Ok(Some(SpecMethodMetadata {
        interface_name: matched.0,
        version: matched.1,
        method: methods.remove(0),
        type_defs: spec.type_defs.iter().cloned().collect(),
        throws,
    }))
}

/// Type metadata used to encode arguments and decode return values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallMetadata {
    Spec(SpecMethodMetadata),
    Local(ContractMetadata),
}

/// Locally parsed method metadata recovered from project source files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractMetadata {
    pub interface_name: String,
    pub version: String,
    pub source: PathBuf,
    pub method: MethodMetadata,
}

/// Local argument and return type names parsed from a Rust method signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodMetadata {
    pub arg_types: Vec<String>,
    pub return_type: Option<String>,
}

/// Contract spec metadata for one resolved contract method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecMethodMetadata {
    pub interface_name: String,
    pub version: String,
    pub method: ContractSpecMethod,
    pub type_defs: Vec<ContractTypeDef>,
    pub throws: Vec<ContractExceptionType>,
}

/// Named contract type definitions keyed by contract type name.
pub type TypeDefIndex<'a> = BTreeMap<String, &'a ContractTypeDef>;

/// Encode argument strings into contract calldata using the resolved metadata.
pub fn encode_args(args: &[String], metadata: Option<&CallMetadata>) -> Result<Vec<u8>, Error> {
    if let Some(metadata) = metadata {
        return match metadata {
            CallMetadata::Spec(metadata) => encode_spec_typed_args(args, metadata),
            CallMetadata::Local(metadata) => encode_local_typed_args(args, &metadata.method),
        };
    }
    if args.is_empty() {
        return Ok(vec![]);
    }
    if args.len() == 1 {
        let stripped = args[0].strip_prefix("0x").unwrap_or(&args[0]);
        return hex::decode(stripped)
            .map_err(|e| encode_value_error("calldata", format!("invalid calldata hex: {e}")));
    }
    Err(encode_value_error(
        "arguments",
        "multiple call arguments require contract metadata or a single hex-encoded calldata argument"
            .to_owned(),
    ))
}

/// Decode contract return bytes into JSON using the resolved metadata when available.
pub fn decode_return(bytes: &[u8], metadata: Option<&CallMetadata>) -> Option<Value> {
    match metadata? {
        CallMetadata::Spec(metadata) => {
            if matches!(metadata.method.return_type, ContractType::Void) {
                return None;
            }
            let type_defs = build_type_def_index(&metadata.type_defs).ok()?;
            decode_spec_value(&metadata.method.return_type, bytes, &type_defs).ok()
        }
        CallMetadata::Local(metadata) => {
            let ty = metadata.method.return_type.as_deref()?;
            decode_single_value(ty, bytes)
        }
    }
}

/// A contract abort payload decoded through exception metadata.
///
/// Frontends choose which fields to include in their output.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAbort {
    /// Exception type name, when the thrown type is a named spec type.
    pub exception: Option<String>,
    /// Decoded exception payload.
    pub value: Value,
    /// Crate that defines the exception, when it decoded through the
    /// describe registry rather than the method's declared spec exceptions.
    pub exception_crate: Option<&'static str>,
    /// Major version of the defining crate baked into the abort selector,
    /// when known.
    pub exception_version: Option<&'static str>,
}

impl DecodedAbort {
    /// Exception name annotated with its defining crate and major version,
    /// e.g. `LibraryExn (sdk, v1)`; the bare name when the origin is unknown.
    pub fn exception_label(&self) -> Option<String> {
        let name = self.exception.as_deref()?;
        match (self.exception_crate, self.exception_version) {
            (Some(crate_name), Some(version)) => Some(format!("{name} ({crate_name}, v{version})")),
            _ => Some(name.to_owned()),
        }
    }
}

pub fn is_erc20_too_large(decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>) -> bool {
    matches!(
        decoded_abort,
        Some(Ok(DecodedAbort {
            exception: Some(exception),
            value: Value::String(value),
            exception_crate: Some("groundhog-erc20-if"),
            ..
        })) if exception == "ERC20Error" && value == "TooLarge"
    )
}

/// Why an abort buffer could not be decoded into an exception value.
///
/// The cases mirror the SDK's generated client error enum: `Unknown` and
/// `BadDecode` retain the parsed selector and payload (the abort prefix was
/// readable), while `UnparseableAbort` means even the selector could not be
/// read and only the raw buffer is meaningful.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AbortDecodeError {
    #[error("failed to read the exception selector: {detail}")]
    UnparseableAbort { detail: String },
    #[error(
        "abort selector 0x{selector:016x} does not match a declared exception or a known \
         standard exception; payload: 0x{}", hex::encode(.payload)
    )]
    Unknown { selector: u64, payload: Vec<u8> },
    #[error(
        "abort payload for {exception} (selector 0x{selector:016x}) failed to decode: {detail}; \
         payload: 0x{}", hex::encode(.payload)
    )]
    BadDecode {
        selector: u64,
        exception: String,
        payload: Vec<u8>,
        detail: String,
    },
}

/// Decode an abort buffer (`[selector][payload]`) into the exception it
/// carries.
///
/// The selector is matched against the resolved method's declared spec
/// exceptions. Frontends may resolve an unknown selector against their own
/// standard exception registry and call [`decode_abort_payload`].
/// An unknown selector or a payload that does not decode cleanly against
/// the matched type is an error carrying the parsed selector and payload;
/// callers report it alongside the raw hex.
pub fn decode_abort(
    bytes: &[u8],
    metadata: Option<&CallMetadata>,
) -> Result<DecodedAbort, AbortDecodeError> {
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(bytes);
    let selector: u64 = read_xdr_value(&mut reader, "abort selector").map_err(|e| {
        AbortDecodeError::UnparseableAbort {
            detail: e.to_string(),
        }
    })?;
    let payload = &bytes[bytes.len() - reader.remaining()..];

    if let Some(CallMetadata::Spec(spec)) = metadata
        && let Some(exception) = spec.throws.iter().find(|e| e.selector == selector)
    {
        return decode_abort_payload(&exception.ty, &spec.type_defs, selector, payload);
    }

    Err(AbortDecodeError::Unknown {
        selector,
        payload: payload.to_vec(),
    })
}

/// Decode a payload against caller-supplied exception and type definitions.
pub fn decode_abort_payload(
    ty: &ContractType,
    type_defs: &[ContractTypeDef],
    selector: u64,
    payload: &[u8],
) -> Result<DecodedAbort, AbortDecodeError> {
    let exception = match ty {
        ContractType::Named { named } => spec_string_text(named).ok().map(str::to_owned),
        _ => None,
    };
    let bad_decode = |detail: String| AbortDecodeError::BadDecode {
        selector,
        exception: exception
            .clone()
            .unwrap_or_else(|| "the declared exception".to_owned()),
        payload: payload.to_vec(),
        detail,
    };
    let index = build_type_def_index(type_defs).map_err(|e| bad_decode(e.to_string()))?;
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(payload);
    let value = decode_spec_value_from_reader(ty, &mut reader, &index, "abort value")
        .map_err(|e| bad_decode(e.to_string()))?;
    if reader.remaining() != 0 {
        return Err(bad_decode(format!(
            "{} unread byte(s) remain after decoding",
            reader.remaining()
        )));
    }
    Ok(DecodedAbort {
        exception,
        value,
        exception_crate: None,
        exception_version: None,
    })
}

/// Parse a C `StrKey` or legacy hex contract address, including in typed calldata.
pub fn parse_contract_address(value: &str) -> Result<ContractAddress, Error> {
    groundhog_signing::strkey::parse_contract_address(value).map_err(|error| {
        Error::InvalidArgument {
            argument: "contract address".into(),
            message: error.to_string(),
        }
    })
}

fn parse_public_key(value: &str) -> Result<Ed25519PublicKey, Error> {
    groundhog_signing::strkey::parse_public_key(value)
        .map(Ed25519PublicKey::from)
        .map_err(|error| Error::InvalidArgument {
            argument: "ed25519 public key".into(),
            message: error.to_string(),
        })
}

/// Format a contract address as 0x-prefixed lowercase hex.
pub fn format_contract_address(address: &ContractAddress) -> String {
    prefixed_hex(address)
}

/// Decode a spec string as UTF-8 text and validate it as a safe identifier.
///
/// Spec strings originate from Rust type and method names. A string
/// containing quotes, backslashes, or whitespace signals a malformed
/// spec and is rejected rather than propagated into generated output.
pub fn spec_string_text(value: &ContractSpecString) -> Result<&str, Error> {
    let s = std::str::from_utf8(value.as_ref())
        .map_err(|e| Error::SpecFailed(format!("contract spec contains invalid UTF-8: {e}")))?;
    if let Some(bad) = s
        .chars()
        .find(|&c| c == '"' || c == '\\' || c.is_whitespace())
    {
        return Err(Error::SpecFailed(format!(
            "contract spec name `{s}` contains invalid character `{bad}`"
        )));
    }
    Ok(s)
}

/// Build an index of named embedded type definitions.
pub fn build_type_def_index(type_defs: &[ContractTypeDef]) -> Result<TypeDefIndex<'_>, Error> {
    let mut index = BTreeMap::new();

    for type_def in type_defs {
        let name = spec_type_def_name(type_def)?.to_owned();
        if index.insert(name.clone(), type_def).is_some() {
            return Err(Error::SpecFailed(format!(
                "contract spec defines `{name}` multiple times"
            )));
        }
    }

    Ok(index)
}

/// Decode one top-level contract spec value from a byte slice.
pub fn decode_spec_value(
    ty: &ContractType,
    bytes: &[u8],
    type_defs: &TypeDefIndex<'_>,
) -> Result<Value, Error> {
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(bytes);
    let value = decode_spec_value_from_reader(ty, &mut reader, type_defs, "return value")?;
    if reader.remaining() != 0 {
        return Err(decode_value_error(
            "return value",
            format!(
                "{} unread byte(s) remain after decoding",
                reader.remaining()
            ),
        ));
    }
    Ok(value)
}

/// Decode one contract spec value from the current XDR reader position.
pub fn decode_spec_value_from_reader(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Value, Error> {
    decode_spec_value_from_reader_with_policy(
        ty,
        reader,
        type_defs,
        path,
        &mut DecodePolicy {
            strict_enums: false,
            remaining_values: None,
        },
    )
}

/// Decode one event value while rejecting enum discriminants absent from the spec.
pub fn decode_event_spec_value_from_reader(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    remaining_values: &mut usize,
) -> Result<Value, Error> {
    decode_spec_value_from_reader_with_policy(
        ty,
        reader,
        type_defs,
        path,
        &mut DecodePolicy {
            strict_enums: true,
            remaining_values: Some(remaining_values),
        },
    )
}

struct DecodePolicy<'a> {
    strict_enums: bool,
    remaining_values: Option<&'a mut usize>,
}

fn decode_spec_value_from_reader_with_policy(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    if let ContractType::Option { option_inner } = ty {
        return decode_option_value_from_reader(
            option_inner.as_ref(),
            reader,
            type_defs,
            path,
            policy,
        );
    }
    charge_decoded_values(policy, path, 1)?;
    if let Some(decoded) = decode_spec_scalar_value_from_reader(ty, reader, path) {
        return decoded;
    }

    match ty {
        ContractType::Vector { vector } => {
            decode_vector_value_from_reader(vector, reader, type_defs, path, policy)
        }
        ContractType::Tuple { tuple } => {
            ensure_decoded_values(policy, path, tuple.len())?;
            let mut values = Vec::with_capacity(tuple.len());
            for (index, item) in tuple.iter().enumerate() {
                values.push(decode_spec_value_from_reader_with_policy(
                    item,
                    reader,
                    type_defs,
                    &index_path(path, index),
                    policy,
                )?);
            }
            Ok(Value::Array(values))
        }
        ContractType::Named { named } => {
            decode_named_value_from_reader(named, reader, type_defs, path, policy)
        }
        ContractType::Array { array } => {
            decode_array_value_from_reader(array, reader, type_defs, path, policy)
        }
        _ => Err(Error::SpecFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

/// Normalize supported Rust syntax from locally parsed type names.
pub fn normalize_type_name(ty: &str) -> String {
    let mut ty = ty.trim();

    while let Some(stripped) = ty.strip_prefix('&') {
        ty = stripped.trim_start();
        if let Some(lifetime_rest) = ty.strip_prefix('\'') {
            ty = lifetime_rest
                .find(char::is_whitespace)
                .map_or(lifetime_rest, |idx| &lifetime_rest[idx..])
                .trim_start();
        }
        if let Some(stripped) = ty.strip_prefix("mut ") {
            ty = stripped.trim_start();
        }
    }

    match ty {
        "sdk::U256" | "groundhog_sdk::U256" => "U256".into(),
        "sdk::I256" | "groundhog_sdk::I256" => "I256".into(),
        _ => ty.to_owned(),
    }
}

fn encode_spec_typed_args(
    args: &[String],
    metadata: &SpecMethodMetadata,
) -> Result<Vec<u8>, Error> {
    if args.len() != metadata.method.args.len() {
        return Err(encode_value_error(
            "arguments",
            format!(
                "method expects {} argument(s) but {} were provided",
                metadata.method.args.len(),
                args.len()
            ),
        ));
    }
    if metadata.method.args.is_empty() {
        return Ok(vec![]);
    }

    let type_defs = build_type_def_index(&metadata.type_defs)?;
    let mut encoded = Vec::new();

    for (index, (arg, field)) in args.iter().zip(metadata.method.args.iter()).enumerate() {
        let field_name = spec_string_text(&field.name)?;
        let path = format!("argument {} (`{field_name}`)", index + 1);
        encoded.extend(encode_cli_spec_value(&field.ty, arg, &type_defs, &path)?);
    }

    Ok(encoded)
}

fn encode_local_typed_args(args: &[String], method: &MethodMetadata) -> Result<Vec<u8>, Error> {
    if args.len() != method.arg_types.len() {
        return Err(encode_value_error(
            "arguments",
            format!(
                "method expects {} argument(s) but {} were provided",
                method.arg_types.len(),
                args.len()
            ),
        ));
    }
    if method.arg_types.is_empty() {
        return Ok(vec![]);
    }

    let mut encoded = Vec::new();
    for (index, (arg, ty)) in args.iter().zip(&method.arg_types).enumerate() {
        let path = format!("argument {}", index + 1);
        encoded.extend(encode_single_value(ty, arg, &path)?);
    }
    Ok(encoded)
}

fn spec_type_def_name(type_def: &ContractTypeDef) -> Result<&str, Error> {
    match type_def {
        ContractTypeDef::TypeDefStruct { struct_def } => spec_string_text(&struct_def.name),
        ContractTypeDef::TypeDefEnum { enum_def } => spec_string_text(&enum_def.name),
        ContractTypeDef::TypeDefUnion { union_def } => spec_string_text(&union_def.name),
        _ => Err(Error::SpecFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

fn resolve_named_type_def<'a>(
    named: &ContractSpecString,
    type_defs: &'a TypeDefIndex<'a>,
) -> Result<&'a ContractTypeDef, Error> {
    let name = spec_string_text(named)?;
    type_defs
        .get(name)
        .copied()
        .ok_or_else(|| Error::SpecFailed(format!("contract spec is missing `{name}`")))
}

fn encode_cli_spec_value(
    ty: &ContractType,
    argument: &str,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    match ty {
        ContractType::Void => Err(encode_value_error(
            path,
            "void is not a valid contract argument type",
        )),
        ContractType::Option { option_inner } => {
            let mut encoded = encode_xdr_value(argument != "null")
                .map_err(|e| encode_value_error(path, e.to_string()))?;
            if argument != "null" {
                encoded.extend(encode_cli_spec_value(
                    option_inner.as_ref(),
                    argument,
                    type_defs,
                    path,
                )?);
            }
            Ok(encoded)
        }
        ContractType::Vector { vector } if vector_contains_u8(&vector.element) => {
            let bytes = parse_bytes_argument(argument, path)?;
            if bytes.len() > vector.max_len as usize {
                return Err(encode_value_error(
                    path,
                    format!(
                        "byte vector exceeds max length {} (got {})",
                        vector.max_len,
                        bytes.len()
                    ),
                ));
            }
            encode_variable_bytes(bytes, path)
        }
        ContractType::Array { array } if vector_contains_u8(&array.element) => {
            let bytes = parse_bytes_argument(argument, path)?;
            if bytes.len() != array.len as usize {
                return Err(encode_value_error(
                    path,
                    format!(
                        "byte array expects length {} but got {}",
                        array.len,
                        bytes.len()
                    ),
                ));
            }
            encode_fixed_bytes(&bytes, path)
        }
        ContractType::Tuple { .. } | ContractType::Vector { .. } | ContractType::Array { .. } => {
            let json = parse_json_argument(argument, path)?;
            encode_json_spec_value(ty, &json, type_defs, path)
        }
        ContractType::Named { named } => match resolve_named_type_def(named, type_defs)? {
            ContractTypeDef::TypeDefStruct { .. } => {
                let json = parse_json_argument(argument, path)?;
                encode_json_spec_value(ty, &json, type_defs, path)
            }
            ContractTypeDef::TypeDefEnum { enum_def } => {
                encode_enum_cli_value(enum_def, argument, path)
            }
            ContractTypeDef::TypeDefUnion { union_def } => {
                encode_union_cli_value(union_def, argument, type_defs, path)
            }
            _ => Err(Error::SpecFailed(
                "contract spec contains an unsupported type definition kind".into(),
            )),
        },
        _ => encode_scalar_cli_value(ty, argument, path),
    }
}

fn encode_json_spec_value(
    ty: &ContractType,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    if let Some(encoded) = encode_json_scalar_value(ty, value, path) {
        return encoded;
    }

    match ty {
        ContractType::Option { option_inner } => {
            encode_option_json_value(option_inner.as_ref(), value, type_defs, path)
        }
        ContractType::Vector { vector } => encode_vector_json_value(vector, value, type_defs, path),
        ContractType::Tuple { tuple } => encode_tuple_json_items(tuple, value, type_defs, path),
        ContractType::Named { named } => encode_named_json_value(named, value, type_defs, path),
        ContractType::Array { array } => encode_array_json_value(array, value, type_defs, path),
        _ => Err(Error::SpecFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

fn encode_json_scalar_value(
    ty: &ContractType,
    value: &Value,
    path: &str,
) -> Option<Result<Vec<u8>, Error>> {
    match ty {
        ContractType::Void => Some(match value {
            Value::Null => Ok(vec![]),
            _ => Err(encode_value_error(path, "expected null for a void value")),
        }),
        ContractType::Bool => Some(parse_json_bool(value, path).and_then(|value| {
            encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string()))
        })),
        ContractType::U8 => Some(
            parse_json_numeric::<u8>(value, "u8", path).and_then(|value| {
                encode_xdr_value(u32::from(value))
                    .map_err(|e| encode_value_error(path, e.to_string()))
            }),
        ),
        ContractType::U32 => Some(parse_json_numeric::<u32>(value, "u32", path).and_then(
            |value| encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string())),
        )),
        ContractType::I32 => Some(parse_json_numeric::<i32>(value, "i32", path).and_then(
            |value| encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string())),
        )),
        ContractType::U64 => Some(parse_json_numeric::<u64>(value, "u64", path).and_then(
            |value| encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string())),
        )),
        ContractType::I64 => Some(parse_json_numeric::<i64>(value, "i64", path).and_then(
            |value| encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string())),
        )),
        ContractType::U128 => Some(parse_json_numeric::<u128>(value, "u128", path).and_then(
            |value| encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string())),
        )),
        ContractType::I128 => Some(parse_json_numeric::<i128>(value, "i128", path).and_then(
            |value| encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string())),
        )),
        ContractType::U256 => Some(
            parse_json_numeric::<groundhog_u256::U256>(value, "U256", path).and_then(|value| {
                encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string()))
            }),
        ),
        ContractType::I256 => Some(
            parse_json_numeric::<groundhog_u256::I256>(value, "I256", path).and_then(|value| {
                encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string()))
            }),
        ),
        ContractType::String => Some(parse_json_string(value, path).and_then(|value| {
            encode_xdr_value(value.to_owned()).map_err(|e| encode_value_error(path, e.to_string()))
        })),
        ContractType::Bytes => Some(
            parse_bytes_json_value(value, path)
                .and_then(|bytes| encode_variable_bytes(bytes, path)),
        ),
        ContractType::Address => Some(parse_json_string(value, path).and_then(|value| {
            let address = parse_contract_address(value)?;
            encode_xdr_value(address).map_err(|e| encode_value_error(path, e.to_string()))
        })),
        _ => None,
    }
}

fn encode_option_json_value(
    inner: &ContractType,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let mut encoded =
        encode_xdr_value(!value.is_null()).map_err(|e| encode_value_error(path, e.to_string()))?;
    if !value.is_null() {
        encoded.extend(encode_json_spec_value(inner, value, type_defs, path)?);
    }
    Ok(encoded)
}

fn encode_vector_json_value(
    vector: &ContractVectorType,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let inner = vector.element.as_ref();
    if vector_contains_u8(inner) {
        let bytes = parse_bytes_json_value(value, path)?;
        if bytes.len() > vector.max_len as usize {
            return Err(encode_value_error(
                path,
                format!(
                    "byte vector exceeds max length {} (got {})",
                    vector.max_len,
                    bytes.len()
                ),
            ));
        }
        return encode_variable_bytes(bytes, path);
    }

    let items = parse_json_array(value, path)?;
    if items.len() > vector.max_len as usize {
        return Err(encode_value_error(
            path,
            format!(
                "vector exceeds max length {} (got {})",
                vector.max_len,
                items.len()
            ),
        ));
    }

    let mut encoded =
        encode_xdr_value(u32::try_from(items.len()).map_err(|_| {
            encode_value_error(path, "vector length exceeds the supported u32 range")
        })?)
        .map_err(|e| encode_value_error(path, e.to_string()))?;
    for (index, item) in items.iter().enumerate() {
        encoded.extend(encode_json_spec_value(
            inner,
            item,
            type_defs,
            &index_path(path, index),
        )?);
    }
    Ok(encoded)
}

fn encode_named_json_value(
    named: &ContractSpecString,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    match resolve_named_type_def(named, type_defs)? {
        ContractTypeDef::TypeDefStruct { struct_def } => {
            encode_struct_json_value(struct_def, value, type_defs, path)
        }
        ContractTypeDef::TypeDefEnum { enum_def } => encode_enum_json_value(enum_def, value, path),
        ContractTypeDef::TypeDefUnion { union_def } => {
            encode_union_json_value(union_def, value, type_defs, path)
        }
        _ => Err(Error::SpecFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

fn encode_array_json_value(
    array: &ContractArrayType,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let inner = array.element.as_ref();
    if vector_contains_u8(inner) {
        let bytes = parse_bytes_json_value(value, path)?;
        if bytes.len() != array.len as usize {
            return Err(encode_value_error(
                path,
                format!(
                    "byte array expects length {} but got {}",
                    array.len,
                    bytes.len()
                ),
            ));
        }
        return encode_fixed_bytes(&bytes, path);
    }

    let items = parse_json_array(value, path)?;
    if items.len() != array.len as usize {
        return Err(encode_value_error(
            path,
            format!("array expects length {} but got {}", array.len, items.len()),
        ));
    }

    let mut encoded = Vec::new();
    for (index, item) in items.iter().enumerate() {
        encoded.extend(encode_json_spec_value(
            inner,
            item,
            type_defs,
            &index_path(path, index),
        )?);
    }
    Ok(encoded)
}

fn encode_struct_json_value(
    struct_def: &ContractStructDef,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    encode_field_list(struct_def.fields.iter(), value, type_defs, path)
}

fn encode_field_list<'a>(
    fields: impl ExactSizeIterator<Item = &'a ContractField>,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let fields = fields.collect::<Vec<_>>();
    match value {
        Value::Array(values) => {
            if values.len() != fields.len() {
                return Err(encode_value_error(
                    path,
                    format!(
                        "expected {} field value(s) but got {}",
                        fields.len(),
                        values.len()
                    ),
                ));
            }

            let mut encoded = Vec::new();
            for (index, (field, value)) in fields.iter().zip(values).enumerate() {
                encoded.extend(encode_json_spec_value(
                    &field.ty,
                    value,
                    type_defs,
                    &index_path(path, index),
                )?);
            }
            Ok(encoded)
        }
        Value::Object(object) => {
            let expected_names = fields
                .iter()
                .map(|field| spec_string_text(&field.name))
                .collect::<Result<Vec<_>, _>>()?;
            for key in object.keys() {
                if !expected_names.iter().any(|expected| expected == key) {
                    return Err(encode_value_error(
                        path,
                        format!("unexpected field `{key}`"),
                    ));
                }
            }

            let mut encoded = Vec::new();
            for field in fields {
                let field_name = spec_string_text(&field.name)?;
                let field_value = object.get(field_name).ok_or_else(|| {
                    encode_value_error(path, format!("missing field `{field_name}`"))
                })?;
                encoded.extend(encode_json_spec_value(
                    &field.ty,
                    field_value,
                    type_defs,
                    &field_path(path, field_name),
                )?);
            }
            Ok(encoded)
        }
        _ => Err(encode_value_error(path, "expected a JSON object or array")),
    }
}

fn encode_tuple_json_items(
    tuple: &[ContractType],
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let items = parse_json_array(value, path)?;
    if items.len() != tuple.len() {
        return Err(encode_value_error(
            path,
            format!(
                "expected {} tuple item(s) but got {}",
                tuple.len(),
                items.len()
            ),
        ));
    }

    let mut encoded = Vec::new();
    for (index, (ty, item)) in tuple.iter().zip(items).enumerate() {
        encoded.extend(encode_json_spec_value(
            ty,
            item,
            type_defs,
            &index_path(path, index),
        )?);
    }
    Ok(encoded)
}

fn encode_enum_cli_value(
    enum_def: &ContractEnumDef,
    argument: &str,
    path: &str,
) -> Result<Vec<u8>, Error> {
    if let Ok(json) = serde_json::from_str::<Value>(argument)
        && matches!(json, Value::String(_) | Value::Number(_))
    {
        return encode_enum_json_value(enum_def, &json, path);
    }
    encode_enum_json_value(enum_def, &Value::String(argument.to_owned()), path)
}

fn encode_enum_json_value(
    enum_def: &ContractEnumDef,
    value: &Value,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let discriminant = match value {
        Value::String(name) => {
            if let Some(discriminant) = enum_def.variants.iter().find_map(|variant| {
                let variant_name = spec_string_text(&variant.name).ok()?;
                (variant_name == name).then_some(variant.value)
            }) {
                discriminant
            } else {
                name.parse::<i32>().map_err(|_| {
                    encode_value_error(path, format!("unknown enum variant `{name}`"))
                })?
            }
        }
        Value::Number(number) => number
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| encode_value_error(path, "enum discriminants must fit in i32"))?,
        _ => {
            return Err(encode_value_error(
                path,
                "expected an enum variant name or discriminant",
            ));
        }
    };

    encode_xdr_value(discriminant).map_err(|e| encode_value_error(path, e.to_string()))
}

fn encode_union_cli_value(
    union_def: &ContractUnionDef,
    argument: &str,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    if let Ok(json) = serde_json::from_str::<Value>(argument) {
        return encode_union_json_value(union_def, &json, type_defs, path);
    }

    let case = resolve_union_case(union_def, &Value::String(argument.to_owned()), path)?;
    encode_union_case(union_def, case, None, type_defs, path)
}

fn encode_union_json_value(
    union_def: &ContractUnionDef,
    value: &Value,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    match value {
        Value::Object(object) => {
            let case_value = object
                .get("case")
                .or_else(|| object.get("discriminant"))
                .ok_or_else(|| {
                    encode_value_error(
                        path,
                        "union values must include a `case` (or `discriminant`) field",
                    )
                })?;
            let case = resolve_union_case(union_def, case_value, &field_path(path, "case"))?;
            encode_union_case(union_def, case, object.get("value"), type_defs, path)
        }
        Value::String(_) | Value::Number(_) => {
            let case = resolve_union_case(union_def, value, path)?;
            encode_union_case(union_def, case, None, type_defs, path)
        }
        _ => Err(encode_value_error(
            path,
            "expected a union case name/discriminant or an object with `case` and optional `value`",
        )),
    }
}

fn resolve_union_case<'a>(
    union_def: &'a ContractUnionDef,
    value: &Value,
    path: &str,
) -> Result<&'a ContractUnionCase, Error> {
    match value {
        Value::String(name) => {
            if let Some(case) = union_def
                .cases
                .iter()
                .find(|case| spec_string_text(&case.name).ok() == Some(name.as_str()))
            {
                Ok(case)
            } else if let Ok(discriminant) = name.parse::<i32>() {
                union_def
                    .cases
                    .iter()
                    .find(|case| case.discriminant == discriminant)
                    .ok_or_else(|| {
                        encode_value_error(
                            path,
                            format!("unknown union discriminant `{discriminant}`"),
                        )
                    })
            } else {
                Err(encode_value_error(
                    path,
                    format!("unknown union case `{name}`"),
                ))
            }
        }
        Value::Number(number) => {
            let discriminant = number
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| encode_value_error(path, "union discriminants must fit in i32"))?;
            union_def
                .cases
                .iter()
                .find(|case| case.discriminant == discriminant)
                .ok_or_else(|| {
                    encode_value_error(path, format!("unknown union discriminant `{discriminant}`"))
                })
        }
        _ => Err(encode_value_error(
            path,
            "expected a union case name or discriminant",
        )),
    }
}

fn encode_union_case(
    union_def: &ContractUnionDef,
    case: &ContractUnionCase,
    payload: Option<&Value>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let mut encoded =
        encode_xdr_value(case.discriminant).map_err(|e| encode_value_error(path, e.to_string()))?;
    match &case.case_type {
        ContractUnionCaseType::UnionCaseVoid => {
            if let Some(value) = payload
                && !value.is_null()
            {
                return Err(encode_value_error(
                    path,
                    format!(
                        "union case `{}` does not take a payload",
                        spec_string_text(&case.name)?
                    ),
                ));
            }
        }
        ContractUnionCaseType::UnionCaseTuple { tuple } => {
            let case_name = spec_string_text(&case.name)?;
            let payload = payload.ok_or_else(|| {
                encode_value_error(
                    path,
                    format!("union case `{case_name}` requires a `value` payload"),
                )
            })?;
            encoded.extend(encode_tuple_json_items(
                tuple,
                payload,
                type_defs,
                &field_path(path, "value"),
            )?);
        }
        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
            let case_name = spec_string_text(&case.name)?;
            let payload = payload.ok_or_else(|| {
                encode_value_error(
                    path,
                    format!("union case `{case_name}` requires a `value` payload"),
                )
            })?;
            encoded.extend(encode_field_list(
                struct_fields.iter(),
                payload,
                type_defs,
                &field_path(path, "value"),
            )?);
        }
        _ => {
            return Err(Error::SpecFailed(format!(
                "contract spec contains an unsupported union case in `{}`",
                spec_string_text(&union_def.name)?
            )));
        }
    }

    Ok(encoded)
}

fn encode_single_value(ty: &str, value: &str, path: &str) -> Result<Vec<u8>, Error> {
    let ty = normalize_type_name(ty);
    match ty.as_str() {
        "u16" => encode_local_xdr_value::<u16>(parse_numeric(value, &ty)?, path),
        "u32" => encode_local_xdr_value::<u32>(parse_numeric(value, &ty)?, path),
        "u64" => encode_local_xdr_value::<u64>(parse_numeric(value, &ty)?, path),
        "u128" => encode_local_xdr_value::<u128>(parse_numeric(value, &ty)?, path),
        "U256" => encode_local_xdr_value::<groundhog_u256::U256>(parse_numeric(value, &ty)?, path),
        "i16" => encode_local_xdr_value::<i16>(parse_numeric(value, &ty)?, path),
        "i32" => encode_local_xdr_value::<i32>(parse_numeric(value, &ty)?, path),
        "i64" => encode_local_xdr_value::<i64>(parse_numeric(value, &ty)?, path),
        "i128" => encode_local_xdr_value::<i128>(parse_numeric(value, &ty)?, path),
        "I256" => encode_local_xdr_value::<groundhog_u256::I256>(parse_numeric(value, &ty)?, path),
        "bool" => {
            let parsed = value
                .parse::<bool>()
                .map_err(|e| encode_value_error(path, format!("invalid bool `{value}`: {e}")))?;
            encode_local_xdr_value(parsed, path)
        }
        "String" | "str" => encode_local_xdr_value(value.to_string(), path),
        "ContractAddress" => encode_local_xdr_value(parse_contract_address(value)?, path),
        "Ed25519PublicKey" => encode_local_xdr_value(parse_public_key(value)?, path),
        "SHA256Hash" => encode_local_xdr_value(
            SHA256Hash::from(parse_fixed_hex::<32>(value, "sha256 hash")?),
            path,
        ),
        "ContractEntrypointID" => encode_local_xdr_value(
            ContractEntrypointID::from(parse_fixed_hex::<8>(value, "contract entrypoint")?),
            path,
        ),
        _ => Err(encode_value_error(
            path,
            format!(
                "typed call encoding for `{ty}` is not implemented yet; pass an explicit \
                 0x-prefixed entrypoint ID and raw calldata instead"
            ),
        )),
    }
}

fn parse_numeric<T>(value: &str, ty: &str) -> Result<T, Error>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value.parse::<T>().map_err(|e| Error::InvalidArgument {
        argument: format!("{ty} argument"),
        message: format!("`{value}` is not a valid {ty}: {e}"),
    })
}

fn encode_xdr_value<T>(value: T) -> Result<Vec<u8>, Error>
where
    T: xdrs::WriteXDR,
{
    xdrs::cxs::encode(&value).map_err(|e| Error::Other(format!("failed to encode calldata: {e}")))
}

fn encode_local_xdr_value<T>(value: T, path: &str) -> Result<Vec<u8>, Error>
where
    T: xdrs::WriteXDR,
{
    encode_xdr_value(value).map_err(|e| encode_value_error(path, e.to_string()))
}

fn encode_variable_bytes(bytes: Vec<u8>, path: &str) -> Result<Vec<u8>, Error> {
    encode_xdr_value(bytes).map_err(|e| encode_value_error(path, e.to_string()))
}

fn encode_fixed_bytes(bytes: &[u8], path: &str) -> Result<Vec<u8>, Error> {
    let mut writer = xdrs::cxs::buf_writer::BufWriter::with_capacity(bytes.len());
    writer
        .write_opaque_fixed(bytes)
        .map_err(|e| encode_value_error(path, format!("failed to encode fixed bytes: {e}")))?;
    Ok(writer.into())
}

fn parse_json_argument(argument: &str, path: &str) -> Result<Value, Error> {
    serde_json::from_str(argument)
        .map_err(|e| encode_value_error(path, format!("must be valid JSON: {e}")))
}

fn parse_json_array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>, Error> {
    value
        .as_array()
        .ok_or_else(|| encode_value_error(path, "expected a JSON array"))
}

fn parse_json_string<'a>(value: &'a Value, path: &str) -> Result<&'a str, Error> {
    value
        .as_str()
        .ok_or_else(|| encode_value_error(path, "expected a JSON string"))
}

fn parse_json_numeric<T>(value: &Value, ty: &str, path: &str) -> Result<T, Error>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match value {
        Value::Number(number) => parse_numeric(&number.to_string(), ty),
        Value::String(string) => parse_numeric(string, ty),
        _ => Err(encode_value_error(path, format!("expected a {ty}"))),
    }
}

fn parse_json_bool(value: &Value, path: &str) -> Result<bool, Error> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::String(value) => value
            .parse::<bool>()
            .map_err(|e| encode_value_error(path, format!("invalid bool `{value}`: {e}"))),
        _ => Err(encode_value_error(path, "expected a bool")),
    }
}

fn encode_scalar_cli_value(ty: &ContractType, value: &str, path: &str) -> Result<Vec<u8>, Error> {
    match ty {
        ContractType::Bool => encode_xdr_value(
            value
                .parse::<bool>()
                .map_err(|e| encode_value_error(path, format!("invalid bool `{value}`: {e}")))?,
        )
        .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U8 => encode_xdr_value(u32::from(parse_numeric::<u8>(value, "u8")?))
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U32 => encode_xdr_value(parse_numeric::<u32>(value, "u32")?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I32 => encode_xdr_value(parse_numeric::<i32>(value, "i32")?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U64 => encode_xdr_value(parse_numeric::<u64>(value, "u64")?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I64 => encode_xdr_value(parse_numeric::<i64>(value, "i64")?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U128 => encode_xdr_value(parse_numeric::<u128>(value, "u128")?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::I128 => encode_xdr_value(parse_numeric::<i128>(value, "i128")?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        ContractType::U256 => {
            encode_xdr_value(parse_numeric::<groundhog_u256::U256>(value, "U256")?)
                .map_err(|e| encode_value_error(path, e.to_string()))
        }
        ContractType::I256 => {
            encode_xdr_value(parse_numeric::<groundhog_u256::I256>(value, "I256")?)
                .map_err(|e| encode_value_error(path, e.to_string()))
        }
        ContractType::String => {
            encode_xdr_value(value.to_owned()).map_err(|e| encode_value_error(path, e.to_string()))
        }
        ContractType::Bytes => encode_variable_bytes(parse_bytes_argument(value, path)?, path),
        ContractType::Address => encode_xdr_value(parse_contract_address(value)?)
            .map_err(|e| encode_value_error(path, e.to_string())),
        _ => Err(encode_value_error(
            path,
            "this contract argument type requires structured JSON input",
        )),
    }
}

fn parse_bytes_argument(argument: &str, path: &str) -> Result<Vec<u8>, Error> {
    if let Ok(json) = serde_json::from_str::<Value>(argument)
        && matches!(json, Value::String(_) | Value::Array(_))
    {
        return parse_bytes_json_value(&json, path);
    }
    parse_byte_string(argument, path)
}

fn parse_bytes_json_value(value: &Value, path: &str) -> Result<Vec<u8>, Error> {
    match value {
        Value::String(value) => parse_byte_string(value, path),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| parse_json_numeric::<u8>(value, "u8", &index_path(path, index)))
            .collect(),
        _ => Err(encode_value_error(
            path,
            "expected a hex string, UTF-8 string, or JSON array of byte values",
        )),
    }
}

fn parse_byte_string(value: &str, path: &str) -> Result<Vec<u8>, Error> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return hex::decode(hex)
            .map_err(|e| encode_value_error(path, format!("invalid byte hex `{value}`: {e}")));
    }
    Ok(value.as_bytes().to_vec())
}

fn vector_contains_u8(element: &ContractType) -> bool {
    matches!(element, ContractType::U8)
}

fn encode_value_error(path: &str, message: impl Into<String>) -> Error {
    Error::CallEncode {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn decode_value_error(path: &str, message: impl Into<String>) -> Error {
    Error::CallDecode {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn ensure_decoded_values(
    policy: &mut DecodePolicy<'_>,
    path: &str,
    required: usize,
) -> Result<(), Error> {
    if policy
        .remaining_values
        .as_deref()
        .is_some_and(|remaining| required > *remaining)
    {
        return Err(decode_value_error(
            path,
            "decoded value exceeds the per-event value limit",
        ));
    }
    Ok(())
}

fn charge_decoded_values(
    policy: &mut DecodePolicy<'_>,
    path: &str,
    count: usize,
) -> Result<(), Error> {
    ensure_decoded_values(policy, path, count)?;
    if let Some(remaining) = policy.remaining_values.as_deref_mut() {
        *remaining -= count;
    }
    Ok(())
}

fn field_path(path: &str, field: &str) -> String {
    format!("{path}.{field}")
}

fn index_path(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn decode_spec_scalar_value_from_reader(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    path: &str,
) -> Option<Result<Value, Error>> {
    match ty {
        ContractType::Void => Some(Ok(Value::Null)),
        ContractType::Bool => Some(read_xdr_value::<bool>(reader, path).map(Value::from)),
        ContractType::U8 => Some(read_xdr_value::<u32>(reader, path).and_then(|value| {
            let value: u8 = value
                .try_into()
                .map_err(|_| decode_value_error(path, "decoded u8 value is out of range"))?;
            Ok(Value::from(value))
        })),
        ContractType::U32 => Some(read_xdr_value::<u32>(reader, path).map(Value::from)),
        ContractType::I32 => Some(read_xdr_value::<i32>(reader, path).map(Value::from)),
        ContractType::U64 => Some(read_xdr_value::<u64>(reader, path).map(Value::from)),
        ContractType::I64 => Some(read_xdr_value::<i64>(reader, path).map(Value::from)),
        ContractType::U128 => {
            Some(read_xdr_value::<u128>(reader, path).map(|value| Value::from(value.to_string())))
        }
        ContractType::I128 => {
            Some(read_xdr_value::<i128>(reader, path).map(|value| Value::from(value.to_string())))
        }
        ContractType::U256 => Some(
            read_xdr_value::<groundhog_u256::U256>(reader, path)
                .map(|value| Value::from(value.to_string())),
        ),
        ContractType::I256 => Some(
            read_xdr_value::<groundhog_u256::I256>(reader, path)
                .map(|value| Value::from(value.to_string())),
        ),
        ContractType::String => Some(read_xdr_value::<String>(reader, path).map(Value::from)),
        ContractType::Bytes => Some(
            reader
                .read_opaque_variable_owned(u32::MAX)
                .map(hex_json_value)
                .map_err(|e| decode_value_error(path, format!("failed to decode bytes: {e}"))),
        ),
        ContractType::Address => {
            Some(read_xdr_value::<ContractAddress>(reader, path).map(hex_json_value))
        }
        _ => None,
    }
}

fn decode_option_value_from_reader(
    inner: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    if read_xdr_value::<bool>(reader, path)? {
        decode_spec_value_from_reader_with_policy(inner, reader, type_defs, path, policy)
    } else {
        charge_decoded_values(policy, path, 1)?;
        Ok(Value::Null)
    }
}

fn decode_vector_value_from_reader(
    vector: &ContractVectorType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    let inner = vector.element.as_ref();
    if vector_contains_u8(inner) {
        return reader
            .read_opaque_variable_owned(vector.max_len)
            .map(hex_json_value)
            .map_err(|e| decode_value_error(path, format!("failed to decode byte vector: {e}")));
    }

    let len = read_xdr_value::<u32>(reader, path)? as usize;
    if len > vector.max_len as usize {
        return Err(decode_value_error(
            path,
            format!("decoded vector length {len} exceeds max {}", vector.max_len),
        ));
    }
    ensure_decoded_values(policy, path, len)?;
    let mut values = Vec::with_capacity(len);
    for index in 0..len {
        values.push(decode_spec_value_from_reader_with_policy(
            inner,
            reader,
            type_defs,
            &index_path(path, index),
            policy,
        )?);
    }
    Ok(Value::Array(values))
}

fn decode_named_value_from_reader(
    named: &ContractSpecString,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    match resolve_named_type_def(named, type_defs)? {
        ContractTypeDef::TypeDefStruct { struct_def } => {
            decode_struct_value(struct_def, reader, type_defs, path, policy)
        }
        ContractTypeDef::TypeDefEnum { enum_def } => {
            decode_enum_value(enum_def, reader, path, policy.strict_enums)
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            decode_union_value(union_def, reader, type_defs, path, policy)
        }
        _ => Err(Error::SpecFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

fn decode_array_value_from_reader(
    array: &ContractArrayType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    let inner = array.element.as_ref();
    if vector_contains_u8(inner) {
        return reader
            .read_opaque_fixed_owned(array.len)
            .map(hex_json_value)
            .map_err(|e| decode_value_error(path, format!("failed to decode byte array: {e}")));
    }

    ensure_decoded_values(policy, path, array.len as usize)?;
    let mut values = Vec::with_capacity(array.len as usize);
    for index in 0..array.len as usize {
        values.push(decode_spec_value_from_reader_with_policy(
            inner,
            reader,
            type_defs,
            &index_path(path, index),
            policy,
        )?);
    }
    Ok(Value::Array(values))
}

fn decode_struct_value(
    struct_def: &ContractStructDef,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    decode_field_values(struct_def.fields.iter(), reader, type_defs, path, policy)
}

fn decode_field_values<'a>(
    fields: impl ExactSizeIterator<Item = &'a ContractField>,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    let fields = fields.collect::<Vec<_>>();
    ensure_decoded_values(policy, path, fields.len())?;
    if fields_decode_to_object(&fields)? {
        let mut object = serde_json::Map::with_capacity(fields.len());
        for field in fields {
            let field_name = spec_string_text(&field.name)?;
            let value = decode_spec_value_from_reader_with_policy(
                &field.ty,
                reader,
                type_defs,
                &field_path(path, field_name),
                policy,
            )?;
            object.insert(field_name.to_owned(), value);
        }
        Ok(Value::Object(object))
    } else {
        let mut values = Vec::with_capacity(fields.len());
        for (index, field) in fields.iter().enumerate() {
            values.push(decode_spec_value_from_reader_with_policy(
                &field.ty,
                reader,
                type_defs,
                &index_path(path, index),
                policy,
            )?);
        }
        Ok(Value::Array(values))
    }
}

fn fields_decode_to_object(fields: &[&ContractField]) -> Result<bool, Error> {
    for field in fields {
        let name = spec_string_text(&field.name)?;
        if name.is_empty() || name.parse::<usize>().is_ok() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn decode_enum_value(
    enum_def: &ContractEnumDef,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    path: &str,
    strict: bool,
) -> Result<Value, Error> {
    let discriminant = read_xdr_value::<i32>(reader, path)?;
    for variant in enum_def.variants.iter() {
        if variant.value == discriminant {
            return Ok(Value::from(spec_string_text(&variant.name)?.to_owned()));
        }
    }
    if strict {
        Err(decode_value_error(
            path,
            format!("unknown enum discriminant `{discriminant}`"),
        ))
    } else {
        Ok(Value::from(discriminant))
    }
}

fn decode_union_value(
    union_def: &ContractUnionDef,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    policy: &mut DecodePolicy<'_>,
) -> Result<Value, Error> {
    let discriminant = read_xdr_value::<i32>(reader, path)?;
    let case = union_def
        .cases
        .iter()
        .find(|case| case.discriminant == discriminant)
        .ok_or_else(|| {
            decode_value_error(path, format!("unknown union discriminant `{discriminant}`"))
        })?;

    let mut object = serde_json::Map::new();
    object.insert(
        "case".into(),
        Value::from(spec_string_text(&case.name)?.to_owned()),
    );
    charge_decoded_values(policy, path, 1)?;

    match &case.case_type {
        ContractUnionCaseType::UnionCaseVoid => {}
        ContractUnionCaseType::UnionCaseTuple { tuple } => {
            object.insert(
                "value".into(),
                decode_spec_value_from_reader_with_policy(
                    &ContractType::Tuple {
                        tuple: tuple.clone(),
                    },
                    reader,
                    type_defs,
                    &field_path(path, "value"),
                    policy,
                )?,
            );
        }
        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
            charge_decoded_values(policy, &field_path(path, "value"), 1)?;
            object.insert(
                "value".into(),
                decode_field_values(
                    struct_fields.iter(),
                    reader,
                    type_defs,
                    &field_path(path, "value"),
                    policy,
                )?,
            );
        }
        _ => {
            return Err(Error::SpecFailed(format!(
                "contract spec contains an unsupported union in `{}`",
                spec_string_text(&union_def.name)?
            )));
        }
    }

    Ok(Value::Object(object))
}

fn decode_single_value(ty: &str, bytes: &[u8]) -> Option<Value> {
    let ty = normalize_type_name(ty);
    match ty.as_str() {
        "u16" => decode_xdr_value::<u16>(bytes).map(Value::from),
        "u32" => decode_xdr_value::<u32>(bytes).map(Value::from),
        "u64" => decode_xdr_value::<u64>(bytes).map(Value::from),
        "u128" => decode_xdr_value::<u128>(bytes).map(|value| Value::from(value.to_string())),
        "U256" => decode_xdr_value::<groundhog_u256::U256>(bytes)
            .map(|value| Value::from(value.to_string())),
        "i16" => decode_xdr_value::<i16>(bytes).map(Value::from),
        "i32" => decode_xdr_value::<i32>(bytes).map(Value::from),
        "i64" => decode_xdr_value::<i64>(bytes).map(Value::from),
        "i128" => decode_xdr_value::<i128>(bytes).map(|value| Value::from(value.to_string())),
        "I256" => decode_xdr_value::<groundhog_u256::I256>(bytes)
            .map(|value| Value::from(value.to_string())),
        "bool" => decode_xdr_value::<bool>(bytes).map(Value::from),
        "String" | "str" => decode_xdr_value::<String>(bytes).map(Value::from),
        "ContractAddress" => decode_xdr_value::<ContractAddress>(bytes).map(hex_json_value),
        "Ed25519PublicKey" => decode_xdr_value::<Ed25519PublicKey>(bytes).map(hex_json_value),
        "SHA256Hash" => decode_xdr_value::<SHA256Hash>(bytes).map(hex_json_value),
        "ContractEntrypointID" => {
            decode_xdr_value::<ContractEntrypointID>(bytes).map(hex_json_value)
        }
        _ => None,
    }
}

fn decode_xdr_value<T>(bytes: &[u8]) -> Option<T>
where
    T: xdrs::ReadXDROwned,
{
    xdrs::cxs::decode(bytes).ok()
}

fn read_xdr_value<T>(
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    path: &str,
) -> Result<T, Error>
where
    T: xdrs::ReadXDROwned,
{
    xdrs::read_xdr(reader)
        .map_err(|e| decode_value_error(path, format!("failed to decode value: {e}")))
}

fn parse_fixed_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N], Error> {
    decode_fixed_hex(value).map_err(|error| Error::InvalidArgument {
        argument: label.to_string(),
        message: match error {
            FixedHexError::InvalidHex(error) => format!("`{value}` is not valid hex: {error}"),
            FixedHexError::InvalidLength { len, .. } => format!("must be {N} bytes, got {len}"),
        },
    })
}

fn hex_json_value<T>(value: T) -> Value
where
    T: AsRef<[u8]>,
{
    Value::from(prefixed_hex(value))
}
