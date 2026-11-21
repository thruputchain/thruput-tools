mod fixtures;
mod snapshots;

use fixtures::*;
use thruput_cli::{CliError, Print, codegen_cmd};
use thruput_contract_spec::{
    ContractArrayType, ContractEnumDef, ContractEnumVariant, ContractEventDef,
    ContractExceptionType, ContractField, ContractSpec, ContractSpecInterface, ContractSpecMethod,
    ContractStructDef, ContractType, ContractTypeDef, ContractUnionCase, ContractUnionCaseType,
    ContractUnionDef, ContractVectorType,
};

fn generate(spec: &ContractSpec, source_name: &str) -> Result<String, CliError> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join(source_name);
    std::fs::write(&source, xdrs::txs::encode(spec).unwrap())?;
    generate_file(&source, &dir.path().join("binding.ts"))
}

fn generate_file(source: &std::path::Path, output: &std::path::Path) -> Result<String, CliError> {
    codegen_cmd::run(source, Some(output), "codegen.ts", &Print::new(true, false))?;
    Ok(std::fs::read_to_string(output)?)
}

#[test]
fn codegen_bounds_namespace_depth_independently_of_type_depth() {
    for (depth, accepted) in [(62, true), (63, false), (10_000, false)] {
        let mut name = spec_id("Empty");
        name.module_path = spec_string(&vec!["module"; depth].join("::"));
        let spec = ContractSpec {
            interfaces: vec![].try_into().unwrap(),
            type_defs: vec![ContractTypeDef::TypeDefEnum {
                enum_def: ContractEnumDef {
                    name,
                    variants: vec![].try_into().unwrap(),
                },
            }]
            .try_into()
            .unwrap(),
            events: vec![].try_into().unwrap(),
        };
        let result = generate(&spec, "namespace.spec.xdr");
        if accepted {
            result.unwrap();
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("namespace exceeds nesting limit")
            );
        }
    }
}

#[test]
fn generates_signed_token_returns_from_the_client_runtime() {
    use thruput_contract_spec::ContractSignedTokenPreimage;
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("authorize"),
            args: vec![ContractField {
                name: spec_string("message"),
                ty: ContractType::Bytes,
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::Vector {
                vector: ContractVectorType {
                    element: Box::new(ContractType::SignedToken {
                        signed_token_preimage: ContractSignedTokenPreimage {
                            preimage_type: None,
                        },
                    }),
                    max_len: u32::MAX,
                },
            },
            effect: None,
        }],
        false,
    );
    let output = generate(&spec, "wallet.spec.xdr").unwrap();
    assert!(
        output.contains("export type AuthorizeResult = __gh_client_contract.SignedToken[];"),
        "return type must be a token placeholder list:\n{output}"
    );
    assert!(
        output.contains(
            "{ v: __gh_contract_spec.ContractTypeKind.SignedToken, \
             signed_token_preimage: { preimage_type: null } }"
        ),
        "spec literal must carry the signed-token type:\n{output}"
    );
}

#[test]
fn rejects_signed_tokens_in_event_payloads() {
    use thruput_contract_spec::ContractSignedTokenPreimage;
    let token = ContractType::SignedToken {
        signed_token_preimage: ContractSignedTokenPreimage {
            preimage_type: None,
        },
    };
    // `Auth { token }` is reached only by resolving the named definition,
    // which is what a container-only walk misses.
    let auth_def = ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("Auth"),
            fields: vec![ContractField {
                name: spec_string("token"),
                ty: token.clone(),
            }]
            .try_into()
            .unwrap(),
        },
    };
    for (label, ty) in [
        ("direct", token.clone()),
        (
            "nested",
            ContractType::Vector {
                vector: ContractVectorType {
                    element: Box::new(token),
                    max_len: u32::MAX,
                },
            },
        ),
        (
            "named",
            ContractType::Named {
                named: spec_id("Auth"),
            },
        ),
        (
            "named inside a container",
            ContractType::Option {
                option_inner: Box::new(ContractType::Named {
                    named: spec_id("Auth"),
                }),
            },
        ),
    ] {
        let mut spec = spec_with_events();
        spec.type_defs = vec![auth_def.clone()].try_into().unwrap();
        let mut event = ping_event_def();
        event.fields = vec![ContractField {
            name: spec_string("auth"),
            ty,
        }]
        .try_into()
        .unwrap();
        spec.events = vec![event].try_into().unwrap();
        let err = generate(&spec, "wallet.spec.xdr").unwrap_err();
        assert!(
            err.to_string().contains("carries a signed token"),
            "{label}: {err}"
        );
    }
}

#[test]
fn generates_events_with_shared_named_type_graphs() {
    let mut spec = spec_with_events();
    let definitions: Vec<_> = (0..60)
        .map(|level| {
            let ty = if level == 0 {
                ContractType::U64
            } else {
                ContractType::Named {
                    named: spec_id(&format!("Node{}", level - 1)),
                }
            };
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id(&format!("Node{level}")),
                    fields: vec![
                        ContractField {
                            name: spec_string("left"),
                            ty: ty.clone(),
                        },
                        ContractField {
                            name: spec_string("right"),
                            ty,
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            }
        })
        .collect();
    spec.type_defs = definitions.try_into().unwrap();
    let mut event = ping_event_def();
    event.fields = vec![ContractField {
        name: spec_string("tree"),
        ty: ContractType::Named {
            named: spec_id("Node59"),
        },
    }]
    .try_into()
    .unwrap();
    spec.events = vec![event].try_into().unwrap();
    // Named references are leaves in the wire-format depth check.
    generate(&spec, "shared.spec.xdr").unwrap();

    let mut definitions = spec.type_defs.to_vec();
    let ContractTypeDef::TypeDefStruct { struct_def } = &mut definitions[0] else {
        unreachable!();
    };
    let mut fields = struct_def.fields.to_vec();
    fields[0].ty = ContractType::SignedToken {
        signed_token_preimage: thruput_contract_spec::ContractSignedTokenPreimage {
            preimage_type: None,
        },
    };
    struct_def.fields = fields.try_into().unwrap();
    spec.type_defs = definitions.try_into().unwrap();
    assert!(
        generate(&spec, "shared.spec.xdr")
            .unwrap_err()
            .to_string()
            .contains("carries a signed token")
    );
}

#[test]
fn reaches_the_runtime_signed_token_past_a_type_of_that_name() {
    use thruput_contract_spec::ContractSignedTokenPreimage;
    let mut spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("authorize"),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::SignedToken {
                signed_token_preimage: ContractSignedTokenPreimage {
                    preimage_type: None,
                },
            },
            effect: None,
        }],
        false,
    );
    spec.type_defs = vec![ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("SignedToken"),
            fields: vec![ContractField {
                name: spec_string("id"),
                ty: ContractType::U64,
            }]
            .try_into()
            .unwrap(),
        },
    }]
    .try_into()
    .unwrap();
    // The runtime's name is only ever written through its module.
    let output = generate(&spec, "wallet.spec.xdr").unwrap();
    assert!(
        output.contains("    export interface SignedToken"),
        "spec type must still be declared:\n{output}"
    );
    assert!(
        output.contains("export type AuthorizeResult = __gh_client_contract.SignedToken;"),
        "the method result must resolve to the runtime type:\n{output}"
    );
}

