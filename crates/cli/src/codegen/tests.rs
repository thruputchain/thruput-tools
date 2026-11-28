use super::*;
use thruput_contract_spec::{
    ContractArrayType, ContractEnumVariant, ContractSpecString, ContractUnionCase,
    ContractVectorType,
};

fn spec_string(s: &str) -> ContractSpecString {
    s.try_into().unwrap()
}

/// A fixture id: one crate, one module, so the declared name is what
/// tells these types apart, which is what these tests turn on.
fn spec_id(declared: &str) -> ContractSpecId {
    ContractSpecId {
        crate_name: spec_string("demo"),
        major: spec_string("1"),
        module_path: spec_string("demo"),
        declared: spec_string(declared),
    }
}

#[test]
fn camel_case_leaves_single_word_names_unchanged() {
    assert_eq!(to_camel_case("transfer"), "transfer");
    assert_eq!(to_camel_case("get"), "get");
}

#[test]
fn camel_case_rewrites_snake_case_names() {
    assert_eq!(to_camel_case("get_balance"), "getBalance");
    assert_eq!(to_camel_case("get_balance_of"), "getBalanceOf");
    assert_eq!(to_camel_case("contract_owner"), "contractOwner");
}

#[test]
fn camel_case_preserves_leading_and_trailing_underscores() {
    assert_eq!(to_camel_case("default_"), "default_");
    assert_eq!(to_camel_case("_"), "_");
    assert_eq!(to_camel_case("_owner_id"), "_ownerId");
    assert_eq!(to_camel_case("owner_id_"), "ownerId_");
}

#[test]
fn camel_case_leaves_already_camel_names_unchanged() {
    assert_eq!(to_camel_case("getBalanceOf"), "getBalanceOf");
}

#[test]
fn maps_bare_u8_to_bigint() {
    assert_eq!(ts_type_ref(&ContractType::U8).unwrap(), "bigint");
}

#[test]
fn maps_scalar_types_to_ts() {
    assert_eq!(ts_type_ref(&ContractType::Void).unwrap(), "void");
    assert_eq!(ts_type_ref(&ContractType::Bool).unwrap(), "boolean");
    assert_eq!(ts_type_ref(&ContractType::U32).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::I32).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::U64).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::I64).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::U128).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::I128).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::U256).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::I256).unwrap(), "bigint");
    assert_eq!(ts_type_ref(&ContractType::String).unwrap(), "string");
    assert_eq!(
        ts_type_ref(&ContractType::Bytes).unwrap(),
        "globalThis.Uint8Array"
    );
    assert_eq!(
        ts_type_ref(&ContractType::Address).unwrap(),
        "__gh_protocol.ContractAddress"
    );
}

#[test]
fn maps_option_type() {
    let ty = ContractType::Option {
        option_inner: Box::new(ContractType::U64),
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "bigint | null");
}

#[test]
fn maps_signed_token_types() {
    use thruput_contract_spec::ContractSignedTokenPreimage;
    let opaque = ContractType::SignedToken {
        signed_token_preimage: ContractSignedTokenPreimage {
            preimage_type: None,
        },
    };
    assert_eq!(
        ts_type_ref(&opaque).unwrap(),
        "__gh_client_contract.SignedToken"
    );
    assert_eq!(
        spec_type_to_ts_literal(&opaque).unwrap(),
        "{ v: __gh_contract_spec.ContractTypeKind.SignedToken, \
         signed_token_preimage: { preimage_type: null } }"
    );

    let typed = ContractType::SignedToken {
        signed_token_preimage: ContractSignedTokenPreimage {
            preimage_type: Some(Box::new(ContractType::Named {
                named: spec_id("Auth"),
            })),
        },
    };
    assert_eq!(
        ts_type_ref(&typed).unwrap(),
        "__gh_client_contract.SignedToken<__gh_codegen.demo$v1$Auth>"
    );
    assert_eq!(
        spec_type_to_ts_literal(&typed).unwrap(),
        "{ v: __gh_contract_spec.ContractTypeKind.SignedToken, signed_token_preimage: \
         { preimage_type: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: \
         __gh_s(\"demo\"), major: __gh_s(\"1\"), module_path: __gh_s(\"demo\"), declared: \
         __gh_s(\"Auth\") } } } }"
    );

    let list = ContractType::Vector {
        vector: ContractVectorType {
            element: Box::new(opaque),
            max_len: u32::MAX,
        },
    };
    assert_eq!(
        ts_type_ref(&list).unwrap(),
        "__gh_client_contract.SignedToken[]"
    );
}

#[test]
fn event_token_check_handles_long_named_cycles() {
    let definitions: Vec<_> = (0..10_000)
        .map(|level| ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_id(&format!("Node{level}")),
                fields: vec![ContractField {
                    name: spec_string("next"),
                    ty: ContractType::Named {
                        named: spec_id(&format!("Node{}", (level + 1) % 10_000)),
                    },
                }]
                .try_into()
                .unwrap(),
            },
        })
        .collect();
    let index = definitions
        .iter()
        .map(|def| match def {
            ContractTypeDef::TypeDefStruct { struct_def } => (&struct_def.name, def),
            _ => unreachable!(),
        })
        .collect();
    let fields: Vec<_> = (0..1_000)
        .map(|level| ContractType::Named {
            named: spec_id(&format!("Node{level}")),
        })
        .collect();
    let mut visited = BTreeSet::new();
    for field in &fields {
        assert!(!type_carries_signed_token(field, &index, &mut visited));
    }
    let token = ContractType::SignedToken {
        signed_token_preimage: thruput_contract_spec::ContractSignedTokenPreimage {
            preimage_type: None,
        },
    };
    assert!(type_carries_signed_token(&token, &index, &mut visited));
}

