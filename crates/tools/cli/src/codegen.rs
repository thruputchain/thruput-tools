//! TypeScript binding code generation from contract specs.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use groundhog_contract_spec::{
    ContractEnumDef, ContractEventDef, ContractExceptionType, ContractField, ContractSpec,
    ContractSpecInterface, ContractSpecMethod, ContractStructDef, ContractType, ContractTypeDef,
    ContractUnionCaseType, ContractUnionDef,
};
use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};

use crate::CliError;
use crate::codec::spec_string_text;

/// Map a contract type to its TypeScript type reference string.
fn ts_type_ref(ty: &ContractType) -> Result<String, CliError> {
    match ty {
        ContractType::Void => Ok("void".into()),
        ContractType::Bool => Ok("boolean".into()),
        ContractType::U8 => Err(CliError::SpecFailed(
            "bare U8 is not a valid contract type — U8 must be wrapped in Vector or Array".into(),
        )),
        ContractType::U32
        | ContractType::I32
        | ContractType::U64
        | ContractType::I64
        | ContractType::U128
        | ContractType::I128 => Ok("bigint".into()),
        ContractType::String => Ok("string".into()),
        ContractType::Bytes => Ok("Uint8Array".into()),
        ContractType::Address => Ok("ContractAddress".into()),
        ContractType::Option { option_inner } => {
            Ok(format!("{} | null", ts_type_ref(option_inner.as_ref())?))
        }
        ContractType::Vector { vector } => {
            let element = vector.element.as_ref();
            if matches!(element, ContractType::U8) {
                return Ok("Uint8Array".into());
            }
            let inner = ts_type_ref(element)?;
            if inner.contains('|') {
                Ok(format!("({inner})[]"))
            } else {
                Ok(format!("{inner}[]"))
            }
        }
        ContractType::Array { array } => {
            let element = array.element.as_ref();
            if matches!(element, ContractType::U8) {
                return Ok("Uint8Array".into());
            }
            let inner = ts_type_ref(element)?;
            if inner.contains('|') {
                Ok(format!("({inner})[]"))
            } else {
                Ok(format!("{inner}[]"))
            }
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

// ---------------------------------------------------------------------------
// Full binding file generation
// ---------------------------------------------------------------------------

/// Generate a complete, self-contained TypeScript binding file from a
/// contract spec.
///
/// `source_name` is the human-readable origin (e.g. a WASM file name)
/// embedded in the header comment.
pub fn generate_ts_bindings(spec: &ContractSpec, source_name: &str) -> Result<String, CliError> {
    let mut out = String::new();

    // 1. Header
    let interfaces: Vec<(&str, &str)> = spec
        .interfaces
        .iter()
        .map(|iface| {
            Ok((
                spec_string_text(&iface.name)?,
                spec_string_text(&iface.vers)?,
            ))
        })
        .collect::<Result<_, CliError>>()?;
    validate_public_name_collisions(spec)?;
    emit_header(&mut out, source_name, &interfaces);

    // 2. Imports
    emit_imports(&mut out, spec);

    // 3. Entrypoints
    emit_entrypoints(&mut out, spec)?;

    // 4. Named type declarations
    for def in spec.type_defs.iter() {
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", emit_type_def(def)?);
    }

    // 5. Spec literal
    let _ = writeln!(out);
    emit_spec_literal(&mut out, spec)?;

    // 6. Named codecs
    let _ = writeln!(out);
    let _ = writeln!(out, "const namedCodecs = buildNamedCodecs(spec.type_defs);");

    // 7. Per-method functions
    for iface in spec.interfaces.iter() {
        let iface_name = spec_string_text(&iface.name)?;
        for method in iface.methods.iter() {
            let _ = writeln!(out);
            emit_method_function(&mut out, iface_name, method)?;
        }
    }

    // 8. Event types and decoders
    if !spec.events.is_empty() {
        emit_event_sections(&mut out, spec)?;
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Section emitters
// ---------------------------------------------------------------------------

/// Emit the file header comment with source name and interface info.
fn emit_header(out: &mut String, source_name: &str, interfaces: &[(&str, &str)]) {
    for (name, vers) in interfaces {
        let _ = writeln!(
            out,
            "// Generated from {source_name} \u{2014} {name} interface v{vers}"
        );
    }
    let _ = writeln!(out, "// DO NOT EDIT. Re-run codegen to update.");
}

/// Emit the TypeScript import block.
fn emit_imports(out: &mut String, spec: &ContractSpec) {
    let _ = writeln!(out);

    // Every generated method function takes `ContractAddress`, so always
    // import it alongside the other protocol types.
    let _ = writeln!(
        out,
        "import type {{ ContractAddress, GasAmount, Invocation }} \
         from \"@groundhogchain/protocol\";",
    );
    let _ = writeln!(
        out,
        "import {{ contractEntrypointID }} from \
         \"@groundhogchain/protocol\";"
    );
    let _ = writeln!(
        out,
        "import {{ fromHex }} from \
         \"@groundhogchain/protocol/utils\";"
    );

    // @groundhogchain/contract-spec imports
    let has_unions = spec
        .type_defs
        .iter()
        .any(|d| matches!(d, ContractTypeDef::TypeDefUnion { .. }));
    let has_events = !spec.events.is_empty();
    let mut cs_imports: Vec<&str> = vec![
        "ContractTypeKind",
        "ContractTypeDefKind",
        "contractSpecString",
    ];
    if has_unions {
        cs_imports.push("ContractUnionCaseKind");
    }
    if has_events {
        cs_imports.push("topic");
    }
    let _ = writeln!(
        out,
        "import {{ {} }} from \"@groundhogchain/contract-spec\";",
        cs_imports.join(", "),
    );

    // @groundhogchain/client imports
    let _ = writeln!(
        out,
        "import type {{ ContractSpec }} from \"@groundhogchain/client\";"
    );
    let _ = writeln!(
        out,
        "import {{ encodeCalldata }} from \"@groundhogchain/client\";"
    );
    let _ = writeln!(
        out,
        "import {{ buildNamedCodecs }} from \"@groundhogchain/client\";"
    );
    let _ = writeln!(
        out,
        "import {{ createInvocation }} from \"@groundhogchain/client\";"
    );

    // Event decoder imports
    if has_events {
        let _ = writeln!(
            out,
            "import {{ resolveCodec }} from \"@groundhogchain/client\";"
        );
        let _ = writeln!(out, "import {{ CxsReader }} from \"@groundhogchain/xdr\";");
    }
}

/// Emit the `entrypoints` const mapping method names to their IDs.
fn emit_entrypoints(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let _ = writeln!(out);
    let _ = writeln!(out, "const s = contractSpecString;");
    let _ = writeln!(out);
    let _ = writeln!(out, "const entrypoints = {{");

    for iface in spec.interfaces.iter() {
        let iface_name = spec_string_text(&iface.name)?;
        let iface_vers = spec_string_text(&iface.vers)?;
        for method in iface.methods.iter() {
            let method_name = spec_string_text(&method.name)?;
            let public_method_name = to_camel_case(method_name);
            let sig = NormalizedMethodSignature::new(method_name);
            let id = contract_entrypoint_id(Some(iface_name), iface_vers, &sig);
            let hex = hex::encode(id.as_ref());
            let _ = writeln!(
                out,
                "  {public_method_name}: contractEntrypointID(fromHex(\"{hex}\")),",
            );
        }
    }

    let _ = writeln!(out, "}} as const;");
    Ok(())
}

/// Emit the full `ContractSpec` object literal.
fn emit_spec_literal(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let _ = writeln!(out, "export const spec: ContractSpec = {{");

    // interfaces
    let _ = writeln!(out, "  interfaces: [");
    for iface in spec.interfaces.iter() {
        emit_spec_interface_literal(out, iface)?;
    }
    let _ = writeln!(out, "  ],");

    // type_defs
    let _ = writeln!(out, "  type_defs: [");
    for def in spec.type_defs.iter() {
        emit_spec_type_def_literal(out, def)?;
    }
    let _ = writeln!(out, "  ],");

    // events
    let _ = writeln!(out, "  events: [");
    for event in spec.events.iter() {
        emit_spec_event_literal(out, event)?;
    }
    let _ = writeln!(out, "  ],");

    let _ = writeln!(out, "}};");
    Ok(())
}

/// Emit a single event entry inside the spec literal.
fn emit_spec_event_literal(out: &mut String, event: &ContractEventDef) -> Result<(), CliError> {
    let name = spec_string_text(&event.name)?;
    let sig_hex = hex::encode(event.signature.into_inner());
    let _ = writeln!(out, "    {{");
    let _ = writeln!(out, "      name: s(\"{name}\"),");
    let _ = writeln!(out, "      signature: topic(fromHex(\"{sig_hex}\")),");

    let _ = writeln!(out, "      topics: [");
    for field in event.topics.iter() {
        let fname = spec_string_text(&field.name)?;
        let ty_lit = spec_type_to_ts_literal(&field.ty)?;
        let _ = writeln!(out, "        {{ name: s(\"{fname}\"), ty: {ty_lit} }},");
    }
    let _ = writeln!(out, "      ],");

    let _ = writeln!(out, "      fields: [");
    for field in event.fields.iter() {
        let fname = spec_string_text(&field.name)?;
        let ty_lit = spec_type_to_ts_literal(&field.ty)?;
        let _ = writeln!(out, "        {{ name: s(\"{fname}\"), ty: {ty_lit} }},");
    }
    let _ = writeln!(out, "      ],");

    let _ = writeln!(out, "    }},");
    Ok(())
}

/// Emit a single interface entry inside the spec literal.
fn emit_spec_interface_literal(
    out: &mut String,
    iface: &ContractSpecInterface,
) -> Result<(), CliError> {
    let name = spec_string_text(&iface.name)?;
    let vers = spec_string_text(&iface.vers)?;
    let _ = writeln!(out, "    {{");
    let _ = writeln!(out, "      name: s(\"{name}\"),");
    let _ = writeln!(out, "      vers: s(\"{vers}\"),");

    // throws
    let _ = writeln!(out, "      throws: [");
    for exc in iface.throws.iter() {
        emit_spec_exception_literal(out, exc)?;
    }
    let _ = writeln!(out, "      ],");

    // methods
    let _ = writeln!(out, "      methods: [");
    for method in iface.methods.iter() {
        emit_spec_method_literal(out, method)?;
    }
    let _ = writeln!(out, "      ],");

    let _ = writeln!(out, "    }},");
    Ok(())
}

/// Emit a single exception type entry inside the spec literal.
fn emit_spec_exception_literal(
    out: &mut String,
    exc: &ContractExceptionType,
) -> Result<(), CliError> {
    let ty_lit = spec_type_to_ts_literal(&exc.ty)?;
    let _ = writeln!(
        out,
        "        {{ selector: 0x{:x}n, ty: {} }},",
        exc.selector, ty_lit,
    );
    Ok(())
}

/// Emit a single method entry inside the spec literal.
fn emit_spec_method_literal(out: &mut String, method: &ContractSpecMethod) -> Result<(), CliError> {
    let name = spec_string_text(&method.name)?;
    let _ = writeln!(out, "        {{");
    let _ = writeln!(out, "          name: s(\"{name}\"),");

    // args
    let _ = writeln!(out, "          args: [");
    for arg in method.args.iter() {
        let arg_name = spec_string_text(&arg.name)?;
        let ty_lit = spec_type_to_ts_literal(&arg.ty)?;
        let _ = writeln!(
            out,
            "            {{ name: s(\"{arg_name}\"), ty: {ty_lit} }},",
        );
    }
    let _ = writeln!(out, "          ],");

    // return_type
    let ret_lit = spec_type_to_ts_literal(&method.return_type)?;
    let _ = writeln!(out, "          return_type: {ret_lit},");

    let _ = writeln!(out, "        }},");
    Ok(())
}

/// Emit a single type-def entry inside the spec literal.
fn emit_spec_type_def_literal(out: &mut String, def: &ContractTypeDef) -> Result<(), CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => {
            let name = spec_string_text(&struct_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      v: ContractTypeDefKind.TypeDefStruct,");
            let _ = writeln!(out, "      struct_def: {{");
            let _ = writeln!(out, "        name: s(\"{name}\"),");
            let _ = writeln!(out, "        fields: [");
            for field in struct_def.fields.iter() {
                let fname = spec_string_text(&field.name)?;
                let ty_lit = spec_type_to_ts_literal(&field.ty)?;
                let _ = writeln!(out, "          {{ name: s(\"{fname}\"), ty: {ty_lit} }},");
            }
            let _ = writeln!(out, "        ],");
            let _ = writeln!(out, "      }},");
            let _ = writeln!(out, "    }},");
        }
        ContractTypeDef::TypeDefEnum { enum_def } => {
            let name = spec_string_text(&enum_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      v: ContractTypeDefKind.TypeDefEnum,");
            let _ = writeln!(out, "      enum_def: {{");
            let _ = writeln!(out, "        name: s(\"{name}\"),");
            let _ = writeln!(out, "        variants: [");
            for variant in enum_def.variants.iter() {
                let vname = spec_string_text(&variant.name)?;
                let _ = writeln!(
                    out,
                    "          {{ name: s(\"{vname}\"), value: {}n }},",
                    variant.value,
                );
            }
            let _ = writeln!(out, "        ],");
            let _ = writeln!(out, "      }},");
            let _ = writeln!(out, "    }},");
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            let name = spec_string_text(&union_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      v: ContractTypeDefKind.TypeDefUnion,");
            let _ = writeln!(out, "      union_def: {{");
            let _ = writeln!(out, "        name: s(\"{name}\"),");
            let _ = writeln!(out, "        cases: [");
            for case in union_def.cases.iter() {
                let cname = spec_string_text(&case.name)?;
                emit_spec_union_case_literal(out, cname, case.discriminant, &case.case_type)?;
            }
            let _ = writeln!(out, "        ],");
            let _ = writeln!(out, "      }},");
            let _ = writeln!(out, "    }},");
        }
        _ => {
            return Err(CliError::SpecFailed(
                "contract spec contains an unsupported type definition \
                 kind"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// Emit a single union case inside the spec literal.
fn emit_spec_union_case_literal(
    out: &mut String,
    name: &str,
    discriminant: i32,
    case_type: &ContractUnionCaseType,
) -> Result<(), CliError> {
    match case_type {
        ContractUnionCaseType::UnionCaseVoid => {
            let _ = writeln!(out, "          {{");
            let _ = writeln!(out, "            name: s(\"{name}\"),");
            let _ = writeln!(out, "            discriminant: {discriminant}n,");
            let _ = writeln!(
                out,
                "            case_type: \
                 {{ v: ContractUnionCaseKind.UnionCaseVoid }},"
            );
            let _ = writeln!(out, "          }},");
        }
        ContractUnionCaseType::UnionCaseTuple { tuple } => {
            let _ = writeln!(out, "          {{");
            let _ = writeln!(out, "            name: s(\"{name}\"),");
            let _ = writeln!(out, "            discriminant: {discriminant}n,");
            let items: Vec<String> = tuple
                .iter()
                .map(spec_type_to_ts_literal)
                .collect::<Result<_, _>>()?;
            let _ = writeln!(
                out,
                "            case_type: {{ v: \
                 ContractUnionCaseKind.UnionCaseTuple, tuple: [{}] }},",
                items.join(", "),
            );
            let _ = writeln!(out, "          }},");
        }
        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
            let _ = writeln!(out, "          {{");
            let _ = writeln!(out, "            name: s(\"{name}\"),");
            let _ = writeln!(out, "            discriminant: {discriminant}n,");
            let mut fields_str = String::from("[");
            for (i, field) in struct_fields.iter().enumerate() {
                if i > 0 {
                    fields_str.push_str(", ");
                }
                let fname = spec_string_text(&field.name)?;
                let ty_lit = spec_type_to_ts_literal(&field.ty)?;
                let _ = write!(fields_str, "{{ name: s(\"{fname}\"), ty: {ty_lit} }}");
            }
            fields_str.push(']');
            let _ = writeln!(
                out,
                "            case_type: {{ v: \
                 ContractUnionCaseKind.UnionCaseStruct, \
                 struct_fields: {fields_str} }},",
            );
            let _ = writeln!(out, "          }},");
        }
        _ => {
            return Err(CliError::SpecFailed(
                "contract spec contains an unsupported union case kind".into(),
            ));
        }
    }
    Ok(())
}

/// Serialize a `ContractType` as a TypeScript object literal suitable for
/// the spec constant (e.g. `{ v: ContractTypeKind.U64 }`).
fn spec_type_to_ts_literal(ty: &ContractType) -> Result<String, CliError> {
    match ty {
        ContractType::Void => Ok("{ v: ContractTypeKind.Void }".into()),
        ContractType::Bool => Ok("{ v: ContractTypeKind.Bool }".into()),
        ContractType::U8 => Ok("{ v: ContractTypeKind.U8 }".into()),
        ContractType::U32 => Ok("{ v: ContractTypeKind.U32 }".into()),
        ContractType::I32 => Ok("{ v: ContractTypeKind.I32 }".into()),
        ContractType::U64 => Ok("{ v: ContractTypeKind.U64 }".into()),
        ContractType::I64 => Ok("{ v: ContractTypeKind.I64 }".into()),
        ContractType::U128 => Ok("{ v: ContractTypeKind.U128 }".into()),
        ContractType::I128 => Ok("{ v: ContractTypeKind.I128 }".into()),
        ContractType::String => Ok("{ v: ContractTypeKind.String }".into()),
        ContractType::Bytes => Ok("{ v: ContractTypeKind.Bytes }".into()),
        ContractType::Address => Ok("{ v: ContractTypeKind.Address }".into()),
        ContractType::Option { option_inner } => {
            let inner_lit = spec_type_to_ts_literal(option_inner.as_ref())?;
            Ok(format!(
                "{{ v: ContractTypeKind.Option, option_inner: \
                 {inner_lit} }}"
            ))
        }
        ContractType::Vector { vector } => {
            let elem = vector.element.as_ref();
            let elem_lit = spec_type_to_ts_literal(elem)?;
            Ok(format!(
                "{{ v: ContractTypeKind.Vector, vector: \
                 {{ element: {elem_lit}, max_len: {}n }} }}",
                vector.max_len,
            ))
        }
        ContractType::Array { array } => {
            let elem = array.element.as_ref();
            let elem_lit = spec_type_to_ts_literal(elem)?;
            Ok(format!(
                "{{ v: ContractTypeKind.Array, array: \
                 {{ element: {elem_lit}, len: {}n }} }}",
                array.len,
            ))
        }
        ContractType::Tuple { tuple } => {
            let items: Vec<String> = tuple
                .iter()
                .map(spec_type_to_ts_literal)
                .collect::<Result<_, _>>()?;
            Ok(format!(
                "{{ v: ContractTypeKind.Tuple, tuple: [{}] }}",
                items.join(", "),
            ))
        }
        ContractType::Named { named } => {
            let name = spec_string_text(named)?;
            Ok(format!(
                "{{ v: ContractTypeKind.Named, named: s(\"{name}\") }}"
            ))
        }
        _ => Err(CliError::SpecFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

/// Emit an exported TypeScript function for a single contract method.
fn emit_method_function(
    out: &mut String,
    iface_name: &str,
    method: &ContractSpecMethod,
) -> Result<(), CliError> {
    let method_name = spec_string_text(&method.name)?;
    let public_method_name = to_camel_case(method_name);
    let has_args = !method.args.is_empty();

    // Function signature
    if has_args {
        let _ = writeln!(out, "export function {public_method_name}(");
        let _ = writeln!(out, "  contract: ContractAddress,");
        let _ = writeln!(out, "  gasLimit: GasAmount,");
        let mut args_fields = String::new();
        for (i, arg) in method.args.iter().enumerate() {
            let arg_name = spec_string_text(&arg.name)?;
            let public_arg_name = to_camel_case(arg_name);
            let ts_ty = ts_type_ref(&arg.ty)?;
            if i > 0 {
                args_fields.push_str("; ");
            }
            let _ = write!(args_fields, "{public_arg_name}: {ts_ty}");
        }
        let _ = writeln!(out, "  args: {{ {args_fields} }},");
        let _ = writeln!(out, "): Invocation {{");
    } else {
        let _ = writeln!(out, "export function {public_method_name}(");
        let _ = writeln!(out, "  contract: ContractAddress,");
        let _ = writeln!(out, "  gasLimit: GasAmount,");
        let _ = writeln!(out, "): Invocation {{");
    }

    // Body
    let _ = writeln!(out, "  return createInvocation({{");
    let _ = writeln!(out, "    invoked_address: contract,");
    let _ = writeln!(out, "    entrypoint: entrypoints.{public_method_name},");
    let _ = writeln!(out, "    gas_limit: gasLimit,");

    if has_args {
        let arg_refs: Vec<String> = method
            .args
            .iter()
            .map(|arg| {
                let name = spec_string_text(&arg.name)?;
                Ok(format!("args.{}", to_camel_case(name)))
            })
            .collect::<Result<_, CliError>>()?;
        let _ = writeln!(
            out,
            "    calldata: encodeCalldata(\
             spec, \"{iface_name}\", \"{method_name}\", \
             [{}], namedCodecs),",
            arg_refs.join(", "),
        );
    } else {
        let _ = writeln!(
            out,
            "    calldata: encodeCalldata(\
             spec, \"{iface_name}\", \"{method_name}\", \
             [], namedCodecs),",
        );
    }

    let _ = writeln!(out, "  }});");
    let _ = writeln!(out, "}}");
    Ok(())
}

/// Emit per-event interface/const pairs, the per-contract event union, and
/// the combined `decodeEvent` dispatcher.
fn emit_event_sections(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let union_name = event_union_name(spec)?;

    // Event names share the file's top-level type namespace with type defs
    // and with the combined-decoder union.
    for event in spec.events.iter() {
        let event_name = spec_string_text(&event.name)?;
        if event_name == union_name {
            return Err(CliError::SpecFailed(format!(
                "event \"{event_name}\" collides with the generated \"{union_name}\" event union"
            )));
        }
        for def in spec.type_defs.iter() {
            let def_name = match def {
                ContractTypeDef::TypeDefStruct { struct_def } => {
                    spec_string_text(&struct_def.name)?
                }
                ContractTypeDef::TypeDefEnum { enum_def } => spec_string_text(&enum_def.name)?,
                ContractTypeDef::TypeDefUnion { union_def } => spec_string_text(&union_def.name)?,
                _ => continue,
            };
            if def_name == event_name {
                return Err(CliError::SpecFailed(format!(
                    "event \"{event_name}\" collides with a type definition of the same name"
                )));
            }
        }
    }
    for def in spec.type_defs.iter() {
        let def_name = match def {
            ContractTypeDef::TypeDefStruct { struct_def } => spec_string_text(&struct_def.name)?,
            ContractTypeDef::TypeDefEnum { enum_def } => spec_string_text(&enum_def.name)?,
            ContractTypeDef::TypeDefUnion { union_def } => spec_string_text(&union_def.name)?,
            _ => continue,
        };
        if def_name == union_name {
            return Err(CliError::SpecFailed(format!(
                "type definition \"{def_name}\" collides with the generated \
                 \"{union_name}\" event union"
            )));
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "function topicEquals(a: Uint8Array | undefined, b: Uint8Array): boolean {{"
    );
    let _ = writeln!(
        out,
        "  if (a === undefined || a.length !== b.length) return false;"
    );
    let _ = writeln!(
        out,
        "  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;"
    );
    let _ = writeln!(out, "  return true;");
    let _ = writeln!(out, "}}");

    for (idx, event) in spec.events.iter().enumerate() {
        let _ = writeln!(out);
        emit_event_decl(out, event, idx)?;
    }

    let _ = writeln!(out);
    emit_decode_event(out, spec)?;
    Ok(())
}

/// Emit the same-named interface + const pair for one event. TypeScript
/// declaration merging makes the event name both the decoded-event type
/// and the value carrying its signature and decoder.
fn emit_event_decl(out: &mut String, event: &ContractEventDef, idx: usize) -> Result<(), CliError> {
    let name = spec_string_text(&event.name)?;
    let sig_hex = hex::encode(event.signature.into_inner());

    // Topic values are hashes on the wire, so topic fields are always raw
    // Uint8Array; payload fields are typed.
    let mut topic_fields = String::new();
    for field in event.topics.iter() {
        let fname = spec_string_text(&field.name)?;
        if !topic_fields.is_empty() {
            topic_fields.push(' ');
        }
        let _ = write!(topic_fields, "{fname}: Uint8Array;");
    }
    let payload_fields = emit_inline_struct_fields(event.fields.iter())?;

    let _ = writeln!(out, "export interface {name} {{");
    let _ = writeln!(out, "  name: \"{name}\";");
    let _ = writeln!(out, "  topics: {{ {topic_fields} }};");
    let _ = writeln!(out, "  fields: {{ {payload_fields} }};");
    let _ = writeln!(out, "}}");

    let n_topics = event.topics.len() + 1; // +1 for the signature at topic 0
    let _ = writeln!(out, "export const {name} = {{");
    let _ = writeln!(
        out,
        "  /** Topic-0 signature hash from the contract spec. */"
    );
    let _ = writeln!(out, "  signature: fromHex(\"{sig_hex}\"),");
    let _ = writeln!(
        out,
        "  /** Returns null if topic 0 does not identify this event. */"
    );
    let _ = writeln!(
        out,
        "  decode(event: {{ topics: Uint8Array[]; data: Uint8Array }}): {name} | null {{"
    );
    let _ = writeln!(
        out,
        "    if (!topicEquals(event.topics[0], {name}.signature)) return null;"
    );
    let _ = writeln!(out, "    if (event.topics.length !== {n_topics}) {{");
    let _ = writeln!(
        out,
        "      throw new Error(`{name}: expected {n_topics} topics, got ${{event.topics.length}}`);"
    );
    let _ = writeln!(out, "    }}");

    // Topic field mapping (topics[1..]).
    let mut topic_entries = String::new();
    for (i, field) in event.topics.iter().enumerate() {
        let fname = spec_string_text(&field.name)?;
        if !topic_entries.is_empty() {
            topic_entries.push_str(", ");
        }
        let _ = write!(topic_entries, "{fname}: event.topics[{}]", i + 1);
    }

    // Payload decode, in declaration order, CXS-encoded like calldata.
    let _ = writeln!(out, "    const r = new CxsReader(event.data);");
    let mut field_entries = String::new();
    for (i, field) in event.fields.iter().enumerate() {
        let fname = spec_string_text(&field.name)?;
        let ts_ty = ts_type_ref(&field.ty)?;
        let _ = writeln!(
            out,
            "    const f{i} = resolveCodec(spec.events[{idx}].fields[{i}].ty, namedCodecs).decode(r) as {ts_ty};"
        );
        if !field_entries.is_empty() {
            field_entries.push_str(", ");
        }
        let _ = write!(field_entries, "{fname}: f{i}");
    }
    let _ = writeln!(out, "    if (r.remaining !== 0) {{");
    let _ = writeln!(
        out,
        "      throw new Error(`{name}: ${{r.remaining}} trailing bytes after payload`);"
    );
    let _ = writeln!(out, "    }}");

    let _ = writeln!(
        out,
        "    return {{ name: \"{name}\", topics: {{ {topic_entries} }}, fields: {{ {field_entries} }} }};"
    );
    let _ = writeln!(out, "  }},");
    let _ = writeln!(out, "}} as const;");
    Ok(())
}

/// Name of the per-contract event union: the first interface name plus
/// `Event`. The generated file is per-contract, so all events share it.
fn event_union_name(spec: &ContractSpec) -> Result<String, CliError> {
    let first_iface = spec.interfaces.iter().next().ok_or_else(|| {
        CliError::SpecFailed("contract spec with events has no interfaces".into())
    })?;
    Ok(format!("{}Event", spec_string_text(&first_iface.name)?))
}

/// Emit the per-contract event union and combined decoder.
fn emit_decode_event(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let union_name = event_union_name(spec)?;

    let event_names: Vec<&str> = spec
        .events
        .iter()
        .map(|e| spec_string_text(&e.name))
        .collect::<Result<_, _>>()?;

    let _ = writeln!(
        out,
        "export type {union_name} = {};",
        event_names.join(" | "),
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "export function decodeEvent(event: {{ topics: Uint8Array[]; data: Uint8Array }}): {union_name} | null {{"
    );
    let chain = event_names
        .iter()
        .map(|n| format!("{n}.decode(event)"))
        .collect::<Vec<_>>()
        .join(" ?? ");
    let _ = writeln!(out, "  return {chain};");
    let _ = writeln!(out, "}}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_public_name_collisions(spec: &ContractSpec) -> Result<(), CliError> {
    let mut top_level_values = BTreeMap::new();
    for name in [
        "contractEntrypointID",
        "fromHex",
        "ContractTypeKind",
        "ContractTypeDefKind",
        "contractSpecString",
        "encodeCalldata",
        "buildNamedCodecs",
        "createInvocation",
        "s",
        "entrypoints",
        "spec",
        "namedCodecs",
    ] {
        reserve_ts_value_name(
            &mut top_level_values,
            name.to_owned(),
            "generated binding".into(),
        )?;
    }

    if spec
        .type_defs
        .iter()
        .any(|d| matches!(d, ContractTypeDef::TypeDefUnion { .. }))
    {
        reserve_ts_value_name(
            &mut top_level_values,
            "ContractUnionCaseKind".into(),
            "generated union import".into(),
        )?;
    }

    for def in spec.type_defs.iter() {
        if let ContractTypeDef::TypeDefEnum { enum_def } = def {
            let enum_name = spec_string_text(&enum_def.name)?;
            reserve_ts_value_name(
                &mut top_level_values,
                enum_name.to_owned(),
                format!("enum \"{enum_name}\""),
            )?;
        }
    }

    if !spec.events.is_empty() {
        reserve_ts_value_name(
            &mut top_level_values,
            "topic".into(),
            "generated event import".into(),
        )?;
        reserve_ts_value_name(
            &mut top_level_values,
            "resolveCodec".into(),
            "generated event import".into(),
        )?;
        reserve_ts_value_name(
            &mut top_level_values,
            "CxsReader".into(),
            "generated event import".into(),
        )?;
        reserve_ts_value_name(
            &mut top_level_values,
            "topicEquals".into(),
            "generated event decoder".into(),
        )?;
        reserve_ts_value_name(
            &mut top_level_values,
            "decodeEvent".into(),
            "generated event decoder".into(),
        )?;

        for event in spec.events.iter() {
            let event_name = spec_string_text(&event.name)?;
            reserve_ts_value_name(
                &mut top_level_values,
                event_name.to_owned(),
                format!("event \"{event_name}\""),
            )?;
        }
    }

    for iface in spec.interfaces.iter() {
        for method in iface.methods.iter() {
            let method_name = spec_string_text(&method.name)?;
            let public_method_name = to_camel_case(method_name);
            reserve_ts_value_name(
                &mut top_level_values,
                public_method_name,
                format!("method \"{method_name}\""),
            )?;
            validate_method_arg_name_collisions(method_name, method)?;
        }
    }

    Ok(())
}

fn reserve_ts_value_name(
    names: &mut BTreeMap<String, String>,
    name: String,
    source: String,
) -> Result<(), CliError> {
    if let Some(existing) = names.get(&name) {
        return Err(CliError::SpecFailed(format!(
            "generated TypeScript value name \"{name}\" collides between {existing} and {source}"
        )));
    }
    names.insert(name, source);
    Ok(())
}

fn validate_method_arg_name_collisions(
    method_name: &str,
    method: &ContractSpecMethod,
) -> Result<(), CliError> {
    let mut args: BTreeMap<String, &str> = BTreeMap::new();
    for arg in method.args.iter() {
        let arg_name = spec_string_text(&arg.name)?;
        let public_arg_name = to_camel_case(arg_name);
        if let Some(existing) = args.get(&public_arg_name) {
            return Err(CliError::SpecFailed(format!(
                "method \"{method_name}\" argument names \"{existing}\" and \"{arg_name}\" both map to \"{public_arg_name}\""
            )));
        }
        args.insert(public_arg_name, arg_name);
    }
    Ok(())
}

fn to_camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut uppercase_next = false;

    for ch in name.chars() {
        if ch == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            out.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            out.push(ch);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_contract_spec::{
        ContractArrayType, ContractEnumDef, ContractEnumVariant, ContractEventDef, ContractSpec,
        ContractSpecInterface, ContractSpecMethod, ContractSpecString, ContractStructDef,
        ContractUnionCase, ContractVectorType,
    };

    fn spec_string(s: &str) -> ContractSpecString {
        s.try_into().unwrap()
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
    fn camel_case_leaves_already_camel_names_unchanged() {
        assert_eq!(to_camel_case("getBalanceOf"), "getBalanceOf");
    }

    #[test]
    fn bare_u8_is_an_error() {
        assert!(ts_type_ref(&ContractType::U8).is_err());
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
            option_inner: Box::new(ContractType::U64),
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "bigint | null");
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
    fn maps_vector_of_u8_to_uint8array() {
        let ty = ContractType::Vector {
            vector: ContractVectorType {
                element: Box::new(ContractType::U8),
                max_len: 1024,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "Uint8Array");
    }

    #[test]
    fn maps_array_of_u8_to_uint8array() {
        let ty = ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::U8),
                len: 32,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "Uint8Array");
    }

    #[test]
    fn maps_array_of_non_u8_to_array() {
        let ty = ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::U32),
                len: 4,
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

    /// An event with one topic field and one payload field.
    fn ping_event_def() -> ContractEventDef {
        ContractEventDef {
            name: spec_string("PingEvent"),
            signature: groundhog_xdr::event::Topic::from([0xAB; 32]),
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
    fn spec_with_events() -> ContractSpec {
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

    #[test]
    fn spec_literal_includes_events() {
        let output = generate_ts_bindings(&spec_with_events(), "counter.wasm").unwrap();
        assert!(
            output.contains("events: ["),
            "spec literal missing events array:\n{output}"
        );
        assert!(
            output.contains(&format!(
                "signature: topic(fromHex(\"{}\"))",
                "ab".repeat(32)
            )),
            "event signature literal missing:\n{output}"
        );
        assert!(
            output.contains("{ name: s(\"tag\"), ty: { v: ContractTypeKind.U64 } }"),
            "event topic field literal missing:\n{output}"
        );
        assert!(
            output.contains("{ name: s(\"note\"), ty: { v: ContractTypeKind.String } }"),
            "event payload field literal missing:\n{output}"
        );
    }

    #[test]
    fn generates_event_types_and_decoders() {
        let output = generate_ts_bindings(&spec_with_events(), "counter.wasm").unwrap();

        // interface + companion const (declaration merging)
        assert!(
            output.contains("export interface PingEvent {"),
            "missing interface:\n{output}"
        );
        assert!(
            output.contains("name: \"PingEvent\";"),
            "missing name discriminant:\n{output}"
        );
        assert!(
            output.contains("topics: { tag: Uint8Array; };"),
            "missing topics shape:\n{output}"
        );
        assert!(
            output.contains("fields: { note: string; };"),
            "missing fields shape:\n{output}"
        );
        assert!(
            output.contains("export const PingEvent = {"),
            "missing companion const:\n{output}"
        );
        assert!(
            output.contains(&format!("signature: fromHex(\"{}\")", "ab".repeat(32))),
            "missing signature const:\n{output}"
        );
        assert!(
            output.contains(
                "decode(event: { topics: Uint8Array[]; data: Uint8Array }): PingEvent | null"
            ),
            "missing per-event decode:\n{output}"
        );

        // combined union + dispatcher
        assert!(
            output.contains("export type CounterEvent = PingEvent;"),
            "missing union:\n{output}"
        );
        assert!(
            output.contains(
                "export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): CounterEvent | null"
            ),
            "missing decodeEvent:\n{output}"
        );
        assert!(
            output.contains("return PingEvent.decode(event);"),
            "missing dispatch body:\n{output}"
        );

        // decoder internals
        assert!(
            output.contains("CxsReader"),
            "decoder must use CxsReader:\n{output}"
        );
        assert!(
            output.contains("resolveCodec"),
            "decoder must use resolveCodec:\n{output}"
        );
    }

    #[test]
    fn no_event_section_without_events() {
        let output = generate_ts_bindings(&counter_spec(), "counter.wasm").unwrap();
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
    fn event_name_colliding_with_union_name_errors() {
        // The combined-decoder union is named "{FirstInterface}Event"; an
        // event with that exact name would collide in TS's type namespace.
        let mut spec = spec_with_events();
        let mut event = ping_event_def();
        event.name = spec_string("CounterEvent");
        spec.events = vec![event].try_into().unwrap();
        assert!(generate_ts_bindings(&spec, "counter.wasm").is_err());
    }

    #[test]
    fn event_name_colliding_with_type_def_errors() {
        let mut spec = spec_with_events();
        spec.type_defs = vec![ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name: spec_string("PingEvent"),
                fields: vec![].try_into().unwrap(),
            },
        }]
        .try_into()
        .unwrap();
        assert!(generate_ts_bindings(&spec, "counter.wasm").is_err());
    }

    #[test]
    fn method_name_public_collision_errors() {
        let spec = counter_spec_with_methods(
            vec![
                ContractSpecMethod {
                    name: spec_string("get_balance"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U64,
                },
                ContractSpecMethod {
                    name: spec_string("getBalance"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U64,
                },
            ],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.wasm").unwrap_err();
        assert!(err.to_string().contains("getBalance"));
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
            }],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.wasm").unwrap_err();
        assert!(err.to_string().contains("ownerId"));
    }

    #[test]
    fn method_name_colliding_with_import_errors() {
        let spec = counter_spec_with_methods(
            vec![ContractSpecMethod {
                name: spec_string("from_hex"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::Void,
            }],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.wasm").unwrap_err();
        assert!(err.to_string().contains("fromHex"));
    }

    #[test]
    fn method_name_colliding_with_event_decoder_errors() {
        let spec = counter_spec_with_methods(
            vec![ContractSpecMethod {
                name: spec_string("decode_event"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::Void,
            }],
            true,
        );

        let err = generate_ts_bindings(&spec, "counter.wasm").unwrap_err();
        assert!(err.to_string().contains("decodeEvent"));
    }

    fn counter_spec_with_methods(
        methods: Vec<ContractSpecMethod>,
        include_events: bool,
    ) -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
                name: spec_string("Counter"),
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

    /// Build a minimal counter contract spec: `get() -> U64` and
    /// `increment(amount: U64) -> Void`.
    fn counter_spec() -> ContractSpec {
        counter_spec_with_methods(
            vec![
                ContractSpecMethod {
                    name: spec_string("get"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U64,
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
                },
            ],
            false,
        )
    }

    #[test]
    fn generates_full_binding_for_simple_contract() {
        let spec = counter_spec();
        let output = generate_ts_bindings(&spec, "counter.wasm").unwrap();

        // Header
        assert!(output.contains("DO NOT EDIT"));
        assert!(output.contains("counter.wasm"));
        assert!(output.contains("Counter interface v0.1.0"));

        // Imports
        assert!(output.contains("@groundhogchain/protocol"));
        assert!(output.contains("@groundhogchain/client"));
        assert!(output.contains("@groundhogchain/contract-spec"));

        // ContractAddress is always imported (used in function sigs)
        assert!(output.contains("ContractAddress"));

        // Entrypoints block
        assert!(output.contains("const entrypoints = {"));
        assert!(output.contains("} as const;"));

        // Spec literal
        assert!(output.contains("export const spec: ContractSpec"));

        // Named codecs
        assert!(output.contains("const namedCodecs = buildNamedCodecs(spec.type_defs);"));

        // Zero-arg function has no args parameter
        assert!(output.contains("export function get("));
        let get_fn = output.split("export function get(").nth(1).unwrap();
        let get_sig = &get_fn[..get_fn.find("): Invocation").unwrap()];
        assert!(
            !get_sig.contains("args:"),
            "zero-arg function should not have args parameter"
        );

        // Function with args
        assert!(output.contains("export function increment("));
        let inc_fn = output.split("export function increment(").nth(1).unwrap();
        let inc_sig = &inc_fn[..inc_fn.find("): Invocation").unwrap()];
        assert!(
            inc_sig.contains("args:"),
            "function with args should have args parameter"
        );
        assert!(inc_sig.contains("amount: bigint"));

        // Braces balanced
        let opens = output.chars().filter(|&c| c == '{').count();
        let closes = output.chars().filter(|&c| c == '}').count();
        assert_eq!(
            opens, closes,
            "braces must be balanced: {opens} opens vs {closes} closes"
        );
    }

    /// ERC20-shaped contract with error enum, throws, and multiple methods.
    fn erc20_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
                name: spec_string("ERC20"),
                vers: spec_string("0.1.0"),
                throws: vec![ContractExceptionType {
                    selector: 0xb199_47ac_b2fa_14ff,
                    ty: ContractType::Named {
                        named: spec_string("ERC20Error"),
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
                    },
                    ContractSpecMethod {
                        name: spec_string("get_balance"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U128,
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
                    },
                ]
                .try_into()
                .unwrap(),
            }]
            .try_into()
            .unwrap(),
            type_defs: vec![ContractTypeDef::TypeDefEnum {
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

    #[test]
    fn generates_erc20_bindings() {
        let spec = erc20_spec();
        let output = generate_ts_bindings(&spec, "erc20.wasm").unwrap();

        // Entrypoint hashes
        assert!(
            output.contains("3799fd1c3d9b098f"),
            "transfer entrypoint hash missing"
        );
        assert!(
            output.contains("7a44649d55cabda8"),
            "get_balance entrypoint hash missing"
        );
        assert!(
            output.contains("73c9aac12bd15a84"),
            "get_balance_of entrypoint hash missing"
        );

        // Enum declaration
        assert!(
            output.contains("export enum ERC20Error"),
            "ERC20Error enum declaration missing"
        );
        assert!(
            output.contains("AlreadyInitialized = 1"),
            "AlreadyInitialized variant missing"
        );
        assert!(
            output.contains("Internal = 100"),
            "Internal variant missing"
        );

        // Function declarations
        assert!(
            output.contains("export function transfer("),
            "transfer function missing"
        );
        assert!(
            output.contains("export function getBalance("),
            "getBalance function missing"
        );
        assert!(
            output.contains("export function getBalanceOf("),
            "getBalanceOf function missing"
        );
        assert!(
            !output.contains("export function get_balance("),
            "get_balance function should be camelCase"
        );
        assert!(
            !output.contains("export function get_balance_of("),
            "get_balance_of function should be camelCase"
        );

        // Typed arguments
        let transfer_fn = output.split("export function transfer(").nth(1).unwrap();
        let transfer_sig = &transfer_fn[..transfer_fn.find("): Invocation").unwrap()];
        assert!(
            transfer_sig.contains("to: ContractAddress"),
            "transfer 'to' arg type mismatch"
        );
        assert!(
            transfer_sig.contains("amount: bigint"),
            "transfer 'amount' arg type mismatch"
        );

        let get_balance_of_fn = output
            .split("export function getBalanceOf(")
            .nth(1)
            .unwrap();
        let get_balance_of_sig =
            &get_balance_of_fn[..get_balance_of_fn.find("): Invocation").unwrap()];
        assert!(
            get_balance_of_sig.contains("accountId: ContractAddress"),
            "getBalanceOf 'accountId' arg type mismatch"
        );
        assert!(
            output.contains("entrypoint: entrypoints.getBalanceOf"),
            "getBalanceOf should read the camelCase entrypoint key"
        );
        assert!(
            output.contains("\"get_balance_of\""),
            "source method name should be preserved for encodeCalldata/spec lookup"
        );
        assert!(
            output.contains("name: s(\"account_id\")"),
            "source arg name should be preserved in the spec literal"
        );
        assert!(
            output.contains("args.accountId"),
            "calldata extraction should read the camelCase public arg"
        );

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

    /// Contract with recursive type (Tree with optional left/right children).
    fn recursive_type_spec() -> ContractSpec {
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
                            named: spec_string("Tree"),
                        },
                    }]
                    .try_into()
                    .unwrap(),
                    return_type: ContractType::U32,
                }]
                .try_into()
                .unwrap(),
            }]
            .try_into()
            .unwrap(),
            type_defs: vec![ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_string("Tree"),
                    fields: vec![
                        ContractField {
                            name: spec_string("value"),
                            ty: ContractType::U32,
                        },
                        ContractField {
                            name: spec_string("left"),
                            ty: ContractType::Option {
                                option_inner: Box::new(ContractType::Named {
                                    named: spec_string("Tree"),
                                }),
                            },
                        },
                        ContractField {
                            name: spec_string("right"),
                            ty: ContractType::Option {
                                option_inner: Box::new(ContractType::Named {
                                    named: spec_string("Tree"),
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

    #[test]
    fn generates_recursive_type_bindings() {
        let spec = recursive_type_spec();
        let output = generate_ts_bindings(&spec, "recursive.wasm").unwrap();

        // Tree struct emitted as TypeScript interface
        assert!(
            output.contains("export interface Tree"),
            "Tree interface declaration missing"
        );
        assert!(
            output.contains("value: bigint;"),
            "Tree.value field missing"
        );
        assert!(
            output.contains("left: Tree | null;"),
            "Tree.left field missing"
        );
        assert!(
            output.contains("right: Tree | null;"),
            "Tree.right field missing"
        );

        // depth function uses Named("Tree") argument
        let depth_fn = output.split("export function depth(").nth(1).unwrap();
        let depth_sig = &depth_fn[..depth_fn.find("): Invocation").unwrap()];
        assert!(
            depth_sig.contains("tree: Tree"),
            "depth 'tree' arg type mismatch"
        );
    }
}
