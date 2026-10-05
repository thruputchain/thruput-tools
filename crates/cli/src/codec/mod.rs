//! CLI compatibility adapters for the shared contract codec.

use crate::CliError;
use groundhog_contract_spec::ContractType;
use groundhog_contract_spec::{ContractSpecString, ContractTypeDef};
use groundhog_xdr::types::ContractAddress;
use serde_json::Value;

pub(crate) use groundhog_client::codec::{
    AbortDecodeError, CallMetadata, ContractMetadata, DecodedAbort, MethodMetadata,
    SpecMethodMetadata, TypeDefIndex, decode_return, format_contract_address, is_erc20_too_large,
};

#[cfg(test)]
pub(crate) use groundhog_client::codec::normalize_type_name;

pub(crate) fn encode_args(
    args: &[String],
    metadata: Option<&CallMetadata>,
) -> Result<Vec<u8>, CliError> {
    if metadata.is_none() && args.len() > 1 {
        return Err(CliError::CallEncode {
            path: "arguments".into(),
            message: "multiple call arguments require local contract metadata — run `groundhog call` from \
         inside the project that defines the contract, or pass a single hex-encoded calldata \
         argument".into(),
        });
    }
    groundhog_client::codec::encode_args(args, metadata).map_err(Into::into)
}

pub(crate) fn parse_contract_address(hex_str: &str) -> Result<ContractAddress, CliError> {
    groundhog_client::codec::parse_contract_address(hex_str).map_err(Into::into)
}

pub(crate) fn spec_string_text(value: &ContractSpecString) -> Result<&str, CliError> {
    groundhog_client::codec::spec_string_text(value).map_err(Into::into)
}

