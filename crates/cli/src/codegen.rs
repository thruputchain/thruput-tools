//! TypeScript binding code generation from contract specs.

// These functions are consumed by the public `generate_ts_bindings` entry
// point added in a later task; suppress dead-code warnings in the interim.
#![allow(dead_code)]

use std::fmt::Write as _;

use groundhog_contract_spec::{
    ContractEnumDef, ContractField, ContractStructDef, ContractType, ContractTypeDef,
    ContractUnionCaseType, ContractUnionDef,
};

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

/// Emit a TypeScript type declaration for a named contract type definition.
fn emit_type_def(def: &ContractTypeDef) -> Result<String, CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => emit_struct(struct_def),
        ContractTypeDef::TypeDefEnum { enum_def } => emit_enum(enum_def),
        ContractTypeDef::TypeDefUnion { union_def } => emit_union(union_def),
        _ => Err(CliError::SpecFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

/// Emit a TypeScript interface for a contract struct definition.
fn emit_struct(def: &ContractStructDef) -> Result<String, CliError> {
    let name = spec_string_text(&def.name)?;
    let mut fields = String::new();
    for field in def.fields.iter() {
        let field_name = spec_string_text(&field.name)?;
        let ts_ty = ts_type_ref(&field.ty)?;
        if !fields.is_empty() {
            fields.push(' ');
        }
        let _ = write!(fields, "{field_name}: {ts_ty};");
    }
    Ok(format!("export interface {name} {{ {fields} }}"))
}

/// Emit a TypeScript enum for a contract enum definition.
fn emit_enum(def: &ContractEnumDef) -> Result<String, CliError> {
    let name = spec_string_text(&def.name)?;
    let mut variants = String::new();
    for variant in def.variants.iter() {
        let variant_name = spec_string_text(&variant.name)?;
        if !variants.is_empty() {
            variants.push(' ');
        }
        let _ = write!(variants, "{variant_name} = {},", variant.value);
    }
    Ok(format!("export enum {name} {{ {variants} }}"))
}

/// Emit a TypeScript discriminated union for a contract union definition.
fn emit_union(def: &ContractUnionDef) -> Result<String, CliError> {
    let name = spec_string_text(&def.name)?;
    let mut cases: Vec<String> = Vec::new();

    for case in def.cases.iter() {
        let case_name = spec_string_text(&case.name)?;
        let arm = match &case.case_type {
            ContractUnionCaseType::UnionCaseVoid => {
                format!("{{ case: \"{case_name}\" }}")
            }
            ContractUnionCaseType::UnionCaseTuple { tuple } => {
                let items: Vec<String> = tuple.iter().map(ts_type_ref).collect::<Result<_, _>>()?;
                format!("{{ case: \"{case_name}\"; value: [{}] }}", items.join(", "))
            }
            ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                let fields = emit_inline_struct_fields(struct_fields.iter())?;
                format!("{{ case: \"{case_name}\"; value: {{ {fields} }} }}")
            }
            _ => {
                return Err(CliError::SpecFailed(
                    "contract spec contains an unsupported union case kind".into(),
                ));
            }
        };
        cases.push(arm);
    }

    let mut body = String::new();
    for (i, arm) in cases.iter().enumerate() {
        if i == 0 {
            let _ = write!(body, "\n  {arm}");
        } else {
            let _ = write!(body, "\n  | {arm}");
        }
    }
    body.push(';');

    Ok(format!("export type {name} ={body}"))
}

/// Format struct fields as inline TypeScript object type fields.
fn emit_inline_struct_fields<'a>(
    fields: impl Iterator<Item = &'a ContractField>,
) -> Result<String, CliError> {
    let mut out = String::new();
    for field in fields {
        let field_name = spec_string_text(&field.name)?;
        let ts_ty = ts_type_ref(&field.ty)?;
        if !out.is_empty() {
            out.push(' ');
        }
        let _ = write!(out, "{field_name}: {ts_ty};");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_contract_spec::{
        ContractArrayType, ContractEnumVariant, ContractSpecString, ContractUnionCase,
        ContractVectorType,
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

    #[test]
    fn generates_struct_interface() {
        let def = ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_string("TokenInfo"),
                fields: vec![
                    ContractField {
                        name: spec_string("owner"),
                        ty: ContractType::Address,
                    },
                    ContractField {
                        name: spec_string("supply"),
                        ty: ContractType::U128,
                    },
                    ContractField {
                        name: spec_string("frozen"),
                        ty: ContractType::Bool,
                    },
                ]
                .try_into()
                .unwrap(),
            },
        };
        assert_eq!(
            emit_type_def(&def).unwrap(),
            "export interface TokenInfo { owner: ContractAddress; supply: bigint; frozen: boolean; }"
        );
    }

    #[test]
    fn generates_enum() {
        let def = ContractTypeDef::TypeDefEnum {
            enum_def: ContractEnumDef {
                name: spec_string("ERC20Error"),
                variants: vec![
                    ContractEnumVariant {
                        name: spec_string("AlreadyInitialized"),
                        value: 1,
                    },
                    ContractEnumVariant {
                        name: spec_string("MustBeContractOwner"),
                        value: 2,
                    },
                ]
                .try_into()
                .unwrap(),
            },
        };
        assert_eq!(
            emit_type_def(&def).unwrap(),
            "export enum ERC20Error { AlreadyInitialized = 1, MustBeContractOwner = 2, }"
        );
    }

    #[test]
    fn generates_discriminated_union() {
        let def = ContractTypeDef::TypeDefUnion {
            union_def: ContractUnionDef {
                name: spec_string("Result"),
                cases: vec![
                    ContractUnionCase {
                        name: spec_string("Ok"),
                        discriminant: 0,
                        case_type: ContractUnionCaseType::UnionCaseTuple {
                            tuple: vec![ContractType::U64].try_into().unwrap(),
                        },
                    },
                    ContractUnionCase {
                        name: spec_string("Err"),
                        discriminant: 1,
                        case_type: ContractUnionCaseType::UnionCaseVoid,
                    },
                    ContractUnionCase {
                        name: spec_string("Detail"),
                        discriminant: 2,
                        case_type: ContractUnionCaseType::UnionCaseStruct {
                            struct_fields: vec![
                                ContractField {
                                    name: spec_string("code"),
                                    ty: ContractType::U64,
                                },
                                ContractField {
                                    name: spec_string("msg"),
                                    ty: ContractType::String,
                                },
                            ]
                            .try_into()
                            .unwrap(),
                        },
                    },
                ]
                .try_into()
                .unwrap(),
            },
        };
        let expected = "\
export type Result =
  { case: \"Ok\"; value: [bigint] }
  | { case: \"Err\" }
  | { case: \"Detail\"; value: { code: bigint; msg: string; } };";
        assert_eq!(emit_type_def(&def).unwrap(), expected);
    }
}