#[test]
fn nested_arrays_do_not_expand_generated_output() {
    let mut ty = ContractType::U32;
    for _ in 0..20 {
        ty = ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ty),
                len: 3,
            },
        };
    }
    let mut methods = counter_spec_methods();
    methods[0].return_type = ty;
    let spec = counter_spec_with_methods(methods, false);
    let output = generate(&spec, "nested.spec.xdr").unwrap();
    assert!(output.len() < 100_000);
    assert!(output.contains(&format!(
        "{}bigint{}",
        "__gh_client_contract.FixedArray<".repeat(20),
        ", 3>".repeat(20)
    )));
}

#[test]
fn spec_literal_includes_events() {
    let output = generate(&spec_with_events(), "counter.spec.xdr").unwrap();
    assert!(
        output.contains("events: ["),
        "spec literal missing events array:\n{output}"
    );
    assert!(
        output.contains(&format!(
            "signature: __gh_protocol.topic(__gh_protocol_utils.fromHex(\"{}\"))",
            "ab".repeat(32)
        )),
        "event signature literal missing:\n{output}"
    );
    assert!(
        output.contains(
            "{ name: __gh_s(\"tag\"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } }"
        ),
        "event topic field literal missing:\n{output}"
    );
    assert!(
        output.contains(
            "{ name: __gh_s(\"note\"), ty: { v: __gh_contract_spec.ContractTypeKind.String } }"
        ),
        "event payload field literal missing:\n{output}"
    );
}

#[test]
fn generates_event_types_and_decoders() {
    let output = generate(&spec_with_events(), "counter.spec.xdr").unwrap();

    assert!(
        output.contains(
            "export interface PingEvent extends __gh_client_contract.DecodedContractEvent<"
        ),
        "missing decoded event type:\n{output}"
    );
    assert!(
        output.contains("\"tag\": bigint }")
            && !output.contains("original value cannot be recovered"),
        "missing direct scalar topic shape:\n{output}"
    );
    assert!(
        output.contains("{ \"note\": string }"),
        "missing payload shape:\n{output}"
    );
    assert!(
        output.contains("\"tag\"?: __gh_client_contract.ContractEventFilterValue<bigint>;"),
        "missing scalar topic filter:\n{output}"
    );
    assert!(
        output.contains("PingEvent: __gh_client_contract.defineContractEvent<"),
        "missing event descriptor:\n{output}"
    );
    assert!(
        output.contains("signature: spec.events[0]!.signature"),
        "missing signature metadata:\n{output}"
    );
    assert!(output.contains("type: spec.events[0]!.topics[0]!.ty"));
    assert!(output.contains("type: spec.events[0]!.fields[0]!.ty"));
    assert!(
        output.contains("representation: \"u64\""),
        "missing topic representation metadata:\n{output}"
    );

    assert!(
        output.contains("export type CounterEvent = demo.v1.PingEvent;"),
        "missing union:\n{output}"
    );
    assert!(
        output.contains(
            "export function decodeEvent(__gh_event: { topics: globalThis.Uint8Array[]; \
             data: globalThis.Uint8Array }): CounterEvent | null"
        ),
        "missing decodeEvent:\n{output}"
    );
    assert!(
        output.contains("return demo.v1.PingEvent.decode(__gh_event);"),
        "missing dispatch body:\n{output}"
    );
    assert!(
        output.contains("codecs: __gh_codecs,"),
        "missing shared codecs:\n{output}"
    );
    assert!(!output.contains("CxsReader"));
    assert!(!output.contains("resolveCodec"));
}

#[test]
fn event_only_binding_defines_spec_string_helper() {
    let output = generate(&spec_with_events(), "counter.spec.xdr").unwrap();

    assert!(output.contains("const __gh_s = __gh_contract_spec.contractSpecString;"));
    assert!(output.contains("name: __gh_s(\"Counter\")"));
}

#[test]
fn no_event_section_without_events() {
    let output = generate(&counter_spec(), "counter.spec.xdr").unwrap();
    assert!(
        !output.contains("decodeEvent"),
        "unexpected decodeEvent:\n{output}"
    );
    assert!(
        !output.contains("CxsReader"),
        "unexpected decoder imports:\n{output}"
    );
}

#[test]
fn event_named_after_the_union_is_namespaced_apart() {
    // The combined-decoder union is named "{FirstInterface}Event" at top
    // level. An event with that exact name used to collide with it;
    // namespacing keeps them apart, so this generates.
    let mut spec = spec_with_events();
    let mut event = ping_event_def();
    event.name = spec_id("CounterEvent");
    spec.events = vec![event].try_into().unwrap();
    let output = generate(&spec, "counter.spec.xdr").unwrap();
    assert!(output.contains("export type CounterEvent = demo.v1.CounterEvent;"));
    assert!(output.contains("return demo.v1.CounterEvent.decode(__gh_event);"));
}

#[test]
fn event_name_colliding_with_type_def_errors() {
    let mut spec = spec_with_events();
    spec.type_defs = vec![ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("PingEvent"),
            fields: vec![].try_into().unwrap(),
        },
    }]
    .try_into()
    .unwrap();
    let error = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(error.to_string().contains(
        "generated TypeScript value name \"demo.v1.PingEvent\" collides between struct \
         demo::PingEvent (demo@1) and event demo::PingEvent (demo@1)"
    ));
}