pub(crate) fn build_type_def_index(
    type_defs: &[ContractTypeDef],
) -> Result<TypeDefIndex<'_>, CliError> {
    groundhog_client::codec::build_type_def_index(type_defs).map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn decode_spec_value(
    ty: &ContractType,
    bytes: &[u8],
    type_defs: &TypeDefIndex<'_>,
) -> Result<Value, CliError> {
    groundhog_client::codec::decode_spec_value(ty, bytes, type_defs).map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn decode_spec_value_from_reader(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
) -> Result<Value, CliError> {
    groundhog_client::codec::decode_spec_value_from_reader(ty, reader, type_defs, path)
        .map_err(Into::into)
}

pub(crate) fn decode_event_spec_value_from_reader(
    ty: &ContractType,
    reader: &mut xdrs::cxs::buf_reader::BufReader<'_>,
    type_defs: &TypeDefIndex<'_>,
    path: &str,
    remaining_values: &mut usize,
) -> Result<Value, CliError> {
    groundhog_client::codec::decode_event_spec_value_from_reader(
        ty,
        reader,
        type_defs,
        path,
        remaining_values,
    )
    .map_err(Into::into)
}

/// Decode declared exceptions first, then the CLI's standard SDK exception registry.
pub(crate) fn decode_abort(
    bytes: &[u8],
    metadata: Option<&CallMetadata>,
) -> Result<DecodedAbort, AbortDecodeError> {
    match groundhog_client::codec::decode_abort(bytes, metadata) {
        Err(AbortDecodeError::Unknown { selector, payload }) => {
            if let Some(ambient) = groundhog_spec_tool::describe::lookup_by_selector(selector) {
                let described = groundhog_spec_tool::describe::build_describe(ambient);
                return groundhog_client::codec::decode_abort_payload(
                    &described.exception.ty,
                    &described.type_defs,
                    selector,
                    &payload,
                )
                .map(|mut decoded| {
                    // Both come off the entry's own id now; describe used to
                    // repeat them beside it because a joined name could not be
                    // taken apart. The segments are already 'static.
                    decoded.exception_crate = Some(ambient.root.crate_name);
                    decoded.exception_version = Some(ambient.root.major);
                    decoded
                });
            }
            Err(AbortDecodeError::Unknown { selector, payload })
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_exception_wins_over_ambient_selector_collision() {
        let selector = <groundhog_erc20_if::ERC20Error as sdk::exn::Exn>::SELECTOR;
        let metadata = CallMetadata::Spec(SpecMethodMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            method: groundhog_contract_spec::ContractSpecMethod {
                effect: groundhog_contract_spec::ContractMethodEffect::EffectUnknown,
                name: "increment".try_into().unwrap(),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::Void,
                effect: None,
            },
            type_defs: vec![],
            throws: vec![groundhog_contract_spec::ContractExceptionType {
                selector,
                ty: ContractType::U64,
            }],
        });
        let bytes = xdrs::cxs::encode(&(selector, 7u64)).unwrap();
        let decoded = decode_abort(&bytes, Some(&metadata)).unwrap();
        assert_eq!(decoded.value, serde_json::json!("7"));
        assert_eq!(decoded.exception_crate, None);
        // An invalid declared payload must not fall back to the ambient registry.
        let malformed = xdrs::cxs::encode(&selector).unwrap();
        assert!(matches!(
            decode_abort(&malformed, Some(&metadata)),
            Err(AbortDecodeError::BadDecode { .. })
        ));
    }

    #[test]
    fn decode_abort_decodes_library_exception_without_metadata() {
        let library = groundhog_spec_tool::describe::lookup("LibraryExn")
            .expect("bug: LibraryExn should be registered");
        let described = groundhog_spec_tool::describe::build_describe(library);
        let mut bytes = xdrs::cxs::encode(&described.exception.selector).unwrap();
        // LibraryExn is a void union case; use its registered discriminant.
        let groundhog_contract_spec::ContractTypeDef::TypeDefUnion { union_def } =
            &described.type_defs[0]
        else {
            panic!("expected LibraryExn union")
        };
        let case = union_def
            .cases
            .iter()
            .find(|case| spec_string_text(&case.name).unwrap() == "NonNegativeIntUnderflow")
            .unwrap();
        bytes.extend(xdrs::cxs::encode(&case.discriminant).unwrap());

        let decoded = decode_abort(&bytes, None).expect("bug: library exception should decode");

        assert_eq!(decoded.exception.as_deref(), Some("LibraryExn"));
        assert_eq!(
            decoded.value,
            serde_json::json!({"case": "NonNegativeIntUnderflow"})
        );
        assert_eq!(decoded.exception_crate, Some("sdk"));
        assert_eq!(decoded.exception_version, Some("1"));
    }

    #[test]
    fn decode_abort_decodes_erc20_error_without_metadata() {
        let bytes = xdrs::cxs::encode(&(
            <groundhog_erc20_if::ERC20Error as sdk::exn::Exn>::SELECTOR,
            groundhog_erc20_if::ERC20Error::TooLarge,
        ))
        .expect("bug: ERC20 exception should encode");

        let decoded = decode_abort(&bytes, None).expect("bug: ERC20 exception should decode");

        assert_eq!(decoded.exception.as_deref(), Some("ERC20Error"));
        assert_eq!(decoded.value, serde_json::json!("TooLarge"));
        assert_eq!(decoded.exception_crate, Some("groundhog-erc20-if"));
        assert_eq!(decoded.exception_version, Some("0.1"));
    }

    #[test]
    fn decode_abort_decodes_erc20_permit_error_without_metadata() {
        let bytes = xdrs::cxs::encode(&(
            <groundhog_erc20_permit_if::ERC20PermitError as sdk::exn::Exn>::SELECTOR,
            groundhog_erc20_permit_if::ERC20PermitError::NotSpender,
        ))
        .expect("bug: ERC20 permit exception should encode");

        let decoded =
            decode_abort(&bytes, None).expect("bug: ERC20 permit exception should decode");

        assert_eq!(decoded.exception.as_deref(), Some("ERC20PermitError"));
        assert_eq!(decoded.value, serde_json::json!("NotSpender"));
        assert_eq!(decoded.exception_crate, Some("groundhog-erc20-permit-if"));
        assert_eq!(decoded.exception_version, Some("0.1"));
    }

    #[test]
    fn describe_registry_covers_wallet_ambient_exceptions() {
        // The registry's label/selector consistency is asserted upstream in
        // groundhog-spec-tool; this only pins that the exceptions the CLI's
        // wallet flows depend on stay registered. A permit flow can surface
        // any of the last three: the wallet refuses the signed operation, the
        // token refuses the grant, or the token refuses the transfer itself.
        for name in [
            "LibraryExn",
            "ERC20Error",
            "SignedOpError",
            "ERC20PermitError",
        ] {
            assert!(
                groundhog_spec_tool::describe::lookup(name).is_some(),
                "{name} missing from the describe registry"
            );
        }
    }
}
