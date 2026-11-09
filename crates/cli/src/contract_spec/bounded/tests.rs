use thruput_contract_spec::*;

use super::{Error, MAX_TYPE_DEPTH, MAX_WORK, check};

fn id() -> ContractSpecId {
    ContractSpecId {
        crate_name: "demo".try_into().unwrap(),
        major: "1".try_into().unwrap(),
        module_path: "demo".try_into().unwrap(),
        declared: "Node".try_into().unwrap(),
    }
}

fn field(ty: ContractType) -> ContractField {
    ContractField {
        name: "value".try_into().unwrap(),
        ty,
    }
}

fn spec(ty: ContractType) -> ContractSpec {
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: "Demo".try_into().unwrap(),
            vers: "1".try_into().unwrap(),
            throws: vec![ContractExceptionType {
                selector: 42,
                ty: ty.clone(),
            }]
            .try_into()
            .unwrap(),
            methods: vec![ContractSpecMethod {
                name: "get".try_into().unwrap(),
                args: vec![field(ty.clone())].try_into().unwrap(),
                return_type: ty.clone(),
                effect: Some(ContractMethodEffect::EffectReadOnly),
            }]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: id(),
                    fields: vec![field(ty.clone())].try_into().unwrap(),
                },
            },
            ContractTypeDef::TypeDefEnum {
                enum_def: ContractEnumDef {
                    name: id(),
                    variants: vec![ContractEnumVariant {
                        name: "A".try_into().unwrap(),
                        value: -1,
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            ContractTypeDef::TypeDefUnion {
                union_def: ContractUnionDef {
                    name: id(),
                    cases: vec![
                        ContractUnionCase {
                            name: "Empty".try_into().unwrap(),
                            discriminant: 0,
                            case_type: ContractUnionCaseType::UnionCaseVoid,
                        },
                        ContractUnionCase {
                            name: "Tuple".try_into().unwrap(),
                            discriminant: 1,
                            case_type: ContractUnionCaseType::UnionCaseTuple {
                                tuple: vec![ty.clone()].try_into().unwrap(),
                            },
                        },
                        ContractUnionCase {
                            name: "Fields".try_into().unwrap(),
                            discriminant: -1,
                            case_type: ContractUnionCaseType::UnionCaseStruct {
                                struct_fields: vec![field(ty.clone())].try_into().unwrap(),
                            },
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
        ]
        .try_into()
        .unwrap(),
        events: vec![ContractEventDef {
            name: id(),
            signature: [7; 32].into(),
            topics: vec![field(ty.clone())].try_into().unwrap(),
            fields: vec![field(ty)].try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
    }
}

fn roundtrip(spec: &ContractSpec) {
    let bytes = xdrs::txs::encode(spec).unwrap();
    check(&bytes).unwrap();
    let decoded: ContractSpec = xdrs::txs::decode(&bytes).unwrap();
    assert_eq!(&decoded, spec);
}

#[test]
fn preserves_all_type_variants_in_every_recursive_position() {
    let types = vec![
        ContractType::Void,
        ContractType::Bool,
        ContractType::U8,
        ContractType::U32,
        ContractType::I32,
        ContractType::U64,
        ContractType::I64,
        ContractType::U128,
        ContractType::I128,
        ContractType::U256,
        ContractType::I256,
        ContractType::String,
        ContractType::Bytes,
        ContractType::Address,
        ContractType::Named { named: id() },
        ContractType::Option {
            option_inner: Box::new(ContractType::U8),
        },
        ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::U8),
                len: 32,
            },
        },
        ContractType::Vector {
            vector: ContractVectorType {
                element: Box::new(ContractType::U8),
                max_len: 100,
            },
        },
        ContractType::Tuple {
            tuple: vec![ContractType::U8, ContractType::U64]
                .try_into()
                .unwrap(),
        },
        ContractType::SignedToken {
            signed_token_preimage: ContractSignedTokenPreimage {
                preimage_type: None,
            },
        },
        ContractType::SignedToken {
            signed_token_preimage: ContractSignedTokenPreimage {
                preimage_type: Some(Box::new(ContractType::U8)),
            },
        },
    ];
    for ty in types {
        roundtrip(&spec(ty));
    }
}

#[test]
fn allows_recursive_named_types() {
    roundtrip(&spec(ContractType::Option {
        option_inner: Box::new(ContractType::Named { named: id() }),
    }));
}

// Build malicious input as bytes, never as an owned recursively dropped value.
fn nested_return(depth: usize, kind: ContractTypeKind) -> Vec<u8> {
    let mut bytes = Vec::new();
    // One interface, empty name/version/throws, one method, empty name/args.
    for word in [1_u32, 0, 0, 0, 1, 0, 0] {
        bytes.extend(word.to_be_bytes());
    }
    for _ in 1..depth {
        bytes.extend(kind.0.to_be_bytes());
        if kind == ContractTypeKind::Tuple || kind == ContractTypeKind::SignedToken {
            bytes.extend(1_u32.to_be_bytes());
        }
    }
    bytes.extend(ContractTypeKind::U8.0.to_be_bytes());
    if kind == ContractTypeKind::Array || kind == ContractTypeKind::Vector {
        for _ in 1..depth {
            bytes.extend(32_u32.to_be_bytes());
        }
    }
    // Absent method effect, no typedefs or events.
    bytes.extend([0; 12]);
    bytes
}

#[test]
fn bounds_each_recursive_type_constructor() {
    for kind in [
        ContractTypeKind::Option,
        ContractTypeKind::Array,
        ContractTypeKind::Vector,
        ContractTypeKind::Tuple,
        ContractTypeKind::SignedToken,
    ] {
        let bytes = nested_return(MAX_TYPE_DEPTH, kind);
        check(&bytes).unwrap();
        let decoded: ContractSpec = xdrs::txs::decode(&bytes).unwrap();
        // Exercise the same recursive render and drop used by inspect.
        assert!(!thruput_spec_tool::render_spec_json(&decoded).is_empty());
        assert!(matches!(
            check(&nested_return(MAX_TYPE_DEPTH + 1, kind)),
            Err(Error::Depth)
        ));
    }
}

#[test]
fn rejects_twenty_thousand_levels_before_decoding() {
    assert!(matches!(
        check(&nested_return(20_000, ContractTypeKind::Option)),
        Err(Error::Depth)
    ));
}

#[test]
fn rejects_every_truncated_prefix() {
    let bytes = xdrs::txs::encode(&spec(ContractType::SignedToken {
        signed_token_preimage: ContractSignedTokenPreimage {
            preimage_type: Some(Box::new(ContractType::Named { named: id() })),
        },
    }))
    .unwrap();
    for end in 0..bytes.len() {
        assert!(check(&bytes[..end]).is_err(), "accepted prefix {end}");
    }
}

#[test]
fn rejects_impossible_collection_length() {
    assert!(matches!(
        check(&u32::MAX.to_be_bytes()),
        Err(Error::Read(_))
    ));
}

#[test]
fn bounds_total_work_across_collections() {
    let ty = ContractType::Tuple {
        tuple: vec![ContractType::Void; MAX_WORK / 10].try_into().unwrap(),
    };
    let bytes = xdrs::txs::encode(&spec(ty)).unwrap();
    assert!(matches!(check(&bytes), Err(Error::Work)));
}

#[test]
fn rejects_trailing_bytes() {
    let mut bytes = xdrs::txs::encode(&spec(ContractType::Void)).unwrap();
    bytes.push(0);
    assert!(matches!(check(&bytes), Err(Error::Read(_))));
}

#[test]
fn rejects_unknown_type_discriminant() {
    let mut bytes = nested_return(1, ContractTypeKind::Option);
    bytes[28..32].copy_from_slice(&i32::MAX.to_be_bytes());
    assert!(matches!(check(&bytes), Err(Error::Discriminant("type", _))));
}
