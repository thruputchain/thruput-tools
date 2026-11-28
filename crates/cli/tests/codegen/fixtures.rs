use thruput_contract_spec::{
    ContractArrayType, ContractEnumDef, ContractEnumVariant, ContractEventDef,
    ContractExceptionType, ContractField, ContractSpec, ContractSpecId, ContractSpecInterface,
    ContractSpecMethod, ContractSpecString, ContractStructDef, ContractType, ContractTypeDef,
    ContractUnionCase, ContractUnionCaseType, ContractUnionDef,
};

pub(super) fn spec_string(s: &str) -> ContractSpecString {
    s.try_into().unwrap()
}

/// A fixture id: one crate, one module, so the declared name is what
/// tells these types apart, which is what these tests turn on.
pub(super) fn spec_id(declared: &str) -> ContractSpecId {
    spec_id_at("demo", "demo", declared)
}

/// An event with one topic field and one payload field.
pub(super) fn ping_event_def() -> ContractEventDef {
    ContractEventDef {
        name: spec_id("PingEvent"),
        signature: [0xAB; 32].into(),
        topics: vec![ContractField {
            name: spec_string("tag"),
            ty: ContractType::U64,
        }]
        .try_into()
        .unwrap(),
        fields: vec![ContractField {
            name: spec_string("note"),
            ty: ContractType::String,
        }]
        .try_into()
        .unwrap(),
    }
}

/// A one-interface spec whose only content is `ping_event_def`.
pub(super) fn spec_with_events() -> ContractSpec {
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("Counter"),
            vers: spec_string("0.1.0"),
            throws: vec![].try_into().unwrap(),
            methods: vec![].try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![ping_event_def()].try_into().unwrap(),
    }
}

/// An interface named `name` at version `vers` with one method.
pub(super) fn interface(name: &str, vers: &str, method: &str) -> ContractSpecInterface {
    ContractSpecInterface {
        name: spec_string(name),
        vers: spec_string(vers),
        throws: vec![].try_into().unwrap(),
        methods: vec![ContractSpecMethod {
            name: spec_string(method),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::U64,
            effect: None,
        }]
        .try_into()
        .unwrap(),
    }
}

pub(super) fn spec_with_interfaces(interfaces: Vec<ContractSpecInterface>) -> ContractSpec {
    ContractSpec {
        interfaces: interfaces.try_into().unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    }
}

pub(super) fn spec_id_at(krate: &str, module_path: &str, declared: &str) -> ContractSpecId {
    ContractSpecId {
        crate_name: spec_string(krate),
        major: spec_string("1"),
        module_path: spec_string(module_path),
        declared: spec_string(declared),
    }
}

/// A struct in `demo` holding a type declared in crate `other`.
pub(super) fn cross_crate_spec(holder_module: &str) -> ContractSpec {
    let mut spec = spec_with_events();
    spec.events = vec![].try_into().unwrap();
    spec.type_defs = vec![
        ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_id_at("other", "other", "Thing"),
                fields: vec![ContractField {
                    name: spec_string("n"),
                    ty: ContractType::U64,
                }]
                .try_into()
                .unwrap(),
            },
        },
        ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_id_at("demo", holder_module, "Holder"),
                fields: vec![ContractField {
                    name: spec_string("inner"),
                    ty: ContractType::Named {
                        named: spec_id_at("other", "other", "Thing"),
                    },
                }]
                .try_into()
                .unwrap(),
            },
        },
    ]
    .try_into()
    .unwrap();
    spec
}

pub(super) fn struct_at(name: ContractSpecId) -> ContractTypeDef {
    ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name,
            fields: vec![ContractField {
                name: spec_string("x"),
                ty: ContractType::U64,
            }]
            .try_into()
            .unwrap(),
        },
    }
}

pub(super) fn enum_at(name: ContractSpecId) -> ContractTypeDef {
    ContractTypeDef::TypeDefEnum {
        enum_def: ContractEnumDef {
            name,
            variants: vec![ContractEnumVariant {
                name: spec_string("A"),
                value: 1,
            }]
            .try_into()
            .unwrap(),
        },
    }
}