#[test]
fn event_token_check_terminates_on_recursive_named_types() {
    // `Node { next: Node | null }` refers to itself; the walk must stop
    // rather than recurse forever, and a token-free cycle must pass.
    let node_def = ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("Node"),
            fields: vec![ContractField {
                name: spec_string("next"),
                ty: ContractType::Option {
                    option_inner: Box::new(ContractType::Named {
                        named: spec_id("Node"),
                    }),
                },
            }]
            .try_into()
            .unwrap(),
        },
    };
    let node_id = spec_id("Node");
    let index: BTreeMap<&ContractSpecId, &ContractTypeDef> =
        [(&node_id, &node_def)].into_iter().collect();
    assert!(!type_carries_signed_token(
        &ContractType::Named {
            named: spec_id("Node"),
        },
        &index,
        &mut BTreeSet::new(),
    ));
}

#[test]
fn maps_vector_type() {
    let ty = ContractType::Vector {
        vector: ContractVectorType {
            element: Box::new(ContractType::Bool),
            max_len: 100,
        },
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "boolean[]");
}

#[test]
fn maps_vector_of_option_with_parens() {
    let ty = ContractType::Vector {
        vector: ContractVectorType {
            element: Box::new(ContractType::Option {
                option_inner: Box::new(ContractType::U64),
            }),
            max_len: 100,
        },
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "(bigint | null)[]");
}

#[test]
fn maps_vector_of_tuple_with_parens() {
    let ty = ContractType::Vector {
        vector: ContractVectorType {
            element: Box::new(ContractType::Tuple {
                tuple: vec![ContractType::U64, ContractType::Bool]
                    .try_into()
                    .unwrap(),
            }),
            max_len: 100,
        },
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "(readonly [bigint, boolean])[]");
}

#[test]
fn maps_vector_of_u8_to_uint8array() {
    let ty = ContractType::Vector {
        vector: ContractVectorType {
            element: Box::new(ContractType::U8),
            max_len: 1024,
        },
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "globalThis.Uint8Array");
}

#[test]
fn maps_array_of_u8_to_fixed_bytes() {
    let ty = ContractType::Array {
        array: ContractArrayType {
            element: Box::new(ContractType::U8),
            len: 32,
        },
    };
    assert_eq!(
        ts_type_ref(&ty).unwrap(),
        "__gh_client_contract.FixedBytes<32>"
    );
}

#[test]
fn maps_large_array_to_fixed_array() {
    let ty = ContractType::Array {
        array: ContractArrayType {
            element: Box::new(ContractType::U32),
            len: 4,
        },
    };
    assert_eq!(
        ts_type_ref(&ty).unwrap(),
        "__gh_client_contract.FixedArray<bigint, 4>"
    );
}

#[test]
fn maps_tuple_type() {
    let ty = ContractType::Tuple {
        tuple: vec![ContractType::U32, ContractType::String]
            .try_into()
            .unwrap(),
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "readonly [bigint, string]");
}

#[test]
fn maps_named_type() {
    let ty = ContractType::Named {
        named: spec_id("ERC20Error"),
    };
    assert_eq!(ts_type_ref(&ty).unwrap(), "__gh_codegen.demo$v1$ERC20Error");
}

#[test]
fn generates_struct_interface() {
    let def = ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("TokenInfo"),
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
        "export interface TokenInfo { \"owner\": __gh_protocol.ContractAddress; \
         \"supply\": bigint; \"frozen\": boolean; }"
    );
}

#[test]
fn generates_enum() {
    let def = ContractTypeDef::TypeDefEnum {
        enum_def: ContractEnumDef {
            name: spec_id("ERC20Error"),
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
        "export type ERC20Error = 1n | 2n;"
    );
}

#[test]
fn generates_discriminated_union() {
    let def = ContractTypeDef::TypeDefUnion {
        union_def: ContractUnionDef {
            name: spec_id("Result"),
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
  { case: \"Ok\"; value: readonly [bigint] }
  | { case: \"Err\" }
  | { case: \"Detail\"; value: { \"code\": bigint; \"msg\": string; } };";
    assert_eq!(emit_type_def(&def).unwrap(), expected);
}

#[test]
fn maps_small_arrays_to_fixed_array() {
    for (len, expected) in [
        (0, "__gh_client_contract.FixedArray<bigint, 0>"),
        (1, "__gh_client_contract.FixedArray<bigint, 1>"),
        (2, "__gh_client_contract.FixedArray<bigint, 2>"),
        (3, "__gh_client_contract.FixedArray<bigint, 3>"),
    ] {
        let ty = ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::U64),
                len,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), expected);
    }
}

#[test]
fn escapes_literals_comments_and_property_keys() {
    assert_eq!(ts_string_literal("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    assert_eq!(ts_line_comment_text("a\nb"), "a\\nb");
    assert_eq!(ts_property_key("hyphen-key").unwrap(), "\"hyphen-key\"");

    let escaped_struct = ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("Escaped"),
            fields: vec![ContractField {
                name: spec_string("quoted\" field"),
                ty: ContractType::Bool,
            }]
            .try_into()
            .unwrap(),
        },
    };
    assert_eq!(
        emit_type_def(&escaped_struct).unwrap(),
        "export interface Escaped { \"quoted\\\" field\": boolean; }"
    );

    let mut output = String::new();
    emit_header(
        &mut output,
        "line1\nline2.spec.xdr",
        &[("Counter", "0.1.0")],
    );
    assert!(output.starts_with("// Generated from line1\\nline2.spec.xdr"));
}