#[test]
fn interface_types_are_namespaced_apart_from_spec_types() {
    // Spec types named like the generated ones nest under their crate, and
    // the generated ones under their interface's name and major.
    let mut spec = erc20_spec();
    for name in [
        "TransferArgs",
        "TransferResult",
        "DeclaredException",
        "Exception",
    ] {
        spec.type_defs.0.push(struct_at(spec_id(name)));
    }
    let output = generate(&spec, "erc20.spec.xdr").unwrap();
    for expected in [
        "export namespace ERC20 {\n\n  export namespace v0_1 {\n\n    export interface \
         TransferArgs {",
        "    export interface TransferArgs { \"x\": bigint; }",
        "    export type Exception = __gh_client_contract.ContractException<DeclaredException>;",
        "    export interface Exception { \"x\": bigint; }",
        "    ERC20.v0_1.TransferArgs,",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
}

#[test]
fn method_name_public_collision_errors() {
    let spec = counter_spec_with_methods(
        vec![
            ContractSpecMethod {
                name: spec_string("get_balance"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::U64,
                effect: None,
            },
            ContractSpecMethod {
                name: spec_string("getBalance"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::U64,
                effect: None,
            },
        ],
        false,
    );

    let err = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(err.to_string().contains("getBalance"));
}

#[test]
fn duplicate_method_names_across_interfaces_are_scoped() {
    let spec = ContractSpec {
        interfaces: vec![
            ContractSpecInterface {
                name: spec_string("ERC20"),
                vers: spec_string("0.1.0"),
                throws: vec![].try_into().unwrap(),
                methods: vec![ContractSpecMethod {
                    name: spec_string("initialize"),
                    args: vec![ContractField {
                        name: spec_string("administrator"),
                        ty: ContractType::Address,
                    }]
                    .try_into()
                    .unwrap(),
                    return_type: ContractType::Void,
                    effect: None,
                }]
                .try_into()
                .unwrap(),
            },
            ContractSpecInterface {
                name: spec_string("ERC20Metadata"),
                vers: spec_string("0.1.0"),
                throws: vec![].try_into().unwrap(),
                methods: vec![ContractSpecMethod {
                    name: spec_string("initialize"),
                    args: vec![ContractField {
                        name: spec_string("name"),
                        ty: ContractType::String,
                    }]
                    .try_into()
                    .unwrap(),
                    return_type: ContractType::Void,
                    effect: None,
                }]
                .try_into()
                .unwrap(),
            },
        ]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };

    let output = generate(&spec, "token.spec.xdr").unwrap();
    assert!(output.contains("export const ERC20 = {"));
    assert!(output.contains("export const ERC20Metadata = {"));
    assert!(output.contains("    initialize: __gh_client_contract.defineContractMethod<"));
    assert!(output.contains("entrypoint: __gh_entrypoints.ERC20.v0_1.initialize,"));
    assert!(output.contains("entrypoint: __gh_entrypoints.ERC20Metadata.v0_1.initialize,"));
    assert!(output.contains("type: spec.interfaces[0]!.methods[0]!.args[0]!.ty"));
    assert!(output.contains("type: spec.interfaces[1]!.methods[0]!.args[0]!.ty"));
}

#[test]
fn interfaces_nest_under_their_name_and_major() {
    // Its name and major are what an interface's entrypoints hash, so one
    // interface at two majors keeps both apart under one name.
    let spec = spec_with_interfaces(vec![
        interface("Token", "0.4.2", "balance"),
        interface("Token", "1.3.0", "balance"),
        interface("Token", "0.0.7", "balance"),
    ]);
    let output = generate(&spec, "token.spec.xdr").unwrap();
    for expected in [
        "const __gh_entrypoints = {\n  Token: {\n    v0_4: {\n",
        "export namespace Token {\n\n  export namespace v0_0_7 {\n\n    export type \
         BalanceArgs = void;",
        "  export namespace v0_4 {\n\n    export type BalanceArgs = void;",
        "  export namespace v1 {\n\n    export type BalanceArgs = void;",
        "export const Token = {\n  v0_4: {\n    balance:",
        "  v1: {\n    balance:",
        "  v0_0_7: {\n    balance:",
        "      Token.v1.BalanceResult,",
        "      entrypoint: __gh_entrypoints.Token.v1.balance,",
        "      interfaceVersion: \"1.3.0\",",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
    assert_eq!(
        output.matches("export const Token = {").count(),
        1,
        "{output}"
    );
}

#[test]
fn an_interface_repeated_at_one_major_errors() {
    let spec = spec_with_interfaces(vec![
        interface("Token", "1.0.0", "balance"),
        interface("Token", "1.2.0", "supply"),
    ]);
    let error = generate(&spec, "token.spec.xdr").unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot generate TypeScript bindings: interface \"Token\" versions 1.0.0 and 1.2.0 \
         would both be \"Token.v1\" in the generated TypeScript"
    );
}

#[test]
fn an_interface_version_must_be_semantic() {
    let spec = spec_with_interfaces(vec![interface("Token", "1", "balance")]);
    let error = generate(&spec, "token.spec.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interface \"Token\" has version \"1\", which is not a semantic version"),
        "{error}"
    );
}

#[test]
fn an_interface_named_after_a_crate_errors() {
    let mut spec = spec_with_interfaces(vec![interface("demo", "0.1.0", "get")]);
    spec.type_defs = vec![struct_at(spec_id("Thing"))].try_into().unwrap();
    let error = generate(&spec, "x.spec.xdr").unwrap_err();
    assert!(
        error.to_string().contains(
            "generated TypeScript value name \"demo\" collides between spec id namespace \
             \"demo\" and interface \"demo\""
        ),
        "{error}"
    );
}

#[test]
fn method_names_sharing_a_stem_error() {
    let mut iface = interface("Token", "1.0.0", "balance");
    iface.methods.0.push(ContractSpecMethod {
        name: spec_string("Balance"),
        args: vec![].try_into().unwrap(),
        return_type: ContractType::U64,
        effect: None,
    });
    let error = generate(&spec_with_interfaces(vec![iface]), "x.spec.xdr").unwrap_err();
    assert!(
        error.to_string().contains(
            "interface \"Token\" methods \"balance\" and \"Balance\" would both declare \
             \"BalanceArgs\" and \"BalanceResult\""
        ),
        "{error}"
    );
}

#[test]
fn roots_named_after_top_level_bindings_are_escaped() {
    // A crate may be named `spec`; its root is escaped, and so is every
    // path through it, types and values alike.
    let mut spec = spec_with_events();
    spec.type_defs = vec![struct_at(spec_id_at("spec", "spec", "Thing"))]
        .try_into()
        .unwrap();
    spec.events.0[0].name = spec_id_at("decodeEvent", "decodeEvent", "Ping");
    let output = generate(&spec, "x.spec.xdr").unwrap();
    for expected in [
        "export namespace $spec {\n\n  export namespace v1 {\n\n    export interface Thing",
        "export namespace $decodeEvent {",
        "export const $decodeEvent = {\n  v1: {\n    Ping: ",
        "      $decodeEvent.v1.Ping[\"topics\"],",
        "export type CounterEvent = $decodeEvent.v1.Ping;",
        "  return $decodeEvent.v1.Ping.decode(__gh_event);",
        "  export type $spec$v1$Thing = $spec.v1.Thing;",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
}

#[test]
fn the_decoder_reaches_a_crate_named_after_its_parameter() {
    // Inside `decodeEvent`, a parameter named `event` would capture a crate
    // of that name; the generator names its parameter itself.
    let mut spec = spec_with_events();
    spec.events.0[0].name = spec_id_at("event", "event", "Ping");
    let output = generate(&spec, "x.spec.xdr").unwrap();
    assert!(
        output.contains("  return event.v1.Ping.decode(__gh_event);"),
        "{output}"
    );
}

#[test]
fn method_arg_name_public_collision_errors() {
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("set_owner"),
            args: vec![
                ContractField {
                    name: spec_string("owner_id"),
                    ty: ContractType::Address,
                },
                ContractField {
                    name: spec_string("ownerId"),
                    ty: ContractType::Address,
                },
            ]
            .try_into()
            .unwrap(),
            return_type: ContractType::Void,
            effect: None,
        }],
        false,
    );

    let err = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(err.to_string().contains("ownerId"));
}

#[test]
fn interface_may_take_a_runtime_export_name() {
    let spec = counter_spec_with_interface_name(
        "fromHex",
        vec![ContractSpecMethod {
            name: spec_string("get"),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::U64,
            effect: None,
        }],
        false,
    );

    let output = generate(&spec, "counter.spec.xdr").unwrap();
    assert!(output.contains("export const fromHex = {"), "{output}");
    assert!(
        output.contains("__gh_protocol_utils.fromHex(\""),
        "{output}"
    );
}