pub(super) fn union_at(name: ContractSpecId) -> ContractTypeDef {
    ContractTypeDef::TypeDefUnion {
        union_def: ContractUnionDef {
            name,
            cases: vec![ContractUnionCase {
                name: spec_string("Ok"),
                discriminant: 0,
                case_type: ContractUnionCaseType::UnionCaseVoid,
            }]
            .try_into()
            .unwrap(),
        },
    }
}

/// `outer`, declared at `demo`'s root, beside a module holding `inner`.
pub(super) fn beside_a_module_holding(
    outer: ContractTypeDef,
    inner: ContractTypeDef,
) -> ContractSpec {
    let mut spec = spec_with_events();
    spec.events = vec![].try_into().unwrap();
    spec.type_defs = vec![outer, inner].try_into().unwrap();
    spec
}

pub(super) fn counter_spec_with_methods(
    methods: Vec<ContractSpecMethod>,
    include_events: bool,
) -> ContractSpec {
    counter_spec_with_interface_name("Counter", methods, include_events)
}

pub(super) fn counter_spec_with_interface_name(
    iface_name: &str,
    methods: Vec<ContractSpecMethod>,
    include_events: bool,
) -> ContractSpec {
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string(iface_name),
            vers: spec_string("0.1.0"),
            throws: vec![].try_into().unwrap(),
            methods: methods.try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: if include_events {
            vec![ping_event_def()].try_into().unwrap()
        } else {
            vec![].try_into().unwrap()
        },
    }
}

/// `get() -> U64` and `increment(amount: U64) -> Void`.
pub(super) fn counter_spec_methods() -> Vec<ContractSpecMethod> {
    vec![
        ContractSpecMethod {
            name: spec_string("get"),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::U64,
            effect: None,
        },
        ContractSpecMethod {
            name: spec_string("increment"),
            args: vec![ContractField {
                name: spec_string("amount"),
                ty: ContractType::U64,
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::Void,
            effect: None,
        },
    ]
}

/// Build a minimal counter contract spec with [`counter_spec_methods`].
pub(super) fn counter_spec() -> ContractSpec {
    counter_spec_with_methods(counter_spec_methods(), false)
}

/// ERC20-shaped contract with error enum, throws, and multiple methods.
pub(super) fn erc20_spec() -> ContractSpec {
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("ERC20"),
            vers: spec_string("0.1.0"),
            throws: vec![ContractExceptionType {
                selector: 0xb199_47ac_b2fa_14ff,
                ty: ContractType::Named {
                    named: spec_id("ERC20Error"),
                },
            }]
            .try_into()
            .unwrap(),
            methods: vec![
                ContractSpecMethod {
                    name: spec_string("transfer"),
                    args: vec![
                        ContractField {
                            name: spec_string("to"),
                            ty: ContractType::Address,
                        },
                        ContractField {
                            name: spec_string("amount"),
                            ty: ContractType::U128,
                        },
                    ]
                    .try_into()
                    .unwrap(),
                    return_type: ContractType::Void,
                    effect: None,
                },
                ContractSpecMethod {
                    name: spec_string("get_balance"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U128,
                    effect: None,
                },
                ContractSpecMethod {
                    name: spec_string("get_balance_of"),
                    args: vec![ContractField {
                        name: spec_string("account_id"),
                        ty: ContractType::Address,
                    }]
                    .try_into()
                    .unwrap(),
                    return_type: ContractType::U128,
                    effect: None,
                },
            ]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![ContractTypeDef::TypeDefEnum {
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
                    ContractEnumVariant {
                        name: spec_string("TooLarge"),
                        value: 3,
                    },
                    ContractEnumVariant {
                        name: spec_string("Internal"),
                        value: 100,
                    },
                ]
                .try_into()
                .unwrap(),
            },
        }]
        .try_into()
        .unwrap(),
        events: vec![].try_into().unwrap(),
    }
}

