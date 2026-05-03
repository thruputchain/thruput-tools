//! TypeScript binding code generation from contract specs.

// These functions are consumed by the public `generate_ts_bindings` entry
// point added in a later task; suppress dead-code warnings in the interim.
#![allow(dead_code)]

use groundhog_contract_spec::ContractType;

use crate::CliError;
use crate::codec::spec_string_text;

/// Map a contract type to its TypeScript type reference string.
fn ts_type_ref(ty: &ContractType) -> Result<String, CliError> {
    match ty {
        ContractType::Void => Ok("void".into()),
        ContractType::Bool => Ok("boolean".into()),
        ContractType::U8
        | ContractType::U32
        | ContractType::I32
        | ContractType::U64
        | ContractType::I64
        | ContractType::U128
        | ContractType::I128 => Ok("bigint".into()),
        ContractType::String => Ok("string".into()),
        ContractType::Bytes => Ok("Uint8Array".into()),
        ContractType::Address => Ok("ContractAddress".into()),
        ContractType::Option { option_inner } => {
            let inner = option_inner.as_deref().ok_or_else(|| {
                CliError::SpecFailed("contract spec is missing an option inner type".into())
            })?;
            Ok(format!("{} | null", ts_type_ref(inner)?))
        }
        ContractType::Vector { vector } => {
            let element = vector.element.as_deref().ok_or_else(|| {
                CliError::SpecFailed("contract spec is missing a vector element type".into())
            })?;
            Ok(format!("{}[]", ts_type_ref(element)?))
        }
        ContractType::Array { array } => {
            let element = array.element.as_deref().ok_or_else(|| {
                CliError::SpecFailed("contract spec is missing an array element type".into())
            })?;
            Ok(format!("{}[]", ts_type_ref(element)?))
        }
        ContractType::Tuple { tuple } => {
            let items: Vec<String> = tuple.iter().map(ts_type_ref).collect::<Result<_, _>>()?;
            Ok(format!("[{}]", items.join(", ")))
        }
        ContractType::Named { named } => Ok(spec_string_text(named)?.to_owned()),
        _ => Err(CliError::SpecFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_contract_spec::{
        ContractArrayType, ContractSpecString, ContractVectorType,
    };

    fn spec_string(s: &str) -> ContractSpecString {
        s.try_into().unwrap()
    }

    #[test]
    fn maps_scalar_types_to_ts() {
        assert_eq!(ts_type_ref(&ContractType::Void).unwrap(), "void");
        assert_eq!(ts_type_ref(&ContractType::Bool).unwrap(), "boolean");
        assert_eq!(ts_type_ref(&ContractType::U8).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::U32).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::I32).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::U64).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::I64).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::U128).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::I128).unwrap(), "bigint");
        assert_eq!(ts_type_ref(&ContractType::String).unwrap(), "string");
        assert_eq!(ts_type_ref(&ContractType::Bytes).unwrap(), "Uint8Array");
        assert_eq!(
            ts_type_ref(&ContractType::Address).unwrap(),
            "ContractAddress"
        );
    }

    #[test]
    fn maps_option_type() {
        let ty = ContractType::Option {
            option_inner: Some(Box::new(ContractType::U64)),
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "bigint | null");
    }

    #[test]
    fn maps_vector_type() {
        let ty = ContractType::Vector {
            vector: ContractVectorType {
                element: Some(Box::new(ContractType::Bool)),
                max_len: 100,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "boolean[]");
    }

    #[test]
    fn maps_array_type() {
        let ty = ContractType::Array {
            array: ContractArrayType {
                element: Some(Box::new(ContractType::U8)),
                len: 32,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "bigint[]");
    }

    #[test]
    fn maps_tuple_type() {
        let ty = ContractType::Tuple {
            tuple: vec![ContractType::U32, ContractType::String]
                .try_into()
                .unwrap(),
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "[bigint, string]");
    }

    #[test]
    fn maps_named_type() {
        let ty = ContractType::Named {
            named: spec_string("ERC20Error"),
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "ERC20Error");
    }
}