#[test]
fn interfaces_named_after_top_level_bindings_are_escaped() {
    for name in ["spec", "decodeEvent", "globalThis"] {
        let spec = counter_spec_with_interface_name(name, vec![], true);
        let output = generate(&spec, "counter.spec.xdr").unwrap();
        for expected in [
            format!("export namespace ${name} {{"),
            format!("export const ${name} = {{"),
            format!("export type {name}Event = demo.v1.PingEvent;"),
            "export const spec: __gh_client_contract.ContractSpec = {".to_owned(),
            "export function decodeEvent(".to_owned(),
        ] {
            assert!(
                output.contains(&expected),
                "{name}: missing {expected}:\n{output}"
            );
        }
    }
}

#[test]
fn reaches_the_global_uint8_array_past_a_type_of_that_name() {
    let mut spec = counter_spec();
    spec.type_defs = vec![ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id("Uint8Array"),
            fields: vec![ContractField {
                name: spec_string("bytes"),
                ty: ContractType::Bytes,
            }]
            .try_into()
            .unwrap(),
        },
    }]
    .try_into()
    .unwrap();

    // The declaration lands at `demo.v1.Uint8Array`, in the very namespace
    // whose `bytes` fields are typed `Uint8Array`, so it would capture the
    // global. `globalThis` reaches past it, and no import is needed.
    let output = generate(&spec, "counter.spec.xdr").unwrap();
    assert!(
        output.contains(r#""bytes": globalThis.Uint8Array;"#),
        "field must reach the global:\n{output}"
    );
    assert!(
        output.contains("    export interface Uint8Array"),
        "spec type must still be declared:\n{output}"
    );
}

#[test]
fn reaches_the_global_record_past_an_event_of_that_name() {
    let mut spec = spec_with_events();
    let mut event = ping_event_def();
    event.name = spec_id("Record");
    event.topics.0[0].ty = ContractType::String;
    spec.events = vec![event].try_into().unwrap();

    // A topic-only event's filter aliases `Record<string, never>` in the
    // same namespace as the event interface, so an event named `Record`
    // would leave the filter referring to a non-generic interface.
    let output = generate(&spec, "events.spec.xdr").unwrap();
    assert!(
        output.contains("export type RecordFilter = globalThis.Record<string, never>;"),
        "filter must reach the global:\n{output}"
    );
}

#[test]
fn embeds_a_type_declared_in_another_crate() {
    let output = generate(&cross_crate_spec("demo"), "x.spec.xdr").unwrap();

    // The reference goes through `__gh_codegen`, whose alias names the
    // other crate from the top level, where its root resolves to itself.
    assert!(
        output.contains(r#"export interface Holder { "inner": __gh_codegen.other$v1$Thing; }"#),
        "cross-crate reference missing:\n{output}"
    );
    assert!(output.contains("export namespace other {"));
    assert!(
        output.contains("  export type other$v1$Thing = other.v1.Thing;\n"),
        "{output}"
    );
}

#[test]
fn a_module_named_after_a_crate_leaves_it_reachable() {
    // `Holder` sits in `demo::other`, where `other.v1.Thing` would resolve
    // to its own enclosing module; the alias is written outside it.
    let output = generate(&cross_crate_spec("demo::other"), "x.spec.xdr").unwrap();
    assert!(
        output.contains(
            "    export namespace other {\n\n      export interface Holder { \"inner\": \
             __gh_codegen.other$v1$Thing; }"
        ),
        "{output}"
    );
}

#[test]
fn a_type_named_after_a_crate_leaves_it_reachable() {
    // A crate is only named as the first segment of a qualified reference,
    // which TypeScript resolves to a namespace, never to a type.
    let mut spec = cross_crate_spec("demo");
    spec.type_defs
        .0
        .push(struct_at(spec_id_at("demo", "demo", "other")));
    let output = generate(&spec, "x.spec.xdr").unwrap();
    assert!(
        output.contains(r#"export interface Holder { "inner": __gh_codegen.other$v1$Thing; }"#),
        "{output}"
    );
}

#[test]
fn event_descriptors_read_the_generated_bindings_from_the_top_level() {
    // An event in module `spec` and an event named `codecs` become keys in
    // the value object, which bind nothing the descriptors read.
    let mut spec = spec_with_events();
    let mut in_spec_module = ping_event_def();
    in_spec_module.name = spec_id_at("demo", "demo::spec", "PingEvent");
    let mut named_codecs = ping_event_def();
    named_codecs.name = spec_id("codecs");
    named_codecs.signature = [0xCD; 32].into();
    spec.events = vec![in_spec_module, named_codecs].try_into().unwrap();

    let output = generate(&spec, "events.spec.xdr").unwrap();
    for expected in [
        "signature: spec.events[0]!.signature,",
        "type: spec.events[1]!.fields[0]!.ty",
        "      codecs: __gh_codecs,\n",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
    // The descriptors are built with the object, after the codec context.
    let at = |needle: &str| {
        output
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle}:\n{output}"))
    };
    assert!(at("const __gh_codecs = ") < at("export const demo = {"));
}

#[test]
fn reserved_prefix_is_rejected_wherever_a_spec_name_lands() {
    // An interface object sits at top level beside the runtime imports.
    let spec = counter_spec_with_interface_name("__gh_client_contract", vec![], false);
    let error = generate(&spec, "x.spec.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("\"__gh_client_contract\" for interface \"__gh_client_contract\""),
        "{error}"
    );

    // So does a crate root; a module segment is nested beside the others.
    for id in [
        spec_id_at("__gh_x", "__gh_x", "Thing"),
        spec_id_at("demo", "demo::__gh_x", "Thing"),
    ] {
        let mut spec = counter_spec();
        spec.type_defs = vec![struct_at(id)].try_into().unwrap();
        let error = generate(&spec, "x.spec.xdr").unwrap_err();
        assert!(
            error.to_string().contains("starts with \"__gh_\""),
            "{error}"
        );
    }
}

#[test]
fn global_this_is_escaped_wherever_a_spec_name_lands() {
    // Built-in types are written `globalThis.X`, which a module or type of
    // that name would capture, still compiling with the wrong type.
    let mut spec = counter_spec();
    spec.type_defs = vec![
        ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_id_at("demo", "demo::globalThis", "Uint8Array"),
                fields: vec![ContractField {
                    name: spec_string("bytes"),
                    ty: ContractType::Bytes,
                }]
                .try_into()
                .unwrap(),
            },
        },
        struct_at(spec_id_at("demo", "demo", "globalThis")),
        struct_at(spec_id_at("globalThis", "globalThis", "Thing")),
    ]
    .try_into()
    .unwrap();
    let output = generate(&spec, "x.spec.xdr").unwrap();
    for expected in [
        "    export namespace $globalThis {\n\n      export interface Uint8Array { \"bytes\": \
         globalThis.Uint8Array; }",
        "    export interface $globalThis { \"x\": bigint; }",
        "export namespace $globalThis {\n\n  export namespace v1 {",
        "  export type $globalThis$v1$Thing = $globalThis.v1.Thing;",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
}