/// Contract with recursive type (Tree with optional left/right children).
pub(super) fn recursive_type_spec() -> ContractSpec {
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("RecursiveTypeContract"),
            vers: spec_string("0.1.0"),
            throws: vec![].try_into().unwrap(),
            methods: vec![ContractSpecMethod {
                name: spec_string("depth"),
                args: vec![ContractField {
                    name: spec_string("tree"),
                    ty: ContractType::Named {
                        named: spec_id("Tree"),
                    },
                }]
                .try_into()
                .unwrap(),
                return_type: ContractType::U32,
                effect: None,
            }]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_id("Tree"),
                fields: vec![
                    ContractField {
                        name: spec_string("value"),
                        ty: ContractType::U32,
                    },
                    ContractField {
                        name: spec_string("left"),
                        ty: ContractType::Option {
                            option_inner: Box::new(ContractType::Named {
                                named: spec_id("Tree"),
                            }),
                        },
                    },
                    ContractField {
                        name: spec_string("right"),
                        ty: ContractType::Option {
                            option_inner: Box::new(ContractType::Named {
                                named: spec_id("Tree"),
                            }),
                        },
                    },
                ]
                .try_into()
                .unwrap(),
            },
        }]
        .try_into()
        .unwrap(),
        events: vec![].try_into().unwrap(),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the snapshot fixture keeps its complete ABI shape visible in one place"
)]
pub(super) fn typed_contract_spec() -> ContractSpec {
    use thruput_contract_spec::ContractMethodEffect as Effect;

    let request_id = ContractType::Array {
        array: ContractArrayType {
            element: Box::new(ContractType::U8),
            len: 32,
        },
    };
    let amounts = ContractType::Array {
        array: ContractArrayType {
            element: Box::new(ContractType::U128),
            len: 2,
        },
    };
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("ExampleContract"),
            vers: spec_string("0.1.0"),
            throws: vec![ContractExceptionType {
                selector: 0x1234,
                ty: ContractType::Named {
                    named: spec_id("ExampleError"),
                },
            }]
            .try_into()
            .unwrap(),
            methods: vec![
                ContractSpecMethod {
                    effect: Some(Effect::EffectReadOnly),
                    name: spec_string("get_values"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::Tuple {
                        tuple: vec![ContractType::U128, ContractType::U128, ContractType::U64]
                            .try_into()
                            .unwrap(),
                    },
                },
                ContractSpecMethod {
                    effect: Some(Effect::EffectStateChanging),
                    name: spec_string("update"),
                    args: vec![
                        ContractField {
                            name: spec_string("value"),
                            ty: ContractType::U128,
                        },
                        ContractField {
                            name: spec_string("request_id"),
                            ty: ContractType::Option {
                                option_inner: Box::new(request_id.clone()),
                            },
                        },
                    ]
                    .try_into()
                    .unwrap(),
                    return_type: ContractType::Void,
                },
                ContractSpecMethod {
                    effect: Some(Effect::EffectPure),
                    name: spec_string("calculate"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U64,
                },
                ContractSpecMethod {
                    effect: None,
                    name: spec_string("undeclared"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::Void,
                },
            ]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![ContractTypeDef::TypeDefUnion {
            union_def: ContractUnionDef {
                name: spec_id("ExampleError"),
                cases: vec![
                    ContractUnionCase {
                        name: spec_string("InvalidValue"),
                        discriminant: -1,
                        case_type: ContractUnionCaseType::UnionCaseVoid,
                    },
                    ContractUnionCase {
                        name: spec_string("UpdateFailed"),
                        discriminant: 2,
                        case_type: ContractUnionCaseType::UnionCaseTuple {
                            tuple: vec![ContractType::U32].try_into().unwrap(),
                        },
                    },
                ]
                .try_into()
                .unwrap(),
            },
        }]
        .try_into()
        .unwrap(),
        events: vec![ContractEventDef {
            name: spec_id("ChangedEvent"),
            signature: [0x5a; 32].into(),
            topics: vec![
                ContractField {
                    name: spec_string("actor"),
                    ty: ContractType::Address,
                },
                ContractField {
                    name: spec_string("request_id"),
                    ty: request_id.clone(),
                },
            ]
            .try_into()
            .unwrap(),
            fields: vec![
                ContractField {
                    name: spec_string("previous_values"),
                    ty: amounts.clone(),
                },
                ContractField {
                    name: spec_string("current_values"),
                    ty: amounts,
                },
                ContractField {
                    name: spec_string("request_id"),
                    ty: ContractType::Option {
                        option_inner: Box::new(request_id),
                    },
                },
            ]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
    }
}

/// A spec named after what the bindings reach for: standard-library and runtime
/// types, the generated `spec`, the decoder's parameter, and another crate,
/// including from inside a module of that crate's name.
#[expect(
    clippy::too_many_lines,
    reason = "the snapshot fixture keeps its complete ABI shape visible in one place"
)]
pub(super) fn shadowed_names_spec() -> ContractSpec {
    let signed_token = ContractType::SignedToken {
        signed_token_preimage: thruput_contract_spec::ContractSignedTokenPreimage {
            preimage_type: None,
        },
    };
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("Shadowed"),
            vers: spec_string("0.1.0"),
            throws: vec![].try_into().unwrap(),
            methods: vec![ContractSpecMethod {
                name: spec_string("authorize"),
                args: vec![ContractField {
                    name: spec_string("holder"),
                    ty: ContractType::Named {
                        named: spec_id_at("demo", "demo", "Holder"),
                    },
                }]
                .try_into()
                .unwrap(),
                return_type: signed_token.clone(),
                effect: None,
            }]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![
            // Named like the standard library's `Uint8Array`.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id("Uint8Array"),
                    fields: vec![ContractField {
                        name: spec_string("bytes"),
                        ty: ContractType::Bytes,
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            // Named like the runtime's `SignedToken`.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id("SignedToken"),
                    fields: vec![ContractField {
                        name: spec_string("token"),
                        ty: signed_token,
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            // Declared in crate `other`, and referenced from `demo`.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("other", "other", "Thing"),
                    fields: vec![ContractField {
                        name: spec_string("n"),
                        ty: ContractType::U64,
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("demo", "demo", "Holder"),
                    fields: vec![ContractField {
                        name: spec_string("inner"),
                        ty: ContractType::Named {
                            named: spec_id_at("other", "other", "Thing"),
                        },
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            // Named after crate `demo`, and referring to it from inside
            // `other.v1`, where the type does not capture the crate's name.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("other", "other", "demo"),
                    fields: vec![ContractField {
                        name: spec_string("holder"),
                        ty: ContractType::Named {
                            named: spec_id_at("demo", "demo", "Holder"),
                        },
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            // In a module named after crate `other`, referring to that crate.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("demo", "demo::other", "Holder"),
                    fields: vec![ContractField {
                        name: spec_string("inner"),
                        ty: ContractType::Named {
                            named: spec_id_at("other", "other", "Thing"),
                        },
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            // In a module named after the global object, holding bytes.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("demo", "demo::globalThis", "Blob"),
                    fields: vec![ContractField {
                        name: spec_string("bytes"),
                        ty: ContractType::Bytes,
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
        ]
        .try_into()
        .unwrap(),
        events: vec![
            // Topic-only, so its filter aliases `Record<string, never>`.
            {
                let mut event = ping_event_def();
                event.name = spec_id("Record");
                event.topics.0[0].ty = ContractType::String;
                event
            },
            // Declared in a crate named like the decoder's parameter.
            {
                let mut event = ping_event_def();
                event.name = spec_id_at("event", "event", "Ping");
                event.signature = [0xCD; 32].into();
                event
            },
            // Opens a namespace named after the spec literal.
            {
                let mut event = ping_event_def();
                event.name = spec_id_at("demo", "demo::spec", "Pinged");
                event.signature = [0xEF; 32].into();
                event
            },
        ]
        .try_into()
        .unwrap(),
    }
}

/// A spec declaring types beside modules of the same name, in each pairing
/// TypeScript can merge: an interface, a type alias, an enum's const and an
/// event's const, each beside a namespace holding types or values.
pub(super) fn namespace_merges_spec() -> ContractSpec {
    let at = |module: &str, declared: &str| spec_id_at("demo", module, declared);
    ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("Merged"),
            vers: spec_string("0.1.0"),
            throws: vec![].try_into().unwrap(),
            methods: vec![].try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![
            // An interface beside a namespace holding a type.
            struct_at(at("demo", "foo")),
            struct_at(at("demo::foo", "Bar")),
            // A type alias beside a namespace holding a value.
            union_at(at("demo", "bar")),
            enum_at(at("demo::bar", "Kind")),
            // An enum's const beside a namespace holding a type.
            enum_at(at("demo", "baz")),
            struct_at(at("demo::baz", "Inner")),
            // Referenced by the event beside the module it sits in.
            struct_at(at("demo::qux", "Detail")),
        ]
        .try_into()
        .unwrap(),
        // An event's const beside a namespace holding a type.
        events: vec![{
            let mut event = ping_event_def();
            event.name = at("demo", "qux");
            event.fields.0[0].ty = ContractType::Named {
                named: at("demo::qux", "Detail"),
            };
            event
        }]
        .try_into()
        .unwrap(),
    }
}

/// A spec whose names the bindings escape: a reserved word as a module, a
/// type and an interface name, and a crate named after the spec literal;
/// and one interface at two majors.
pub(super) fn escaped_names_spec() -> ContractSpec {
    let kind = || ContractType::Named {
        named: spec_id_at("demo", "demo::default", "Kind"),
    };
    let token = |vers: &str, args: Vec<ContractField>, return_type| ContractSpecInterface {
        name: spec_string("Token"),
        vers: spec_string(vers),
        throws: vec![].try_into().unwrap(),
        methods: vec![ContractSpecMethod {
            name: spec_string("balance"),
            args: args.try_into().unwrap(),
            return_type,
            effect: None,
        }]
        .try_into()
        .unwrap(),
    };
    ContractSpec {
        interfaces: vec![
            ContractSpecInterface {
                name: spec_string("default"),
                vers: spec_string("0.1.0"),
                throws: vec![ContractExceptionType {
                    selector: 0x1,
                    ty: kind(),
                }]
                .try_into()
                .unwrap(),
                methods: vec![ContractSpecMethod {
                    name: spec_string("get"),
                    args: vec![].try_into().unwrap(),
                    return_type: kind(),
                    effect: None,
                }]
                .try_into()
                .unwrap(),
            },
            token("0.4.0", vec![], ContractType::U64),
            token(
                "1.2.0",
                vec![ContractField {
                    name: spec_string("of"),
                    ty: ContractType::Address,
                }],
                ContractType::U128,
            ),
        ]
        .try_into()
        .unwrap(),
        type_defs: vec![
            ContractTypeDef::TypeDefEnum {
                enum_def: ContractEnumDef {
                    name: spec_id_at("demo", "demo::default", "Kind"),
                    variants: vec![
                        ContractEnumVariant {
                            name: spec_string("Active"),
                            value: 1,
                        },
                        ContractEnumVariant {
                            name: spec_string("Frozen"),
                            value: 2,
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("demo", "demo", "string"),
                    fields: vec![
                        ContractField {
                            name: spec_string("kind"),
                            ty: kind(),
                        },
                        ContractField {
                            name: spec_string("thing"),
                            ty: ContractType::Named {
                                named: spec_id_at("spec", "spec", "Thing"),
                            },
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
            struct_at(spec_id_at("spec", "spec", "Thing")),
        ]
        .try_into()
        .unwrap(),
        events: vec![{
            let mut event = ping_event_def();
            event.name = spec_id_at("spec", "spec", "Minted");
            event
        }]
        .try_into()
        .unwrap(),
    }
}