#[test]
fn declarations_merge_with_a_module_of_their_name() {
    // Each declares a type there, which merges with any namespace.
    let foo = || spec_id_at("demo", "demo", "foo");
    let bar = || spec_id_at("demo", "demo::foo", "Bar");
    for (label, outer, inner) in [
        ("struct", struct_at(foo()), enum_at(bar())),
        ("union", union_at(foo()), enum_at(bar())),
        ("enum", enum_at(foo()), struct_at(bar())),
    ] {
        let output = generate(&beside_a_module_holding(outer, inner), "x.spec.xdr")
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        assert!(
            output.contains("    export namespace foo {"),
            "{label}:\n{output}"
        );
    }
}

#[test]
fn an_enum_shares_its_object_with_a_module_of_its_name() {
    // `demo.v1.foo` holds the enum's variants and the module's values, at
    // any depth, whether the module holds an enum or an event.
    for module in ["demo::foo", "demo::foo::deeper"] {
        let spec = beside_a_module_holding(
            enum_at(spec_id_at("demo", "demo", "foo")),
            enum_at(spec_id_at("demo", module, "Bar")),
        );
        generate(&spec, "x.spec.xdr").unwrap_or_else(|error| panic!("{module}: {error}"));
    }
    let spec = beside_a_module_holding(
        enum_at(spec_id_at("demo", "demo", "foo")),
        enum_at(spec_id_at("demo", "demo::foo", "Bar")),
    );
    let output = generate(&spec, "x.spec.xdr").unwrap();
    assert!(
        output.contains("    foo: {\n      \"A\": 1n,\n      Bar: {\n"),
        "{output}"
    );
    let mut spec = spec_with_events();
    spec.events.0[0].name = spec_id_at("demo", "demo::foo", "Ping");
    spec.type_defs = vec![enum_at(spec_id_at("demo", "demo", "foo"))]
        .try_into()
        .unwrap();
    generate(&spec, "x.spec.xdr").unwrap();

    // Unless a variant takes the key a module's value needs.
    let spec = beside_a_module_holding(
        enum_at(spec_id_at("demo", "demo", "foo")),
        enum_at(spec_id_at("demo", "demo::foo", "A")),
    );
    let error = generate(&spec, "x.spec.xdr").unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot generate TypeScript bindings: enum demo::foo (demo@1) has a variant \"A\", \
         and enum demo::foo::A (demo@1) also needs \"demo.v1.foo.A\" in the generated \
         TypeScript, where an enum's variants share one object with the values of the \
         module named after it. Rename either."
    );
    // The contract builds; only its bindings cannot be generated.
    assert_eq!(error.code(), "ts_bindings_failed");
    assert!(error.hint().is_none(), "{error}");
}

#[test]
fn an_event_beside_a_module_holding_a_value_errors() {
    // The event's descriptor cannot hold the module's values, however deep.
    for module in ["demo::foo", "demo::foo::deeper"] {
        let mut spec = spec_with_events();
        spec.events.0[0].name = spec_id_at("demo", "demo", "foo");
        spec.type_defs = vec![enum_at(spec_id_at("demo", module, "Bar"))]
            .try_into()
            .unwrap();
        let error = generate(&spec, "x.spec.xdr").unwrap_err();
        assert!(
            error.to_string().contains(&format!(
                "event demo::foo (demo@1) and enum {module}::Bar (demo@1) both need a value \
                 under \"demo.v1.foo\" in the generated TypeScript: the event's descriptor \
                 is there"
            )),
            "{module}: {error}"
        );
    }
}

#[test]
fn an_invalid_path_segment_names_its_declaration() {
    let mut spec = spec_with_events();
    spec.events = vec![].try_into().unwrap();
    spec.type_defs = vec![struct_at(spec_id_at("demo", "demo::default", "Has-Dash"))]
        .try_into()
        .unwrap();
    let error = generate(&spec, "x.spec.xdr").unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot generate TypeScript bindings: demo::default::Has-Dash (demo@1) would be \
         \"demo.v1.$default.Has-Dash\" in the generated TypeScript, where \"Has-Dash\" is not \
         a valid identifier. Rename it."
    );
}

#[test]
fn reserved_words_in_a_path_are_escaped() {
    // `default` is a valid Rust module name, and a type may take any name
    // through `#[contract_type(name = "...")]`. Types and values escape
    // alike, so one path still names both.
    let mut spec = counter_spec();
    spec.type_defs = vec![
        enum_at(spec_id_at("demo", "demo::default", "Kind")),
        struct_at(spec_id_at("demo", "demo", "string")),
        struct_at(spec_id_at("demo", "demo", "__proto__")),
        struct_at(spec_id_at("default", "default", "Thing")),
    ]
    .try_into()
    .unwrap();
    let output = generate(&spec, "x.spec.xdr").unwrap();
    for expected in [
        "    export namespace $default {\n\n      export type Kind = 1n;",
        "    export interface $string { \"x\": bigint; }",
        "    export interface $__proto__ { \"x\": bigint; }",
        "export namespace $default {\n\n  export namespace v1 {",
        "    $default: {\n      Kind: {\n        \"A\": 1n,",
        "  export type demo$v1$$default$Kind = demo.v1.$default.Kind;",
        "  export type demo$v1$$string = demo.v1.$string;",
        "  export type $default$v1$Thing = $default.v1.Thing;",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
}

#[test]
fn dollar_signs_in_spec_names_are_rejected() {
    // The alias and escaped names use `$`; a spec name with one could
    // collide with them.
    let mut spec = counter_spec();
    spec.type_defs = vec![struct_at(spec_id_at("demo", "demo", "A$b"))]
        .try_into()
        .unwrap();
    let error = generate(&spec, "x.spec.xdr").unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot generate TypeScript bindings: demo::A$b (demo@1) has \"$\" in its name, which \
         the generated TypeScript keeps for the names it escapes. Rename it."
    );

    let spec = counter_spec_with_interface_name("Count$r", vec![], false);
    let error = generate(&spec, "x.spec.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interface \"Count$r\" has \"$\""),
        "{error}"
    );
}

#[test]
fn two_crates_may_declare_the_same_name() {
    // The motivating case: structured spec ids let two crates each declare
    // `Error`, and one crate declare `Config` in two modules. The flat
    // scope rejected all of this; the namespaces keep them apart.
    let mut spec = spec_with_events();
    spec.events = vec![].try_into().unwrap();
    let def = |krate: &str, module: &str, declared: &str| ContractTypeDef::TypeDefStruct {
        struct_def: ContractStructDef {
            name: spec_id_at(krate, module, declared),
            fields: vec![].try_into().unwrap(),
        },
    };
    spec.type_defs = vec![
        def("dup-name-if-a", "dup_name_if_a", "Error"),
        def("dup-name-if-b", "dup_name_if_b", "Error"),
        def("spec-name-reuse", "spec_name_reuse::ledger", "Config"),
        def("spec-name-reuse", "spec_name_reuse::vault", "Config"),
    ]
    .try_into()
    .unwrap();

    let output = generate(&spec, "reuse.spec.xdr").unwrap();
    for expected in [
        "export namespace dup_name_if_a {",
        "export namespace dup_name_if_b {",
        "export namespace ledger {",
        "export namespace vault {",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
}

#[test]
fn invalid_interface_name_errors() {
    let spec = counter_spec_with_interface_name("erc20.v1", vec![], false);

    let err = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(err.to_string().contains("erc20.v1"));
}

#[test]
fn reserved_interface_names_are_escaped() {
    // The escape is the binding's own; the wire keeps the interface's name.
    for name in [
        "eval",
        "arguments",
        "any",
        "bigint",
        "infer",
        "keyof",
        "string",
        "unknown",
    ] {
        let spec = counter_spec_with_interface_name(name, counter_spec_methods(), false);
        let output = generate(&spec, "counter.spec.xdr").unwrap();
        for expected in [
            format!("export namespace ${name} {{"),
            format!("export const ${name} = {{"),
            format!("  ${name}.v0_1.GetArgs,"),
            format!("  entrypoint: __gh_entrypoints.${name}.v0_1.get,"),
            format!("  interfaceName: \"{name}\","),
        ] {
            assert!(
                output.contains(&expected),
                "{name}: missing {expected}:\n{output}"
            );
        }
    }
}

#[test]
fn reserved_property_names_are_allowed_for_methods_and_arguments() {
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("symbol"),
            args: vec![ContractField {
                name: spec_string("return"),
                ty: ContractType::String,
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::String,
            effect: None,
        }],
        false,
    );

    let output = generate(&spec, "counter.spec.xdr").unwrap();
    assert!(output.contains("export interface SymbolArgs"));
    assert!(output.contains("  return: string;"));
    assert!(output.contains("  symbol: __gh_client_contract.defineContractMethod<"));
}

#[test]
fn invalid_method_name_errors() {
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("transfer-from"),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::Void,
            effect: None,
        }],
        false,
    );

    let err = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(err.to_string().contains("transfer-from"));
}

#[test]
fn proto_method_name_errors() {
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("__proto__"),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::Void,
            effect: None,
        }],
        false,
    );

    let err = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(err.to_string().contains("__proto__"));
}

#[test]
fn invalid_method_arg_name_errors() {
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("set_owner"),
            args: vec![ContractField {
                name: spec_string("owner-id"),
                ty: ContractType::Address,
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::Void,
            effect: None,
        }],
        false,
    );

    let err = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(err.to_string().contains("owner-id"));
}

#[test]
fn effects_are_documented_and_preserved_without_changing_entrypoints() {
    use thruput_contract_spec::ContractMethodEffect as Effect;
    let mut spec = counter_spec();
    let baseline = generate(&spec, "counter.spec.xdr").unwrap();
    let entrypoints = |output: &str| {
        output
            .lines()
            .filter(|line| line.contains("contractEntrypointID("))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for (effect, label, variant) in [
        (Effect::EffectPure, "pure", "EffectPure"),
        (Effect::EffectReadOnly, "readOnly", "EffectReadOnly"),
        (
            Effect::EffectStateChanging,
            "stateChanging",
            "EffectStateChanging",
        ),
    ] {
        spec.interfaces.0[0].methods.0[0].effect = Some(effect);
        let output = generate(&spec, "counter.spec.xdr").unwrap();
        assert!(output.contains(&format!(
            "Declared effect: {label}. Author declaration; not compiler/runtime enforced."
        )));
        assert!(output.contains(&format!("ContractMethodEffect.{variant}")));
        assert!(output.contains("effect: spec.interfaces[0]!.methods[0]!.effect"));
        assert_eq!(entrypoints(&output), entrypoints(&baseline));
    }
    assert!(baseline.contains("effect: null,"));
    assert!(!baseline.contains("Declared effect:"));
    spec.interfaces.0[0].methods.0[0].effect = Some(Effect(99));
    assert!(
        generate(&spec, "counter.spec.xdr")
            .unwrap_err()
            .to_string()
            .contains("unsupported method effect")
    );
}

#[test]
fn every_binding_imports_every_runtime_module_whole() {
    let imports = "\n\
        import * as __gh_protocol from \"@thruputchain/protocol\";\n\
        import * as __gh_protocol_utils from \"@thruputchain/protocol/utils\";\n\
        import * as __gh_contract_spec from \"@thruputchain/contract-spec\";\n\
        import * as __gh_client_contract from \"@thruputchain/client/contract\";\n";
    // Methods, events, and neither: the last uses no protocol module at all.
    let bare = counter_spec_with_interface_name("Counter", vec![], false);
    for spec in [counter_spec(), spec_with_events(), bare] {
        let output = generate(&spec, "x.spec.xdr").unwrap();
        assert!(output.contains(imports), "{output}");
        let lines = output.lines().filter(|line| line.starts_with("import "));
        assert_eq!(lines.count(), imports.trim().lines().count(), "{output}");
    }
}

#[test]
fn generates_full_binding_for_simple_contract() {
    let spec = counter_spec();
    let output = generate(&spec, "counter.spec.xdr").unwrap();

    // Header
    assert!(output.contains("DO NOT EDIT"));
    assert!(output.contains("counter.spec.xdr"));
    assert!(output.contains("Counter interface v0.1.0"));

    // Entrypoints block, keyed by interface name and major
    assert!(output.contains("const __gh_entrypoints = {\n  Counter: {\n    v0_1: {\n"));
    assert!(output.contains("} as const;"));

    // Spec literal
    assert!(output.contains("export const spec: __gh_client_contract.ContractSpec"));

    assert!(output.contains(
        "const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);"
    ));

    // Types and methods sit under the interface's name and major.
    assert!(output.contains("export namespace Counter {\n\n  export namespace v0_1 {\n"));
    assert!(output.contains("export const Counter = {\n  v0_1: {\n"));
    assert!(output.contains("    export type GetArgs = void;"));
    assert!(output.contains("    export type GetResult = bigint;"));
    assert!(output.contains("    export interface IncrementArgs"));
    assert!(output.contains("amount: bigint"));
    assert!(output.contains("    export type IncrementResult = void;"));
    assert!(output.contains("    export type DeclaredException = never;"));
    assert!(output.contains(
        "    export type Exception = \
         __gh_client_contract.ContractException<DeclaredException>;"
    ));
    assert!(output.contains("    get: __gh_client_contract.defineContractMethod<"));
    assert!(output.contains("      Counter.v0_1.GetArgs,"));
    assert!(output.contains("    increment: __gh_client_contract.defineContractMethod<"));
    assert!(output.contains("      entrypoint: __gh_entrypoints.Counter.v0_1.increment,"));
    assert!(output.contains("interfaceName: \"Counter\""));
    assert!(output.contains("interfaceVersion: \"0.1.0\""));
    assert!(output.contains("abiName: \"increment\""));
    assert!(output.contains("publicName: \"increment\""));
    assert!(output.contains("returnType: spec.interfaces[0]!.methods[0]!.return_type"));
    assert!(output.contains("returnType: spec.interfaces[0]!.methods[1]!.return_type"));
    assert!(!output.contains("encodeCalldata"));
    assert!(!output.contains("createInvocation"));

    // Braces balanced
    let opens = output.chars().filter(|&c| c == '{').count();
    let closes = output.chars().filter(|&c| c == '}').count();
    assert_eq!(
        opens, closes,
        "braces must be balanced: {opens} opens vs {closes} closes"
    );
}

#[test]
fn generates_erc20_bindings() {
    let spec = erc20_spec();
    let output = generate(&spec, "erc20.spec.xdr").unwrap();

    // Entrypoint hashes
    assert!(
        output.contains("63285b3523f9dc3f"),
        "transfer entrypoint hash missing"
    );
    assert!(
        output.contains("b921b642d31d7dac"),
        "get_balance entrypoint hash missing"
    );
    assert!(
        output.contains("41ac95d6e06e2620"),
        "get_balance_of entrypoint hash missing"
    );

    assert!(
        output.contains("    ERC20Error: {"),
        "ERC20Error enum values missing"
    );
    assert!(
        output.contains("\"AlreadyInitialized\": 1n"),
        "AlreadyInitialized variant missing"
    );
    assert!(
        output.contains("\"Internal\": 100n"),
        "Internal variant missing"
    );
    assert!(output.contains("export type ERC20Error = 1n | 2n | 3n | 100n;"));

    // Interface object methods
    assert!(
        output.contains("export const ERC20 = {"),
        "ERC20 interface object missing"
    );
    assert!(
        output.contains("  transfer: __gh_client_contract.defineContractMethod<"),
        "transfer method missing"
    );
    assert!(
        output.contains("  getBalance: __gh_client_contract.defineContractMethod<"),
        "getBalance method missing"
    );
    assert!(
        output.contains("  getBalanceOf: __gh_client_contract.defineContractMethod<"),
        "getBalanceOf method missing"
    );
    assert!(
        !output.contains("export function get_balance("),
        "get_balance method should be camelCase"
    );
    assert!(
        !output.contains("export function get_balance_of("),
        "get_balance_of method should be camelCase"
    );

    assert!(
        output.contains("export interface TransferArgs")
            && output.contains("to: __gh_protocol.ContractAddress")
            && output.contains("amount: bigint"),
        "transfer 'to' arg type mismatch"
    );
    assert!(
        output.contains("export interface GetBalanceOfArgs")
            && output.contains("accountId: __gh_protocol.ContractAddress"),
        "getBalanceOf 'accountId' arg type mismatch"
    );
    assert!(
        output.contains("entrypoint: __gh_entrypoints.ERC20.v0_1.getBalanceOf"),
        "getBalanceOf should read the nested camelCase entrypoint key"
    );
    assert!(
        output.contains("\"get_balance_of\""),
        "source method name should be preserved in the spec"
    );
    assert!(
        output.contains("name: __gh_s(\"account_id\")"),
        "source arg name should be preserved in the spec literal"
    );
    assert!(output.contains("abiName: \"account_id\""));
    assert!(output.contains("publicName: \"accountId\""));
    assert!(output.contains("export type DeclaredException ="));
    assert!(output.contains("name: \"ERC20Error\";"));
    assert!(output.contains("value: __gh_codegen.demo$v1$ERC20Error;"));
    assert!(output.contains(
        "export type Exception = __gh_client_contract.ContractException<DeclaredException>;"
    ));
    assert!(output.contains("type: spec.interfaces[0]!.throws[0]!.ty"));

    // Spec literal contains exception selector
    assert!(
        output.contains("selector: 0xb19947acb2fa14ffn"),
        "exception selector missing from spec literal"
    );

    // Braces balanced
    let opens = output.chars().filter(|&c| c == '{').count();
    let closes = output.chars().filter(|&c| c == '}').count();
    assert_eq!(
        opens, closes,
        "braces must be balanced: {opens} opens vs {closes} closes"
    );
}

#[test]
fn generates_recursive_type_bindings() {
    let spec = recursive_type_spec();
    let output = generate(&spec, "recursive.spec.xdr").unwrap();

    // Tree struct emitted as TypeScript interface
    assert!(
        output.contains("export interface Tree"),
        "Tree interface declaration missing"
    );
    assert!(
        output.contains("\"value\": bigint;"),
        "Tree.value field missing"
    );
    assert!(
        output.contains("\"left\": __gh_codegen.demo$v1$Tree | null;"),
        "Tree.left field missing"
    );
    assert!(
        output.contains("\"right\": __gh_codegen.demo$v1$Tree | null;"),
        "Tree.right field missing"
    );

    assert!(
        output.contains("export interface DepthArgs")
            && output.contains("tree: __gh_codegen.demo$v1$Tree"),
        "depth 'tree' arg type mismatch"
    );
}

#[test]
fn generates_signed_bigint_enum_values() {
    let def = ContractTypeDef::TypeDefEnum {
        enum_def: ContractEnumDef {
            name: spec_id("SignedValues"),
            variants: vec![
                ContractEnumVariant {
                    name: spec_string("Negative"),
                    value: -2,
                },
                ContractEnumVariant {
                    name: spec_string("Zero"),
                    value: 0,
                },
            ]
            .try_into()
            .unwrap(),
        },
    };

    let mut spec = counter_spec();
    spec.type_defs = vec![def].try_into().unwrap();
    let output = generate(&spec, "x.spec.xdr").unwrap();
    assert!(
        output.contains("export type SignedValues = -2n | 0n;"),
        "{output}"
    );
    assert!(output.contains("\"Negative\": -2n,"), "{output}");
    assert!(output.contains("\"Zero\": 0n,"), "{output}");
}

#[test]
fn duplicate_enum_values_are_rejected() {
    let def = ContractTypeDef::TypeDefEnum {
        enum_def: ContractEnumDef {
            name: spec_id("DuplicateValues"),
            variants: vec![
                ContractEnumVariant {
                    name: spec_string("First"),
                    value: 1,
                },
                ContractEnumVariant {
                    name: spec_string("Second"),
                    value: 1,
                },
            ]
            .try_into()
            .unwrap(),
        },
    };
    let spec = ContractSpec {
        interfaces: vec![].try_into().unwrap(),
        type_defs: vec![def].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };

    let error = generate(&spec, "duplicate-enum.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("enum demo::DuplicateValues (demo@1) has duplicate value 1")
    );
}

#[test]
fn duplicate_union_discriminants_are_rejected() {
    let def = ContractTypeDef::TypeDefUnion {
        union_def: ContractUnionDef {
            name: spec_id("DuplicateDiscriminants"),
            cases: vec![
                ContractUnionCase {
                    name: spec_string("First"),
                    discriminant: -1,
                    case_type: ContractUnionCaseType::UnionCaseVoid,
                },
                ContractUnionCase {
                    name: spec_string("Second"),
                    discriminant: -1,
                    case_type: ContractUnionCaseType::UnionCaseVoid,
                },
            ]
            .try_into()
            .unwrap(),
        },
    };
    let spec = ContractSpec {
        interfaces: vec![].try_into().unwrap(),
        type_defs: vec![def].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };

    let error = generate(&spec, "duplicate-union.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("union demo::DuplicateDiscriminants (demo@1) has duplicate discriminant -1")
    );
}

#[test]
fn empty_union_is_rejected() {
    let def = ContractTypeDef::TypeDefUnion {
        union_def: ContractUnionDef {
            name: spec_id("Empty"),
            cases: vec![].try_into().unwrap(),
        },
    };
    let spec = ContractSpec {
        interfaces: vec![].try_into().unwrap(),
        type_defs: vec![def].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };

    let error = generate(&spec, "empty-union.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("union demo::Empty (demo@1) must declare at least one case")
    );
}

#[test]
fn generates_direct_topic_types_filters_and_metadata() {
    let mut spec = spec_with_events();
    spec.events = vec![ContractEventDef {
        name: spec_id("Changed"),
        signature: [0xCD; 32].into(),
        topics: vec![
            ContractField {
                name: spec_string("owner-address"),
                ty: ContractType::Address,
            },
            ContractField {
                name: spec_string("id"),
                ty: ContractType::Array {
                    array: ContractArrayType {
                        element: Box::new(ContractType::U8),
                        len: 32,
                    },
                },
            },
        ]
        .try_into()
        .unwrap(),
        fields: vec![ContractField {
            name: spec_string("new-value"),
            ty: ContractType::U64,
        }]
        .try_into()
        .unwrap(),
    }]
    .try_into()
    .unwrap();

    let output = generate(&spec, "events.spec.xdr").unwrap();
    assert!(output.contains("\"owner-address\": __gh_protocol.ContractAddress"));
    assert!(output.contains("\"id\": __gh_client_contract.FixedBytes<32>"));
    assert!(output.contains(
        "\"owner-address\"?: \
         __gh_client_contract.ContractEventFilterValue<__gh_protocol.ContractAddress>"
    ));
    assert!(output.contains(
        "\"id\"?: \
         __gh_client_contract.ContractEventFilterValue<__gh_client_contract.FixedBytes<32>>"
    ));
    assert!(output.contains("representation: \"address\""));
    assert!(output.contains("representation: \"fixedBytes32\""));
    assert!(output.contains("__gh_client_contract.defineContractEvent<"));
}

#[test]
fn generates_direct_native_256_topic_metadata() {
    let mut spec = spec_with_events();
    spec.events.0[0].topics = vec![
        ContractField {
            name: spec_string("unsigned"),
            ty: ContractType::U256,
        },
        ContractField {
            name: spec_string("signed"),
            ty: ContractType::I256,
        },
    ]
    .try_into()
    .unwrap();

    let output = generate(&spec, "events.spec.xdr").unwrap();

    assert!(output.contains("\"unsigned\": bigint"));
    assert!(output.contains("\"signed\": bigint"));
    assert!(
        output.contains("\"unsigned\"?: __gh_client_contract.ContractEventFilterValue<bigint>")
    );
    assert!(output.contains("\"signed\"?: __gh_client_contract.ContractEventFilterValue<bigint>"));
    assert!(output.contains("representation: \"u256\""));
    assert!(output.contains("representation: \"i256\""));
    assert!(output.contains("{ v: __gh_contract_spec.ContractTypeKind.U256 }"));
    assert!(output.contains("{ v: __gh_contract_spec.ContractTypeKind.I256 }"));
}

#[test]
fn generates_direct_scalar_topic_types_and_filters() {
    for (ty, representation, ts_type) in [
        (ContractType::U32, "u32", "bigint"),
        (ContractType::U64, "u64", "bigint"),
        (ContractType::U128, "u128", "bigint"),
        (ContractType::I32, "i32", "bigint"),
        (ContractType::I64, "i64", "bigint"),
        (ContractType::I128, "i128", "bigint"),
        (ContractType::Bool, "bool", "boolean"),
    ] {
        let mut spec = spec_with_events();
        spec.events.0[0].topics.0[0].ty = ty;
        let output = generate(&spec, "scalar.spec.xdr").unwrap();
        assert!(output.contains(&format!("\"tag\": {ts_type}")));
        assert!(output.contains(&format!(
            "\"tag\"?: __gh_client_contract.ContractEventFilterValue<{ts_type}>"
        )));
        assert!(output.contains(&format!("representation: \"{representation}\"")));
        assert!(!output.contains("original value cannot be recovered"));
    }
}

#[test]
fn documents_hashed_string_and_bytes_topics() {
    for ty in [ContractType::String, ContractType::Bytes] {
        let mut spec = spec_with_events();
        spec.events.0[0].topics.0[0].ty = ty;
        let output = generate(&spec, "hashed.spec.xdr").unwrap();
        assert!(output.contains("\"tag\": globalThis.Uint8Array"));
        assert!(output.contains("original value cannot be recovered"));
        assert!(output.contains("representation: \"hash\""));
        assert!(output.contains("export type PingEventFilter = globalThis.Record<string, never>;"));
    }
}

#[test]
fn duplicate_event_signatures_are_rejected() {
    let mut first = spec_with_events().events[0].clone();
    first.name = spec_id("First");
    let mut second = first.clone();
    second.name = spec_id("Second");
    let spec = ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("Counter"),
            vers: spec_string("0.1.0"),
            throws: vec![].try_into().unwrap(),
            methods: vec![].try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![first, second].try_into().unwrap(),
    };

    let error = generate(&spec, "events.spec.xdr").unwrap_err();
    assert!(error.to_string().contains(
        "event demo::First (demo@1) and event demo::Second (demo@1) have duplicate signature \
         0xabababababababababababababababababababababababababababababababab"
    ));
}

#[test]
fn duplicate_exception_selectors_report_interface_context() {
    let spec = ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: spec_string("ERC20"),
            vers: spec_string("0.1.0"),
            throws: vec![
                ContractExceptionType {
                    selector: 7,
                    ty: ContractType::U32,
                },
                ContractExceptionType {
                    selector: 7,
                    ty: ContractType::U64,
                },
            ]
            .try_into()
            .unwrap(),
            methods: vec![].try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };

    let error = generate(&spec, "erc20.spec.xdr").unwrap_err();
    assert!(error.to_string().contains("interface \"ERC20\""));
    assert!(
        error
            .to_string()
            .contains("duplicate exception selector 0x7")
    );
}

#[test]
fn unresolved_named_type_reports_method_context() {
    let spec = counter_spec_with_methods(
        vec![ContractSpecMethod {
            name: spec_string("set"),
            args: vec![ContractField {
                name: spec_string("value"),
                ty: ContractType::Named {
                    named: spec_id("Missing"),
                },
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::Void,
            effect: None,
        }],
        false,
    );

    let error = generate(&spec, "counter.spec.xdr").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("method \"Counter.set\" argument \"value\"")
    );
    assert!(
        error
            .to_string()
            .contains("unresolved named type \"demo::Missing (demo@1)\"")
    );
}

#[test]
fn generation_is_deterministic() {
    let spec = erc20_spec();
    let first = generate(&spec, "erc20.spec.xdr").unwrap();
    let second = generate(&spec, "erc20.spec.xdr").unwrap();
    assert_eq!(first, second);
}
