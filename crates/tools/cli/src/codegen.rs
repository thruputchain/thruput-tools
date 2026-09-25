//! TypeScript binding code generation from contract specs.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use groundhog_contract_spec::{
    ContractEnumDef, ContractEventDef, ContractExceptionType, ContractField, ContractSpec,
    ContractSpecId, ContractSpecInterface, ContractSpecMethod, ContractStructDef, ContractType,
    ContractTypeDef, ContractUnionCaseType, ContractUnionDef,
};
use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};

use crate::CliError;

/// Where a spec id lands in the generated TypeScript: the namespaces it nests
/// under, then the declaration's own name.
///
/// The mapping is total and injective, so no pass has to detect collisions and
/// no name shifts when an unrelated type joins the spec. Two types differing
/// anywhere in their id land in different places.
struct TsIdPath {
    namespaces: Vec<String>,
    name: String,
}

impl TsIdPath {
    /// The fully qualified reference, e.g. `groundhog_erc20_if.v0_1.ERC20Error`.
    fn qualified(&self) -> String {
        let mut out = String::new();
        for ns in &self.namespaces {
            out.push_str(ns);
            out.push('.');
        }
        out.push_str(&self.name);
        out
    }
}

/// Map a spec id onto its TypeScript path.
///
/// Every segment is rendered, never parsed back out:
/// - the crate name is a Cargo package name, so hyphenated; `-` becomes `_`,
///   which cannot alias two crates together because a registry already treats
///   `a-b` and `a_b` as one name
/// - the major is nested rather than suffixed, so a crate actually named
///   `foo_v1` lands at `foo_v1.v...` instead of aliasing onto `foo` major 1
/// - `module_path!()` leads with the crate's own Rust ident, which the first
///   namespace already carries, so that segment is dropped when it matches
fn ts_id_path(id: &ContractSpecId) -> Result<TsIdPath, CliError> {
    let crate_name = spec_string_text(&id.crate_name)?;
    let major = spec_string_text(&id.major)?;
    let module_path = spec_string_text(&id.module_path)?;
    let declared = spec_string_text(&id.declared)?;

    // `name = "..."` is substituted verbatim and the SDK validates it only as
    // UTF-8, so a declared name can contain `::`. Nesting on it would make this
    // mapping non-injective: a type named `A::B` at a crate's root and a type
    // named `B` in that crate's module `A` would land in one place.
    if declared.contains("::") {
        return Err(CliError::SpecFailed(format!(
            "spec type `{}` has `::` in its declared name, which a generated \
             binding cannot tell apart from a module of that path; drop it from \
             the `name = \"...\"` attribute",
            groundhog_client::codec::spec_id_text(id)?
        )));
    }

    let krate = crate_name.replace('-', "_");
    let mut namespaces = vec![krate.clone(), format!("v{}", major.replace('.', "_"))];

    let mut segments = module_path.split("::").filter(|s| !s.is_empty()).peekable();
    if segments.peek() == Some(&krate.as_str()) {
        segments.next();
    }
    namespaces.extend(segments.map(str::to_owned));

    for ns in &namespaces {
        validate_ts_identifier(ns, "spec id segment")?;
    }
    validate_ts_identifier(declared, "spec type name")?;

    Ok(TsIdPath {
        namespaces,
        name: declared.to_owned(),
    })
}

/// A spec id in TypeScript reference position.
fn ts_id_ref(id: &ContractSpecId) -> Result<String, CliError> {
    Ok(ts_id_path(id)?.qualified())
}

/// The id a type def declares itself under.
fn type_def_id(def: &ContractTypeDef) -> Result<&ContractSpecId, CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => Ok(&struct_def.name),
        ContractTypeDef::TypeDefEnum { enum_def } => Ok(&enum_def.name),
        ContractTypeDef::TypeDefUnion { union_def } => Ok(&union_def.name),
        _ => Err(CliError::SpecFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}
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
        | ContractType::I128
        | ContractType::U256
        | ContractType::I256 => Ok("bigint".into()),
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
            if inner.contains('|') || inner.starts_with("readonly ") {
                Ok(format!("({inner})[]"))
            } else {
                Ok(format!("{inner}[]"))
            }
        }
        ContractType::Array { array } => {
            let element = array.element.as_ref();
            if matches!(element, ContractType::U8) {
                return Ok(format!("FixedBytes<{}>", array.len));
            }
            let inner = ts_type_ref(element)?;
            if array.len <= 3 {
                let item = if inner.contains('|') {
                    format!("({inner})")
                } else {
                    inner
                };
                Ok(format!(
                    "readonly [{}]",
                    vec![item; array.len as usize].join(", ")
                ))
            } else {
                Ok(format!("FixedArray<{inner}, {}>", array.len))
            }
        }
        ContractType::Tuple { tuple } => {
            let items: Vec<String> = tuple.iter().map(ts_type_ref).collect::<Result<_, _>>()?;
            Ok(format!("readonly [{}]", items.join(", ")))
        }
        ContractType::Named { named } => ts_id_ref(named),
        ContractType::SignedToken {
            signed_token_preimage,
        } => match signed_token_preimage.preimage_type.as_deref() {
            Some(inner) => Ok(format!("SignedToken<{}>", ts_type_ref(inner)?)),
            None => Ok("SignedToken".into()),
        },
        _ => Err(CliError::SpecFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

/// Emit a TypeScript type declaration for a named contract type definition.
/// Write declarations grouped under the namespaces their ids map to.
///
/// Items are grouped by namespace path and the paths walked in sorted order,
/// so a shared prefix opens once and every namespace closes exactly where it
/// should. Each item renders itself with its bare declared name; the
/// surrounding namespaces supply the qualification.
fn emit_namespaced<T>(
    out: &mut String,
    items: &[(TsIdPath, T)],
    mut render: impl FnMut(&T) -> Result<String, CliError>,
) -> Result<(), CliError> {
    let mut by_namespace: BTreeMap<&[String], Vec<&T>> = BTreeMap::new();
    for (path, item) in items {
        by_namespace
            .entry(path.namespaces.as_slice())
            .or_default()
            .push(item);
    }

    let mut open: Vec<String> = Vec::new();
    for (path, group) in &by_namespace {
        let shared = open
            .iter()
            .zip(path.iter())
            .take_while(|(a, b)| a == b)
            .count();
        while open.len() > shared {
            open.pop();
            let _ = writeln!(out, "{}}}", "  ".repeat(open.len()));
        }
        while open.len() < path.len() {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "{}export namespace {} {{",
                "  ".repeat(open.len()),
                path[open.len()]
            );
            open.push(path[open.len()].clone());
        }

        let indent = "  ".repeat(open.len());
        for item in group {
            let _ = writeln!(out);
            for line in render(item)?.lines() {
                if line.is_empty() {
                    let _ = writeln!(out);
                } else {
                    let _ = writeln!(out, "{indent}{line}");
                }
            }
        }
    }

    while open.pop().is_some() {
        let _ = writeln!(out, "{}}}", "  ".repeat(open.len()));
    }

    Ok(())
}

/// Emit every named type declaration, nested under its id's namespaces.
fn emit_type_declarations(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let items = spec
        .type_defs
        .iter()
        .map(|def| Ok((ts_id_path(type_def_id(def)?)?, def)))
        .collect::<Result<Vec<_>, CliError>>()?;
    emit_namespaced(out, &items, |def| emit_type_def(def))
}

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
    let name = spec_string_text(&def.name.declared)?;
    let mut fields = String::new();
    for field in def.fields.iter() {
        let field_name = spec_string_text(&field.name)?;
        let ts_ty = ts_type_ref(&field.ty)?;
        if !fields.is_empty() {
            fields.push(' ');
        }
        let _ = write!(fields, "{}: {ts_ty};", ts_property_key(field_name)?);
    }
    Ok(format!("export interface {name} {{ {fields} }}"))
}

/// Emit a TypeScript enum for a contract enum definition.
fn emit_enum(def: &ContractEnumDef) -> Result<String, CliError> {
    let name = spec_string_text(&def.name.declared)?;
    let mut variants = String::new();
    for variant in def.variants.iter() {
        let variant_name = spec_string_text(&variant.name)?;
        if !variants.is_empty() {
            variants.push(' ');
        }
        let _ = write!(
            variants,
            "{}: {}n,",
            ts_property_key(variant_name)?,
            variant.value
        );
    }
    Ok(format!(
        "export const {name} = {{ {variants} }} as const;\n\nexport type {name} = (typeof {name})[keyof typeof {name}];"
    ))
}

/// Emit a TypeScript discriminated union for a contract union definition.
fn emit_union(def: &ContractUnionDef) -> Result<String, CliError> {
    let name = spec_string_text(&def.name.declared)?;
    let mut cases: Vec<String> = Vec::new();

    for case in def.cases.iter() {
        let case_name = spec_string_text(&case.name)?;
        let arm = match &case.case_type {
            ContractUnionCaseType::UnionCaseVoid => {
                format!("{{ case: {} }}", ts_string_literal(case_name))
            }
            ContractUnionCaseType::UnionCaseTuple { tuple } => {
                let items: Vec<String> = tuple.iter().map(ts_type_ref).collect::<Result<_, _>>()?;
                format!(
                    "{{ case: {}; value: readonly [{}] }}",
                    ts_string_literal(case_name),
                    items.join(", ")
                )
            }
            ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                let fields = emit_inline_struct_fields(struct_fields.iter())?;
                format!(
                    "{{ case: {}; value: {{ {fields} }} }}",
                    ts_string_literal(case_name)
                )
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
        let _ = write!(out, "{}: {ts_ty};", ts_property_key(field_name)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Full binding file generation
// ---------------------------------------------------------------------------

/// Generate a complete, self-contained TypeScript binding file from a
/// contract spec.
///
/// `source_name` is the human-readable origin (for example, a spec sidecar)
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
    let public_method_names_by_interface = public_method_names_by_interface(spec)?;
    validate_spec(spec, &public_method_names_by_interface)?;
    let has_methods = spec
        .interfaces
        .iter()
        .any(|iface| !iface.methods.is_empty());
    emit_header(&mut out, source_name, &interfaces);

    // 2. Imports
    emit_imports(&mut out, spec);

    let _ = writeln!(out);
    let _ = writeln!(out, "const s = contractSpecString;");

    // 3. Entrypoints
    if has_methods {
        emit_entrypoints(&mut out, spec, &public_method_names_by_interface)?;
    }

    // 4. Named type declarations, nested under the namespaces their ids map to
    emit_type_declarations(&mut out, spec)?;

    // 5. Method argument, result, and exception declarations
    for (iface_index, iface) in spec.interfaces.iter().enumerate() {
        let _ = writeln!(out);
        emit_interface_types(
            &mut out,
            iface,
            &public_method_names_by_interface[iface_index],
        )?;
    }

    // 6. Spec literal
    let _ = writeln!(out);
    emit_spec_literal(&mut out, spec)?;

    // 7. Shared codec context
    if has_methods || !spec.events.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "const codecs = createContractCodecContext(spec.type_defs);"
        );
    }

    // 8. Per-interface method descriptors
    for (iface_index, iface) in spec.interfaces.iter().enumerate() {
        let _ = writeln!(out);
        emit_interface_object(
            &mut out,
            iface,
            iface_index,
            &public_method_names_by_interface[iface_index],
        )?;
    }

    // 9. Event descriptors and combined decoder
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
    let source_name = ts_line_comment_text(source_name);
    for (name, vers) in interfaces {
        let name = ts_line_comment_text(name);
        let vers = ts_line_comment_text(vers);
        let _ = writeln!(
            out,
            "// Generated from {source_name} - {name} interface v{vers}"
        );
    }
    let _ = writeln!(out, "// DO NOT EDIT. Re-run codegen to update.");
}

/// Emit the TypeScript import block.
fn emit_imports(out: &mut String, spec: &ContractSpec) {
    let _ = writeln!(out);
    let has_events = !spec.events.is_empty();
    let has_methods = spec
        .interfaces
        .iter()
        .any(|iface| !iface.methods.is_empty());
    let has_contract_types = spec_uses_type(spec, |_| true);
    let uses_address = spec_uses_type(spec, |ty| matches!(ty, ContractType::Address));
    let uses_fixed_bytes = spec_uses_type(
        spec,
        |ty| matches!(ty, ContractType::Array { array } if matches!(array.element.as_ref(), ContractType::U8)),
    );
    let uses_fixed_array = spec_uses_type(
        spec,
        |ty| matches!(ty, ContractType::Array { array } if array.len > 3 && !matches!(array.element.as_ref(), ContractType::U8)),
    );
    let uses_signed_token =
        spec_uses_type(spec, |ty| matches!(ty, ContractType::SignedToken { .. }));
    let has_direct_event_topics = spec.events.iter().any(|event| {
        event
            .topics
            .iter()
            .any(|field| event_topic_representation(&field.ty) != "hash")
    });

    if uses_address {
        let _ = writeln!(
            out,
            "import type {{ ContractAddress }} from \"@groundhogchain/protocol\";",
        );
    }
    let mut protocol_imports = Vec::new();
    if has_methods {
        protocol_imports.push("contractEntrypointID");
    }
    if has_events {
        protocol_imports.push("topic");
    }
    if !protocol_imports.is_empty() {
        let _ = writeln!(
            out,
            "import {{ {} }} from \"@groundhogchain/protocol\";",
            protocol_imports.join(", ")
        );
        let _ = writeln!(
            out,
            "import {{ fromHex }} from \"@groundhogchain/protocol/utils\";"
        );
    }

    // @groundhogchain/contract-spec imports
    let has_unions = spec
        .type_defs
        .iter()
        .any(|d| matches!(d, ContractTypeDef::TypeDefUnion { .. }));
    let mut cs_imports: Vec<&str> = vec!["contractSpecString"];
    if has_contract_types {
        cs_imports.insert(0, "ContractTypeKind");
    }
    if !spec.type_defs.is_empty() {
        cs_imports.insert(1, "ContractTypeDefKind");
    }
    if has_unions {
        cs_imports.push("ContractUnionCaseKind");
    }
    let _ = writeln!(
        out,
        "import {{ {} }} from \"@groundhogchain/contract-spec\";",
        cs_imports.join(", "),
    );

    let mut client_imports = vec!["type ContractSpec"];
    if has_methods || has_events {
        client_imports.push("createContractCodecContext");
    }
    if has_methods {
        client_imports.push("defineContractMethod");
    }
    if !spec.interfaces.is_empty() {
        client_imports.push("type ContractException");
    }
    if uses_fixed_array {
        client_imports.push("type FixedArray");
    }
    if uses_fixed_bytes {
        client_imports.push("type FixedBytes");
    }
    if uses_signed_token {
        client_imports.push("type SignedToken");
    }
    if has_events {
        client_imports.extend(["defineContractEvent", "type DecodedContractEvent"]);
        if has_direct_event_topics {
            client_imports.push("type ContractEventFilterValue");
        }
    }
    let _ = writeln!(
        out,
        "import {{ {} }} from \"@groundhogchain/client/contract\";",
        client_imports.join(", ")
    );
}

/// Emit the `entrypoints` const mapping interface/method names to their IDs.
fn emit_entrypoints(
    out: &mut String,
    spec: &ContractSpec,
    public_method_names: &[Vec<String>],
) -> Result<(), CliError> {
    let _ = writeln!(out);
    let _ = writeln!(out, "const entrypoints = {{");

    for (iface_index, iface) in spec.interfaces.iter().enumerate() {
        let iface_name = spec_string_text(&iface.name)?;
        let iface_vers = spec_string_text(&iface.vers)?;
        let _ = writeln!(out, "  {iface_name}: {{");
        for (method_index, method) in iface.methods.iter().enumerate() {
            let method_name = spec_string_text(&method.name)?;
            let public_method_name = &public_method_names[iface_index][method_index];
            let sig = NormalizedMethodSignature::new(method_name);
            let id = contract_entrypoint_id(Some(iface_name), iface_vers, &sig);
            let hex = hex::encode(id.as_ref());
            let _ = writeln!(
                out,
                "    {public_method_name}: contractEntrypointID(fromHex(\"{hex}\")),",
            );
        }
        let _ = writeln!(out, "  }},");
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

/// A spec id as a runtime literal, mirroring the XDR struct field for field.
///
/// The generated spec object is data the TypeScript codec resolves named
/// references through, so the id travels whole: flattening it here would hand
/// every consumer back the ambiguity the segments remove.
fn ts_spec_id_literal(id: &ContractSpecId) -> Result<String, CliError> {
    Ok(format!(
        "{{ crate_name: s({}), major: s({}), module_path: s({}), declared: s({}) }}",
        ts_string_literal(spec_string_text(&id.crate_name)?),
        ts_string_literal(spec_string_text(&id.major)?),
        ts_string_literal(spec_string_text(&id.module_path)?),
        ts_string_literal(spec_string_text(&id.declared)?),
    ))
}

/// Emit a single event entry inside the spec literal.
fn emit_spec_event_literal(out: &mut String, event: &ContractEventDef) -> Result<(), CliError> {
    let name = ts_spec_id_literal(&event.name)?;
    let sig_hex = hex::encode(event.signature.into_inner());
    let _ = writeln!(out, "    {{");
    let _ = writeln!(out, "      name: {name},");
    let _ = writeln!(out, "      signature: topic(fromHex(\"{sig_hex}\")),");

    let _ = writeln!(out, "      topics: [");
    for field in event.topics.iter() {
        let fname = spec_string_text(&field.name)?;
        let ty_lit = spec_type_to_ts_literal(&field.ty)?;
        let _ = writeln!(
            out,
            "        {{ name: s({}), ty: {ty_lit} }},",
            ts_string_literal(fname)
        );
    }
    let _ = writeln!(out, "      ],");

    let _ = writeln!(out, "      fields: [");
    for field in event.fields.iter() {
        let fname = spec_string_text(&field.name)?;
        let ty_lit = spec_type_to_ts_literal(&field.ty)?;
        let _ = writeln!(
            out,
            "        {{ name: s({}), ty: {ty_lit} }},",
            ts_string_literal(fname)
        );
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
    let _ = writeln!(out, "      name: s({}),", ts_string_literal(name));
    let _ = writeln!(out, "      vers: s({}),", ts_string_literal(vers));

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
    let _ = writeln!(out, "          name: s({}),", ts_string_literal(name));

    // args
    let _ = writeln!(out, "          args: [");
    for arg in method.args.iter() {
        let arg_name = spec_string_text(&arg.name)?;
        let ty_lit = spec_type_to_ts_literal(&arg.ty)?;
        let _ = writeln!(
            out,
            "            {{ name: s({}), ty: {ty_lit} }},",
            ts_string_literal(arg_name),
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
            let name = ts_spec_id_literal(&struct_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      v: ContractTypeDefKind.TypeDefStruct,");
            let _ = writeln!(out, "      struct_def: {{");
            let _ = writeln!(out, "        name: {name},");
            let _ = writeln!(out, "        fields: [");
            for field in struct_def.fields.iter() {
                let fname = spec_string_text(&field.name)?;
                let ty_lit = spec_type_to_ts_literal(&field.ty)?;
                let _ = writeln!(
                    out,
                    "          {{ name: s({}), ty: {ty_lit} }},",
                    ts_string_literal(fname)
                );
            }
            let _ = writeln!(out, "        ],");
            let _ = writeln!(out, "      }},");
            let _ = writeln!(out, "    }},");
        }
        ContractTypeDef::TypeDefEnum { enum_def } => {
            let name = ts_spec_id_literal(&enum_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      v: ContractTypeDefKind.TypeDefEnum,");
            let _ = writeln!(out, "      enum_def: {{");
            let _ = writeln!(out, "        name: {name},");
            let _ = writeln!(out, "        variants: [");
            for variant in enum_def.variants.iter() {
                let vname = spec_string_text(&variant.name)?;
                let _ = writeln!(
                    out,
                    "          {{ name: s({}), value: {}n }},",
                    ts_string_literal(vname),
                    variant.value,
                );
            }
            let _ = writeln!(out, "        ],");
            let _ = writeln!(out, "      }},");
            let _ = writeln!(out, "    }},");
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            let name = ts_spec_id_literal(&union_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      v: ContractTypeDefKind.TypeDefUnion,");
            let _ = writeln!(out, "      union_def: {{");
            let _ = writeln!(out, "        name: {name},");
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
            let _ = writeln!(out, "            name: s({}),", ts_string_literal(name));
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
            let _ = writeln!(out, "            name: s({}),", ts_string_literal(name));
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
            let _ = writeln!(out, "            name: s({}),", ts_string_literal(name));
            let _ = writeln!(out, "            discriminant: {discriminant}n,");
            let mut fields_str = String::from("[");
            for (i, field) in struct_fields.iter().enumerate() {
                if i > 0 {
                    fields_str.push_str(", ");
                }
                let fname = spec_string_text(&field.name)?;
                let ty_lit = spec_type_to_ts_literal(&field.ty)?;
                let _ = write!(
                    fields_str,
                    "{{ name: s({}), ty: {ty_lit} }}",
                    ts_string_literal(fname)
                );
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
        ContractType::U256 => Ok("{ v: ContractTypeKind.U256 }".into()),
        ContractType::I256 => Ok("{ v: ContractTypeKind.I256 }".into()),
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
            // The whole id, matching what the type defs in this same literal
            // declare themselves under. A bare declared name here would never
            // match one of those, so no named reference would resolve.
            Ok(format!(
                "{{ v: ContractTypeKind.Named, named: {} }}",
                ts_spec_id_literal(named)?
            ))
        }
        ContractType::SignedToken {
            signed_token_preimage,
        } => {
            let preimage = match signed_token_preimage.preimage_type.as_deref() {
                Some(inner) => spec_type_to_ts_literal(inner)?,
                None => "null".into(),
            };
            Ok(format!(
                "{{ v: ContractTypeKind.SignedToken, signed_token_preimage: \
                 {{ preimage_type: {preimage} }} }}"
            ))
        }
        _ => Err(CliError::SpecFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

/// Emit the exported object that groups methods for one contract interface.
fn emit_interface_object(
    out: &mut String,
    iface: &ContractSpecInterface,
    iface_index: usize,
    public_method_names: &[String],
) -> Result<(), CliError> {
    let iface_name = spec_string_text(&iface.name)?;
    let _ = writeln!(out, "export const {iface_name} = {{");
    for (method_index, method) in iface.methods.iter().enumerate() {
        emit_interface_method(
            out,
            iface,
            iface_name,
            iface_index,
            method,
            method_index,
            &public_method_names[method_index],
        )?;
    }
    let _ = writeln!(out, "}} as const;");
    Ok(())
}

fn emit_interface_types(
    out: &mut String,
    iface: &ContractSpecInterface,
    public_method_names: &[String],
) -> Result<(), CliError> {
    let iface_name = spec_string_text(&iface.name)?;
    for (method_index, method) in iface.methods.iter().enumerate() {
        let stem = method_type_stem(iface_name, &public_method_names[method_index]);
        if method.args.is_empty() {
            let _ = writeln!(out, "export type {stem}Args = void;");
        } else {
            let _ = writeln!(out, "export interface {stem}Args {{");
            for arg in method.args.iter() {
                let abi_name = spec_string_text(&arg.name)?;
                let public_name = to_camel_case(abi_name);
                let _ = writeln!(out, "  {public_name}: {};", ts_type_ref(&arg.ty)?);
            }
            let _ = writeln!(out, "}}");
        }
        let _ = writeln!(
            out,
            "export type {stem}Result = {};",
            ts_type_ref(&method.return_type)?
        );
        let _ = writeln!(out);
    }

    let declared_exception = format!("{iface_name}DeclaredException");
    if iface.throws.is_empty() {
        let _ = writeln!(out, "export type {declared_exception} = never;");
    } else {
        let _ = writeln!(out, "export type {declared_exception} =");
        for (index, exception) in iface.throws.iter().enumerate() {
            let prefix = if index == 0 { "  " } else { "  | " };
            let name = exception_name(&exception.ty)?;
            let _ = writeln!(out, "{prefix}{{");
            let _ = writeln!(out, "    kind: \"declared\";");
            let _ = writeln!(out, "    name: {name};");
            let _ = writeln!(out, "    selector: 0x{:x}n;", exception.selector);
            let _ = writeln!(out, "    value: {};", ts_type_ref(&exception.ty)?);
            let _ = writeln!(
                out,
                "  }}{}",
                if index + 1 == iface.throws.len() {
                    ";"
                } else {
                    ""
                }
            );
        }
    }
    let _ = writeln!(
        out,
        "export type {iface_name}Exception = ContractException<{declared_exception}>;"
    );
    Ok(())
}

/// Emit one contract method descriptor.
fn emit_interface_method(
    out: &mut String,
    iface: &ContractSpecInterface,
    iface_name: &str,
    iface_index: usize,
    method: &ContractSpecMethod,
    method_index: usize,
    public_method_name: &str,
) -> Result<(), CliError> {
    let stem = method_type_stem(iface_name, public_method_name);
    let _ = writeln!(out, "  {public_method_name}: defineContractMethod<");
    let _ = writeln!(out, "    {stem}Args,");
    let _ = writeln!(out, "    {stem}Result,");
    let _ = writeln!(out, "    {iface_name}DeclaredException");
    let _ = writeln!(out, "  >({{");
    let _ = writeln!(out, "    interfaceName: {},", ts_string_literal(iface_name));
    let _ = writeln!(
        out,
        "    interfaceVersion: {},",
        ts_string_literal(spec_string_text(&iface.vers)?)
    );
    let _ = writeln!(
        out,
        "    abiName: {},",
        ts_string_literal(spec_string_text(&method.name)?)
    );
    let _ = writeln!(
        out,
        "    publicName: {},",
        ts_string_literal(public_method_name)
    );
    let _ = writeln!(
        out,
        "    entrypoint: entrypoints.{iface_name}.{public_method_name},"
    );
    let _ = writeln!(out, "    args: [");
    for (arg_index, arg) in method.args.iter().enumerate() {
        let abi_name = spec_string_text(&arg.name)?;
        let public_name = to_camel_case(abi_name);
        let _ = writeln!(out, "      {{");
        let _ = writeln!(out, "        abiName: {},", ts_string_literal(abi_name));
        let _ = writeln!(
            out,
            "        publicName: {},",
            ts_string_literal(&public_name)
        );
        let _ = writeln!(
            out,
            "        type: spec.interfaces[{iface_index}]!.methods[{method_index}]!.args[{arg_index}]!.ty,"
        );
        let _ = writeln!(out, "      }},");
    }
    let _ = writeln!(out, "    ],");
    let _ = writeln!(
        out,
        "    returnType: spec.interfaces[{iface_index}]!.methods[{method_index}]!.return_type,"
    );
    let _ = writeln!(out, "    throws: [");
    for (exception_index, exception) in iface.throws.iter().enumerate() {
        let _ = writeln!(out, "      {{");
        let _ = writeln!(out, "        selector: 0x{:x}n,", exception.selector);
        let _ = writeln!(out, "        name: {},", exception_name(&exception.ty)?);
        let _ = writeln!(
            out,
            "        type: spec.interfaces[{iface_index}]!.throws[{exception_index}]!.ty,"
        );
        let _ = writeln!(out, "      }},");
    }
    let _ = writeln!(out, "    ],");
    let _ = writeln!(out, "    codecs,");
    let _ = writeln!(out, "  }}),");
    Ok(())
}

/// Emit per-event interface/const pairs, the per-contract event union, and
/// the combined `decodeEvent` dispatcher.
fn emit_event_sections(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let items = spec
        .events
        .iter()
        .enumerate()
        .map(|(idx, event)| Ok((ts_id_path(&event.name)?, (event, idx))))
        .collect::<Result<Vec<_>, CliError>>()?;
    emit_namespaced(out, &items, |(event, idx)| {
        let mut decl = String::new();
        emit_event_decl(&mut decl, event, *idx)?;
        Ok(decl)
    })?;

    let _ = writeln!(out);
    emit_decode_event(out, spec)?;
    Ok(())
}

fn emit_event_decl(out: &mut String, event: &ContractEventDef, idx: usize) -> Result<(), CliError> {
    let name = spec_string_text(&event.name.declared)?;
    let mut topic_fields = Vec::new();
    let mut filter_fields = Vec::new();
    for field in event.topics.iter() {
        let fname = spec_string_text(&field.name)?;
        let ty = event_topic_type(&field.ty);
        let documentation = if event_topic_representation(&field.ty) == "hash" {
            concat!(
                "/** Hash of the ABI-encoded topic value; ",
                "the original value cannot be recovered or directly filtered by this binding. */ ",
            )
        } else {
            ""
        };
        topic_fields.push(format!("{documentation}{}: {ty}", ts_property_key(fname)?));
        if event_topic_representation(&field.ty) != "hash" {
            filter_fields.push(format!(
                "  {}?: ContractEventFilterValue<{ty}>;",
                ts_property_key(fname)?
            ));
        }
    }
    let mut payload_fields = Vec::new();
    for field in event.fields.iter() {
        let fname = spec_string_text(&field.name)?;
        payload_fields.push(format!(
            "{}: {}",
            ts_property_key(fname)?,
            ts_type_ref(&field.ty)?
        ));
    }

    let _ = writeln!(out, "export interface {name} extends DecodedContractEvent<");
    let _ = writeln!(out, "  {},", ts_string_literal(name));
    let _ = writeln!(out, "  {{ {} }},", topic_fields.join("; "));
    let _ = writeln!(out, "  {{ {} }}", payload_fields.join("; "));
    let _ = writeln!(out, "> {{}}");
    let _ = writeln!(out);
    if filter_fields.is_empty() {
        let _ = writeln!(out, "export type {name}Filter = Record<string, never>;");
    } else {
        let _ = writeln!(out, "export interface {name}Filter {{");
        for field in filter_fields {
            let _ = writeln!(out, "{field}");
        }
        let _ = writeln!(out, "}}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "export const {name} = defineContractEvent<");
    let _ = writeln!(out, "  {},", ts_string_literal(name));
    let _ = writeln!(out, "  {name}[\"topics\"],");
    let _ = writeln!(out, "  {name}[\"fields\"],");
    let _ = writeln!(out, "  {name}Filter");
    let _ = writeln!(out, ">({{");
    let _ = writeln!(out, "  name: {},", ts_string_literal(name));
    let _ = writeln!(out, "  signature: spec.events[{idx}]!.signature,");
    let _ = writeln!(out, "  topics: [");
    for (field_index, field) in event.topics.iter().enumerate() {
        let fname = spec_string_text(&field.name)?;
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      name: {},", ts_string_literal(fname));
        let _ = writeln!(
            out,
            "      type: spec.events[{idx}]!.topics[{field_index}]!.ty,"
        );
        let _ = writeln!(
            out,
            "      representation: {},",
            ts_string_literal(event_topic_representation(&field.ty))
        );
        let _ = writeln!(out, "    }},");
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  fields: [");
    for (field_index, field) in event.fields.iter().enumerate() {
        let fname = spec_string_text(&field.name)?;
        let _ = writeln!(
            out,
            "    {{ name: {}, type: spec.events[{idx}]!.fields[{field_index}]!.ty }},",
            ts_string_literal(fname)
        );
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  codecs,");
    let _ = writeln!(out, "}});");
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

    // Qualified, because the declarations now live inside their namespaces.
    let event_names: Vec<String> = spec
        .events
        .iter()
        .map(|e| ts_id_ref(&e.name))
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

#[expect(
    clippy::too_many_lines,
    reason = "validation keeps emitted-name reservation and contextual ABI checks in declaration order"
)]
fn validate_spec(spec: &ContractSpec, public_method_names: &[Vec<String>]) -> Result<(), CliError> {
    let mut top_level_values = BTreeMap::new();
    let has_methods = spec
        .interfaces
        .iter()
        .any(|iface| !iface.methods.is_empty());
    let has_events = !spec.events.is_empty();
    let has_contract_types = spec_uses_type(spec, |_| true);
    let uses_address = spec_uses_type(spec, |ty| matches!(ty, ContractType::Address));
    let uses_fixed_bytes = spec_uses_type(
        spec,
        |ty| matches!(ty, ContractType::Array { array } if matches!(array.element.as_ref(), ContractType::U8)),
    );
    let uses_fixed_array = spec_uses_type(
        spec,
        |ty| matches!(ty, ContractType::Array { array } if array.len > 3 && !matches!(array.element.as_ref(), ContractType::U8)),
    );
    let uses_signed_token =
        spec_uses_type(spec, |ty| matches!(ty, ContractType::SignedToken { .. }));
    let uses_uint8_array = has_events
        || spec_uses_type(spec, |ty| {
            matches!(ty, ContractType::Bytes)
                || matches!(ty, ContractType::Vector { vector } if matches!(vector.element.as_ref(), ContractType::U8))
        });
    let uses_record = spec.events.iter().any(|event| {
        event
            .topics
            .iter()
            .all(|field| event_topic_representation(&field.ty) == "hash")
    });
    // Namespacing relocated the flat file scope's collisions rather than
    // removing them. TypeScript resolves the first segment of a reference
    // lexically, innermost enclosing scope outwards, so a name that lands
    // inside a namespace captures every reference to that name made from
    // within it. Three shadowers, one rule: a name nested inside a namespace
    // must not be one the declarations in there resolve lexically. Each case
    // otherwise emits a binding that reads correctly and does not type-check,
    // so the name is rejected rather than silently shadowed.
    //
    // The crate root is exempt: it sits at the file's top level, where the
    // flat reservations below already cover it.
    let mut captured: BTreeMap<&str, &str> = BTreeMap::new();
    if uses_uint8_array {
        captured.insert("Uint8Array", "the standard library's `Uint8Array`");
    }
    if uses_record {
        captured.insert(
            "Record",
            "the standard library's `Record`, which a topic-only event's filter aliases",
        );
    }
    if uses_address {
        captured.insert("ContractAddress", "the runtime's `ContractAddress` import");
    }
    if uses_fixed_array {
        captured.insert("FixedArray", "the runtime's `FixedArray` import");
    }
    if uses_fixed_bytes {
        captured.insert("FixedBytes", "the runtime's `FixedBytes` import");
    }
    if uses_signed_token {
        captured.insert("SignedToken", "the runtime's `SignedToken` import");
    }
    if has_events {
        captured.insert(
            "DecodedContractEvent",
            "the runtime's `DecodedContractEvent` import",
        );
        captured.insert(
            "defineContractEvent",
            "the runtime's `defineContractEvent` import",
        );
        if spec.events.iter().any(|event| {
            event
                .topics
                .iter()
                .any(|field| event_topic_representation(&field.ty) != "hash")
        }) {
            captured.insert(
                "ContractEventFilterValue",
                "the runtime's `ContractEventFilterValue` import",
            );
        }
    }

    // A reference to a named type is written from its crate's root down, even
    // within that same crate, and the root resolves lexically like any other
    // first segment. So a module named after a crate the spec references
    // captures it, and every reference to that crate's types made from inside
    // resolves into the wrong namespace.
    let referenced_roots = RefCell::new(BTreeSet::new());
    spec_uses_type(spec, |ty| {
        if let ContractType::Named { named } = ty
            && let Ok(path) = ts_id_path(named)
        {
            referenced_roots
                .borrow_mut()
                .insert(path.namespaces[0].clone());
        }
        false
    });
    let referenced_roots = referenced_roots.into_inner();

    let mut declared: Vec<(TsIdPath, String)> = Vec::new();
    for def in spec.type_defs.iter() {
        let path = ts_id_path(type_def_id(def)?)?;
        let what = format!("type definition \"{}\"", path.qualified());
        declared.push((path, what));
    }
    for event in spec.events.iter() {
        let path = ts_id_path(&event.name)?;
        let what = format!("event \"{}\"", path.qualified());
        declared.push((path, what));
    }

    let mut namespace_paths: BTreeSet<String> = BTreeSet::new();
    let mut declaration_paths: BTreeMap<String, String> = BTreeMap::new();
    for (path, what) in &declared {
        for nested in path
            .namespaces
            .iter()
            .skip(1)
            .chain(std::iter::once(&path.name))
        {
            if let Some(captures) = captured.get(nested.as_str()) {
                return Err(CliError::SpecFailed(format!(
                    "{what} puts \"{nested}\" inside a generated namespace, where it captures \
                     {captures}; rename it"
                )));
            }
            if referenced_roots.contains(nested.as_str()) {
                return Err(CliError::SpecFailed(format!(
                    "{what} puts \"{nested}\" inside a generated namespace, where it captures \
                     the \"{nested}\" crate's own namespace and breaks every reference to that \
                     crate's types made from inside; rename it"
                )));
            }
        }

        let mut prefix = String::new();
        for ns in &path.namespaces {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(ns);
            namespace_paths.insert(prefix.clone());
        }
        declaration_paths
            .entry(path.qualified())
            .or_insert_with(|| what.clone());
    }

    // A declaration that lands where a namespace has to be opened: an
    // `export type foo` beside an `export namespace foo` is a duplicate
    // identifier, because a type alias cannot merge with a namespace the way
    // an interface can. The two come from different spec ids -- a union
    // declared `foo` at a crate's root and a type in that crate's module
    // `foo` -- so the per-path reservations below see no clash.
    for (qualified, what) in &declaration_paths {
        if namespace_paths.contains(qualified) {
            return Err(CliError::SpecFailed(format!(
                "generated TypeScript declaration for {what} lands at \"{qualified}\", where \
                 another spec type's module needs a namespace of that name; rename the type or \
                 move it out of that module"
            )));
        }
    }

    for name in ["contractSpecString", "ContractSpec", "s", "spec"] {
        reserve_ts_value_name(
            &mut top_level_values,
            name.to_owned(),
            "generated binding".into(),
        )?;
    }
    for name in uses_uint8_array
        .then_some("Uint8Array")
        .into_iter()
        .chain(uses_record.then_some("Record"))
    {
        reserve_ts_value_name(
            &mut top_level_values,
            name.into(),
            "TypeScript standard library".into(),
        )?;
    }
    let mut conditional_imports = Vec::new();
    if has_methods {
        conditional_imports.extend([
            "contractEntrypointID",
            "defineContractMethod",
            "entrypoints",
        ]);
    }
    if has_methods || has_events {
        conditional_imports.extend(["fromHex", "createContractCodecContext", "codecs"]);
    }
    if has_contract_types {
        conditional_imports.push("ContractTypeKind");
    }
    if !spec.type_defs.is_empty() {
        conditional_imports.push("ContractTypeDefKind");
    }
    if !spec.interfaces.is_empty() {
        conditional_imports.push("ContractException");
    }
    if uses_address {
        conditional_imports.push("ContractAddress");
    }
    if uses_fixed_array {
        conditional_imports.push("FixedArray");
    }
    if uses_fixed_bytes {
        conditional_imports.push("FixedBytes");
    }
    if uses_signed_token {
        conditional_imports.push("SignedToken");
    }
    for name in conditional_imports {
        reserve_ts_value_name(
            &mut top_level_values,
            name.into(),
            "generated binding import".into(),
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

    if has_events {
        let has_direct_event_topics = spec.events.iter().any(|event| {
            event
                .topics
                .iter()
                .any(|field| event_topic_representation(&field.ty) != "hash")
        });
        let mut event_names = vec![
            "topic",
            "defineContractEvent",
            "DecodedContractEvent",
            "decodeEvent",
        ];
        if has_direct_event_topics {
            event_names.push("ContractEventFilterValue");
        }
        for name in event_names {
            reserve_ts_value_name(
                &mut top_level_values,
                name.into(),
                "generated event support".into(),
            )?;
        }
    }

    // A type def occupies a place inside its namespaces, not a top-level name,
    // so the only top-level name it claims is the root of its id -- the crate
    // ident. Several defs legitimately share one, which is the entire point of
    // namespacing them, so a root is reserved once rather than per def.
    // Namespacing moves the collision question inside a namespace: two crates
    // may both declare `Error`, but a type def and an event that land on one
    // qualified path still clash -- TypeScript would merge the declarations.
    // So names are reserved per qualified path, and the only top-level name a
    // def or event claims is the root of its id.
    let mut defined_types = BTreeSet::new();
    let mut reserved_roots = BTreeSet::new();
    let mut namespaced_values: BTreeMap<String, String> = BTreeMap::new();
    for def in spec.type_defs.iter() {
        let id = type_def_id(def)?;
        let path = ts_id_path(id)?;
        let root = path.namespaces[0].clone();
        if reserved_roots.insert(root.clone()) {
            reserve_ts_value_name(
                &mut top_level_values,
                root.clone(),
                format!("spec id namespace \"{root}\""),
            )?;
        }
        let qualified = path.qualified();
        reserve_ts_value_name(
            &mut namespaced_values,
            qualified.clone(),
            format!("type definition \"{qualified}\""),
        )?;
        defined_types.insert(id.clone());
    }

    for (iface_index, iface) in spec.interfaces.iter().enumerate() {
        let iface_name = spec_string_text(&iface.name)?;
        validate_ts_identifier(iface_name, &format!("interface \"{iface_name}\""))?;
        reserve_ts_value_name(
            &mut top_level_values,
            iface_name.to_owned(),
            format!("interface \"{iface_name}\""),
        )?;
        let declared_exception = format!("{iface_name}DeclaredException");
        let exception = format!("{iface_name}Exception");
        for (name, source) in [
            (
                declared_exception,
                format!("interface \"{iface_name}\" exception union"),
            ),
            (
                exception,
                format!("interface \"{iface_name}\" public exception"),
            ),
        ] {
            validate_ts_identifier(&name, &source)?;
            reserve_ts_value_name(&mut top_level_values, name, source)?;
        }
        validate_interface_method_name_collisions(
            iface_name,
            iface,
            &public_method_names[iface_index],
        )?;
        let mut selectors = BTreeSet::new();
        for exception in iface.throws.iter() {
            if !selectors.insert(exception.selector) {
                return Err(CliError::SpecFailed(format!(
                    "interface \"{iface_name}\" has duplicate exception selector 0x{:x}",
                    exception.selector
                )));
            }
            validate_contract_type(
                &exception.ty,
                false,
                &defined_types,
                &format!(
                    "interface \"{iface_name}\" exception 0x{:x}",
                    exception.selector
                ),
            )?;
        }
        for (method_index, method) in iface.methods.iter().enumerate() {
            let method_name = spec_string_text(&method.name)?;
            validate_method_arg_name_collisions(method_name, method)?;
            let stem =
                method_type_stem(iface_name, &public_method_names[iface_index][method_index]);
            for suffix in ["Args", "Result"] {
                let name = format!("{stem}{suffix}");
                validate_ts_identifier(&name, &format!("method \"{iface_name}.{method_name}\""))?;
                reserve_ts_value_name(
                    &mut top_level_values,
                    name,
                    format!("method \"{iface_name}.{method_name}\" {suffix} type"),
                )?;
            }
            for arg in method.args.iter() {
                let arg_name = spec_string_text(&arg.name)?;
                validate_contract_type(
                    &arg.ty,
                    false,
                    &defined_types,
                    &format!("method \"{iface_name}.{method_name}\" argument \"{arg_name}\""),
                )?;
            }
            validate_contract_type(
                &method.return_type,
                true,
                &defined_types,
                &format!("method \"{iface_name}.{method_name}\" return type"),
            )?;
        }
    }

    // Keyed on the whole id: a bare declared name conflates types the spec
    // keeps apart, and would also make the cycle guard below trip on two
    // unrelated same-named types.
    let type_def_index: BTreeMap<&ContractSpecId, &ContractTypeDef> = spec
        .type_defs
        .iter()
        .filter_map(|def| type_def_id(def).ok().map(|id| (id, def)))
        .collect();
    let mut event_signatures = BTreeMap::new();
    for event in spec.events.iter() {
        let event_name = spec_string_text(&event.name.declared)?;
        validate_ts_identifier(event_name, &format!("event \"{event_name}\""))?;
        // Like a type def, an event declares itself inside its id's
        // namespaces, so the only top-level name it claims is the root.
        let event_path = ts_id_path(&event.name)?;
        let event_root = event_path.namespaces[0].clone();
        if reserved_roots.insert(event_root.clone()) {
            reserve_ts_value_name(
                &mut top_level_values,
                event_root.clone(),
                format!("spec id namespace \"{event_root}\""),
            )?;
        }
        let qualified = event_path.qualified();
        for (name, source) in [
            (qualified.clone(), format!("event \"{qualified}\"")),
            (
                format!("{qualified}Filter"),
                format!("event \"{qualified}\" filter"),
            ),
        ] {
            reserve_ts_value_name(&mut namespaced_values, name, source)?;
        }
        if let Some(existing) =
            event_signatures.insert(event.signature.into_inner(), event_name.to_owned())
        {
            return Err(CliError::SpecFailed(format!(
                "events \"{existing}\" and \"{event_name}\" have duplicate signature 0x{}",
                hex::encode(event.signature.into_inner())
            )));
        }
        if event.topics.len() > 2 {
            return Err(CliError::SpecFailed(format!(
                "event \"{event_name}\" declares {} topics including its signature; protocol maximum is 3",
                event.topics.len() + 1
            )));
        }
        validate_fields(
            event.topics.iter(),
            &defined_types,
            &format!("event \"{event_name}\" topics"),
        )?;
        validate_fields(
            event.fields.iter(),
            &defined_types,
            &format!("event \"{event_name}\" fields"),
        )?;
        // A token's handle never travels in event data, and the event
        // tooling rejects one; refuse here rather than emit an event spec it
        // cannot decode. Named definitions are resolved so a token nested in
        // a struct or union the event refers to is caught too.
        for field in event.topics.iter().chain(event.fields.iter()) {
            if type_carries_signed_token(&field.ty, &type_def_index, &mut BTreeSet::new()) {
                return Err(CliError::SpecFailed(format!(
                    "event \"{event_name}\" field \"{}\" carries a signed token, which events cannot log",
                    spec_string_text(&field.name)?
                )));
            }
        }
    }

    if !spec.events.is_empty() {
        let union_name = event_union_name(spec)?;
        validate_ts_identifier(&union_name, "generated event union")?;
        reserve_ts_value_name(
            &mut top_level_values,
            union_name.clone(),
            format!("generated \"{union_name}\" event union"),
        )?;
    }

    for def in spec.type_defs.iter() {
        validate_type_def(def, &defined_types)?;
    }

    Ok(())
}

fn public_method_names_by_interface(spec: &ContractSpec) -> Result<Vec<Vec<String>>, CliError> {
    spec.interfaces
        .iter()
        .map(|iface| {
            iface
                .methods
                .iter()
                .map(|method| {
                    let method_name = spec_string_text(&method.name)?;
                    Ok(to_camel_case(method_name))
                })
                .collect::<Result<Vec<_>, CliError>>()
        })
        .collect()
}

fn validate_interface_method_name_collisions(
    iface_name: &str,
    iface: &ContractSpecInterface,
    public_method_names: &[String],
) -> Result<(), CliError> {
    let mut methods: BTreeMap<&str, &str> = BTreeMap::new();
    for (method_index, method) in iface.methods.iter().enumerate() {
        let method_name = spec_string_text(&method.name)?;
        let public_method_name = public_method_names[method_index].as_str();
        validate_ts_property_identifier(
            public_method_name,
            &format!("method \"{iface_name}.{method_name}\""),
        )?;
        if let Some(existing) = methods.get(public_method_name) {
            return Err(CliError::SpecFailed(format!(
                "interface \"{iface_name}\" method names \"{existing}\" and \"{method_name}\" both map to \"{public_method_name}\""
            )));
        }
        methods.insert(public_method_name, method_name);
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

fn validate_ts_identifier(name: &str, source: &str) -> Result<(), CliError> {
    if !is_ts_identifier(name) || name == "__proto__" {
        return Err(CliError::SpecFailed(format!(
            "generated TypeScript identifier \"{name}\" for {source} is invalid"
        )));
    }
    Ok(())
}

fn validate_ts_property_identifier(name: &str, source: &str) -> Result<(), CliError> {
    if !is_ts_identifier_name(name) || name == "__proto__" {
        return Err(CliError::SpecFailed(format!(
            "generated TypeScript property name \"{name}\" for {source} is invalid"
        )));
    }
    Ok(())
}

fn is_ts_identifier(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "arguments",
        "any",
        "await",
        "bigint",
        "boolean",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "eval",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "infer",
        "instanceof",
        "interface",
        "keyof",
        "let",
        "new",
        "never",
        "null",
        "number",
        "object",
        "package",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "string",
        "super",
        "switch",
        "symbol",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "undefined",
        "unique",
        "unknown",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ];
    if RESERVED.contains(&name) {
        return false;
    }
    is_ts_identifier_name(name)
}

fn is_ts_identifier_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first == '$' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn validate_type_def(
    def: &ContractTypeDef,
    defined_types: &BTreeSet<ContractSpecId>,
) -> Result<(), CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => {
            let name = spec_string_text(&struct_def.name.declared)?;
            validate_fields(
                struct_def.fields.iter(),
                defined_types,
                &format!("struct \"{name}\""),
            )
        }
        ContractTypeDef::TypeDefEnum { enum_def } => {
            let name = spec_string_text(&enum_def.name.declared)?;
            let mut members = BTreeSet::new();
            let mut values = BTreeSet::new();
            for variant in enum_def.variants.iter() {
                let variant_name = spec_string_text(&variant.name)?;
                validate_property_name(variant_name, &format!("enum \"{name}\""))?;
                if !members.insert(variant_name) {
                    return Err(CliError::SpecFailed(format!(
                        "enum \"{name}\" has duplicate member \"{variant_name}\""
                    )));
                }
                if !values.insert(variant.value) {
                    return Err(CliError::SpecFailed(format!(
                        "enum \"{name}\" has duplicate value {}",
                        variant.value
                    )));
                }
            }
            Ok(())
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            let name = spec_string_text(&union_def.name.declared)?;
            if union_def.cases.is_empty() {
                return Err(CliError::SpecFailed(format!(
                    "union \"{name}\" must declare at least one case"
                )));
            }
            let mut members = BTreeSet::new();
            let mut discriminants = BTreeSet::new();
            for case in union_def.cases.iter() {
                let case_name = spec_string_text(&case.name)?;
                if !members.insert(case_name) {
                    return Err(CliError::SpecFailed(format!(
                        "union \"{name}\" has duplicate case \"{case_name}\""
                    )));
                }
                if !discriminants.insert(case.discriminant) {
                    return Err(CliError::SpecFailed(format!(
                        "union \"{name}\" has duplicate discriminant {}",
                        case.discriminant
                    )));
                }
                match &case.case_type {
                    ContractUnionCaseType::UnionCaseVoid => {}
                    ContractUnionCaseType::UnionCaseTuple { tuple } => {
                        for (index, ty) in tuple.iter().enumerate() {
                            validate_contract_type(
                                ty,
                                false,
                                defined_types,
                                &format!(
                                    "union \"{name}\" case \"{case_name}\" tuple item {index}"
                                ),
                            )?;
                        }
                    }
                    ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                        validate_fields(
                            struct_fields.iter(),
                            defined_types,
                            &format!("union \"{name}\" case \"{case_name}\""),
                        )?;
                    }
                    _ => {
                        return Err(CliError::SpecFailed(format!(
                            "union \"{name}\" case \"{case_name}\" has an unsupported kind"
                        )));
                    }
                }
            }
            Ok(())
        }
        _ => Err(CliError::SpecFailed(
            "type definition contains an unsupported kind".into(),
        )),
    }
}

fn validate_fields<'a>(
    fields: impl Iterator<Item = &'a ContractField>,
    defined_types: &BTreeSet<ContractSpecId>,
    location: &str,
) -> Result<(), CliError> {
    let mut names = BTreeSet::new();
    for field in fields {
        let name = spec_string_text(&field.name)?;
        validate_property_name(name, location)?;
        if !names.insert(name) {
            return Err(CliError::SpecFailed(format!(
                "{location} has duplicate field \"{name}\""
            )));
        }
        validate_contract_type(
            &field.ty,
            false,
            defined_types,
            &format!("{location} field \"{name}\""),
        )?;
    }
    Ok(())
}

fn validate_contract_type(
    ty: &ContractType,
    allow_void: bool,
    defined_types: &BTreeSet<ContractSpecId>,
    location: &str,
) -> Result<(), CliError> {
    match ty {
        ContractType::Void if allow_void => Ok(()),
        ContractType::Void => Err(CliError::SpecFailed(format!(
            "{location} uses Void outside a method return"
        ))),
        ContractType::Bool
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
        | ContractType::Address => Ok(()),
        ContractType::Option { option_inner } => {
            validate_contract_type(option_inner, false, defined_types, location)
        }
        ContractType::Vector { vector } => {
            validate_contract_type(&vector.element, false, defined_types, location)
        }
        ContractType::Array { array } => {
            validate_contract_type(&array.element, false, defined_types, location)
        }
        ContractType::Tuple { tuple } => {
            for (index, item) in tuple.iter().enumerate() {
                validate_contract_type(
                    item,
                    false,
                    defined_types,
                    &format!("{location} tuple item {index}"),
                )?;
            }
            Ok(())
        }
        ContractType::Named { named } => {
            // Resolved through the whole id, the same way the generated
            // binding resolves it: a bare declared name matches types the
            // spec keeps apart.
            if defined_types.contains(named) {
                Ok(())
            } else {
                Err(CliError::SpecFailed(format!(
                    "{location} references unresolved named type \"{}\"",
                    groundhog_client::codec::spec_id_text(named)?
                )))
            }
        }
        ContractType::SignedToken {
            signed_token_preimage,
        } => match signed_token_preimage.preimage_type.as_deref() {
            Some(inner) => validate_contract_type(
                inner,
                false,
                defined_types,
                &format!("{location} signed-token preimage"),
            ),
            None => Ok(()),
        },
        _ => Err(CliError::SpecFailed(format!(
            "{location} uses an unsupported contract type"
        ))),
    }
}

fn validate_property_name(name: &str, location: &str) -> Result<(), CliError> {
    if name == "__proto__" {
        return Err(CliError::SpecFailed(format!(
            "{location} contains unsafe property name \"{name}\""
        )));
    }
    Ok(())
}

fn method_type_stem(interface_name: &str, public_method_name: &str) -> String {
    let mut chars = public_method_name.chars();
    let Some(first) = chars.next() else {
        return interface_name.to_owned();
    };
    format!(
        "{interface_name}{}{}",
        first.to_ascii_uppercase(),
        chars.collect::<String>()
    )
}

/// The exception's name as a TypeScript string literal.
///
/// A display position, not spec data: this lands in type position on
/// `<Interface>DeclaredException`, where consumers discriminate on it. The
/// declared segment is what a person reads; the whole id travels in the spec
/// literal beside it.
fn exception_name(ty: &ContractType) -> Result<String, CliError> {
    if let ContractType::Named { named } = ty {
        Ok(ts_string_literal(spec_string_text(&named.declared)?))
    } else {
        Ok("null".into())
    }
}

fn event_topic_representation(ty: &ContractType) -> &'static str {
    match ty {
        ContractType::Address => "address",
        ContractType::U32 => "u32",
        ContractType::U64 => "u64",
        ContractType::U128 => "u128",
        ContractType::U256 => "u256",
        ContractType::I32 => "i32",
        ContractType::I64 => "i64",
        ContractType::I128 => "i128",
        ContractType::I256 => "i256",
        ContractType::Bool => "bool",
        ContractType::Array { array }
            if array.len == 32 && matches!(array.element.as_ref(), ContractType::U8) =>
        {
            "fixedBytes32"
        }
        _ => "hash",
    }
}

fn event_topic_type(ty: &ContractType) -> String {
    match event_topic_representation(ty) {
        "address" => "ContractAddress".into(),
        "fixedBytes32" => "FixedBytes<32>".into(),
        "u32" | "u64" | "u128" | "u256" | "i32" | "i64" | "i128" | "i256" => "bigint".into(),
        "bool" => "boolean".into(),
        _ => "Uint8Array".into(),
    }
}

fn spec_uses_type(spec: &ContractSpec, predicate: impl Fn(&ContractType) -> bool + Copy) -> bool {
    spec.interfaces.iter().any(|interface| {
        interface
            .throws
            .iter()
            .any(|exception| contract_type_matches(&exception.ty, predicate))
            || interface.methods.iter().any(|method| {
                contract_type_matches(&method.return_type, predicate)
                    || method
                        .args
                        .iter()
                        .any(|arg| contract_type_matches(&arg.ty, predicate))
            })
    }) || spec.type_defs.iter().any(|def| match def {
        ContractTypeDef::TypeDefStruct { struct_def } => struct_def
            .fields
            .iter()
            .any(|field| contract_type_matches(&field.ty, predicate)),
        ContractTypeDef::TypeDefUnion { union_def } => {
            union_def.cases.iter().any(|case| match &case.case_type {
                ContractUnionCaseType::UnionCaseTuple { tuple } => {
                    tuple.iter().any(|ty| contract_type_matches(ty, predicate))
                }
                ContractUnionCaseType::UnionCaseStruct { struct_fields } => struct_fields
                    .iter()
                    .any(|field| contract_type_matches(&field.ty, predicate)),
                _ => false,
            })
        }
        _ => false,
    }) || spec.events.iter().any(|event| {
        event
            .topics
            .iter()
            .chain(event.fields.iter())
            .any(|field| contract_type_matches(&field.ty, predicate))
    })
}

/// Whether `ty` contains a signed token anywhere, resolving named definitions
/// through `type_defs`. `visiting` holds the names on the current path, so a
/// recursive definition terminates: a token inside it is found on the first
/// visit or not at all.
fn type_carries_signed_token(
    ty: &ContractType,
    type_defs: &BTreeMap<&ContractSpecId, &ContractTypeDef>,
    visiting: &mut BTreeSet<ContractSpecId>,
) -> bool {
    match ty {
        ContractType::SignedToken { .. } => true,
        ContractType::Option { option_inner } => {
            type_carries_signed_token(option_inner, type_defs, visiting)
        }
        ContractType::Vector { vector } => {
            type_carries_signed_token(&vector.element, type_defs, visiting)
        }
        ContractType::Array { array } => {
            type_carries_signed_token(&array.element, type_defs, visiting)
        }
        ContractType::Tuple { tuple } => tuple
            .iter()
            .any(|item| type_carries_signed_token(item, type_defs, visiting)),
        ContractType::Named { named } => {
            if !visiting.insert(named.clone()) {
                return false;
            }
            let found = match type_defs.get(named) {
                Some(ContractTypeDef::TypeDefStruct { struct_def }) => struct_def
                    .fields
                    .iter()
                    .any(|field| type_carries_signed_token(&field.ty, type_defs, visiting)),
                Some(ContractTypeDef::TypeDefUnion { union_def }) => {
                    union_def.cases.iter().any(|case| match &case.case_type {
                        ContractUnionCaseType::UnionCaseTuple { tuple } => tuple
                            .iter()
                            .any(|item| type_carries_signed_token(item, type_defs, visiting)),
                        ContractUnionCaseType::UnionCaseStruct { struct_fields } => struct_fields
                            .iter()
                            .any(|field| type_carries_signed_token(&field.ty, type_defs, visiting)),
                        _ => false,
                    })
                }
                _ => false,
            };
            visiting.remove(named);
            found
        }
        _ => false,
    }
}

fn contract_type_matches(
    ty: &ContractType,
    predicate: impl Fn(&ContractType) -> bool + Copy,
) -> bool {
    predicate(ty)
        || match ty {
            ContractType::Option { option_inner } => contract_type_matches(option_inner, predicate),
            ContractType::Vector { vector } => contract_type_matches(&vector.element, predicate),
            ContractType::Array { array } => contract_type_matches(&array.element, predicate),
            ContractType::Tuple { tuple } => tuple
                .iter()
                .any(|item| contract_type_matches(item, predicate)),
            ContractType::SignedToken {
                signed_token_preimage,
            } => signed_token_preimage
                .preimage_type
                .as_deref()
                .is_some_and(|inner| contract_type_matches(inner, predicate)),
            _ => false,
        }
}

fn ts_property_key(name: &str) -> Result<String, CliError> {
    validate_property_name(name, "emitted TypeScript object")?;
    Ok(ts_string_literal(name))
}

/// Read a spec string as text for emission; accepts any UTF-8, unlike the codec's validator.
fn spec_string_text(value: &groundhog_contract_spec::ContractSpecString) -> Result<&str, CliError> {
    std::str::from_utf8(value.as_ref()).map_err(|error| {
        CliError::SpecFailed(format!("contract spec contains invalid UTF-8: {error}"))
    })
}

fn ts_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn ts_line_comment_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\u{2028}' => "\\u2028".to_owned(),
            '\u{2029}' => "\\u2029".to_owned(),
            ch if ch.is_control() => "?".to_owned(),
            ch => ch.to_string(),
        })
        .collect()
}

fn validate_method_arg_name_collisions(
    method_name: &str,
    method: &ContractSpecMethod,
) -> Result<(), CliError> {
    let mut args: BTreeMap<String, &str> = BTreeMap::new();
    for arg in method.args.iter() {
        let arg_name = spec_string_text(&arg.name)?;
        let public_arg_name = to_camel_case(arg_name);
        validate_ts_property_identifier(
            &public_arg_name,
            &format!("method \"{method_name}\" argument \"{arg_name}\""),
        )?;
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
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut uppercase_next = false;

    for (i, ch) in chars.iter().enumerate() {
        if *ch == '_' {
            let has_prior_word = chars[..i].iter().any(|ch| *ch != '_');
            let has_later_word = chars[i + 1..].iter().any(|ch| *ch != '_');
            if has_prior_word && has_later_word {
                uppercase_next = true;
            } else {
                out.push('_');
                uppercase_next = false;
            }
        } else if uppercase_next {
            out.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            out.push(*ch);
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

    /// A fixture id: one crate, one module, so the declared name is what
    /// tells these types apart -- which is what these tests turn on. The
    /// generated output takes the declared segment, so it reads as before.
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
    fn maps_signed_token_types() {
        use groundhog_contract_spec::ContractSignedTokenPreimage;
        let opaque = ContractType::SignedToken {
            signed_token_preimage: ContractSignedTokenPreimage {
                preimage_type: None,
            },
        };
        assert_eq!(ts_type_ref(&opaque).unwrap(), "SignedToken");
        assert_eq!(
            spec_type_to_ts_literal(&opaque).unwrap(),
            "{ v: ContractTypeKind.SignedToken, signed_token_preimage: { preimage_type: null } }"
        );

        let typed = ContractType::SignedToken {
            signed_token_preimage: ContractSignedTokenPreimage {
                preimage_type: Some(Box::new(ContractType::Named {
                    named: spec_id("Auth"),
                })),
            },
        };
        assert_eq!(ts_type_ref(&typed).unwrap(), "SignedToken<demo.v1.Auth>");
        assert_eq!(
            spec_type_to_ts_literal(&typed).unwrap(),
            "{ v: ContractTypeKind.SignedToken, signed_token_preimage: \
             { preimage_type: { v: ContractTypeKind.Named, named: { crate_name: \
             s(\"demo\"), major: s(\"1\"), module_path: s(\"demo\"), declared: \
             s(\"Auth\") } } } }"
        );

        let list = ContractType::Vector {
            vector: ContractVectorType {
                element: Box::new(opaque),
                max_len: u32::MAX,
            },
        };
        assert_eq!(ts_type_ref(&list).unwrap(), "SignedToken[]");
    }

    #[test]
    fn generates_signed_token_returns_and_imports_the_type() {
        use groundhog_contract_spec::ContractSignedTokenPreimage;
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
            }],
            false,
        );
        let output = generate_ts_bindings(&spec, "wallet.spec.xdr").unwrap();
        assert!(
            output.contains("type SignedToken } from \"@groundhogchain/client/contract\""),
            "SignedToken must be imported from the client runtime:\n{output}"
        );
        assert!(
            output.contains("export type CounterAuthorizeResult = SignedToken[];"),
            "return type must be a token placeholder list:\n{output}"
        );
        assert!(
            output.contains(
                "{ v: ContractTypeKind.SignedToken, signed_token_preimage: { preimage_type: null } }"
            ),
            "spec literal must carry the signed-token type:\n{output}"
        );
    }

    #[test]
    fn rejects_signed_tokens_in_event_payloads() {
        use groundhog_contract_spec::ContractSignedTokenPreimage;
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
            let err = generate_ts_bindings(&spec, "wallet.spec.xdr").unwrap_err();
            assert!(
                err.to_string().contains("carries a signed token"),
                "{label}: {err}"
            );
        }
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
    fn rejects_a_type_named_signed_token_when_the_import_is_needed() {
        use groundhog_contract_spec::ContractSignedTokenPreimage;
        let mut spec = counter_spec_with_methods(
            vec![ContractSpecMethod {
                name: spec_string("authorize"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::SignedToken {
                    signed_token_preimage: ContractSignedTokenPreimage {
                        preimage_type: None,
                    },
                },
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
        // Namespacing does not make this safe. The spec type nests, but the
        // runtime's `SignedToken` is referenced from inside that same
        // namespace, so the nested declaration captures it and the reference
        // resolves to the spec's own type.
        let error = generate_ts_bindings(&spec, "wallet.spec.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("captures the runtime's `SignedToken` import"),
            "unexpected error: {error}"
        );
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
        assert_eq!(ts_type_ref(&ty).unwrap(), "Uint8Array");
    }

    #[test]
    fn maps_array_of_u8_to_fixed_bytes() {
        let ty = ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::U8),
                len: 32,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "FixedBytes<32>");
    }

    #[test]
    fn maps_large_array_to_fixed_array() {
        let ty = ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::U32),
                len: 4,
            },
        };
        assert_eq!(ts_type_ref(&ty).unwrap(), "FixedArray<bigint, 4>");
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
        assert_eq!(ts_type_ref(&ty).unwrap(), "demo.v1.ERC20Error");
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
            "export interface TokenInfo { \"owner\": ContractAddress; \"supply\": bigint; \"frozen\": boolean; }"
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
            "export const ERC20Error = { \"AlreadyInitialized\": 1n, \"MustBeContractOwner\": 2n, } as const;\n\nexport type ERC20Error = (typeof ERC20Error)[keyof typeof ERC20Error];"
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

    /// An event with one topic field and one payload field.
    fn ping_event_def() -> ContractEventDef {
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
        let output = generate_ts_bindings(&spec_with_events(), "counter.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec_with_events(), "counter.spec.xdr").unwrap();

        assert!(
            output.contains("export interface PingEvent extends DecodedContractEvent<"),
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
            output.contains("\"tag\"?: ContractEventFilterValue<bigint>;"),
            "missing scalar topic filter:\n{output}"
        );
        assert!(
            output.contains("export const PingEvent = defineContractEvent<"),
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
                "export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): CounterEvent | null"
            ),
            "missing decodeEvent:\n{output}"
        );
        assert!(
            output.contains("return demo.v1.PingEvent.decode(event);"),
            "missing dispatch body:\n{output}"
        );
        assert!(
            output.contains("codecs,"),
            "missing shared codecs:\n{output}"
        );
        assert!(!output.contains("CxsReader"));
        assert!(!output.contains("resolveCodec"));
    }

    #[test]
    fn event_only_binding_defines_spec_string_helper() {
        let output = generate_ts_bindings(&spec_with_events(), "counter.spec.xdr").unwrap();

        assert!(output.contains("const s = contractSpecString;"));
        assert!(output.contains("name: s(\"Counter\")"));
    }

    #[test]
    fn no_event_section_without_events() {
        let output = generate_ts_bindings(&counter_spec(), "counter.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
        assert!(output.contains("export type CounterEvent = demo.v1.CounterEvent;"));
        assert!(output.contains("return demo.v1.CounterEvent.decode(event);"));
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
        let error = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(error.to_string().contains(
            "generated TypeScript value name \"demo.v1.PingEvent\" collides between type definition \"demo.v1.PingEvent\" and event \"demo.v1.PingEvent\""
        ));
    }

    #[test]
    fn generated_method_type_names_are_namespaced_apart_from_spec_types() {
        // Method argument and result types are emitted at top level. A spec
        // type declared with the same name used to collide with them; it now
        // lands inside its id's namespaces instead.
        for suffix in ["Args", "Result"] {
            let name = format!("ERC20Transfer{suffix}");
            let mut spec = erc20_spec();
            spec.type_defs.0.push(ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id(&name),
                    fields: vec![].try_into().unwrap(),
                },
            });

            let output = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap();
            // The generated one stays top level, the spec type is nested
            // inside its id's namespaces, so both can exist at once.
            assert!(output.contains(&format!("export interface {name} {{")));
            assert!(output.contains(&format!("    export interface {name} {{")));
        }
    }

    #[test]
    fn method_type_suffixes_allow_noncolliding_spec_types() {
        for name in ["TransferArgs", "TransferResult"] {
            let mut spec = erc20_spec();
            spec.type_defs.0.push(ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id(name),
                    fields: vec![ContractField {
                        name: spec_string("value"),
                        ty: ContractType::U64,
                    }]
                    .try_into()
                    .unwrap(),
                },
            });

            let output = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap();
            assert!(output.contains(&format!("export interface {name} {{")));
            assert!(output.contains("export interface ERC20TransferArgs {"));
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
                },
                ContractSpecMethod {
                    name: spec_string("getBalance"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U64,
                },
            ],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
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

        let output = generate_ts_bindings(&spec, "token.spec.xdr").unwrap();
        assert!(output.contains("export const ERC20 = {"));
        assert!(output.contains("export const ERC20Metadata = {"));
        assert!(output.contains("  initialize: defineContractMethod<"));
        assert!(output.contains("entrypoint: entrypoints.ERC20.initialize,"));
        assert!(output.contains("entrypoint: entrypoints.ERC20Metadata.initialize,"));
        assert!(output.contains("type: spec.interfaces[0]!.methods[0]!.args[0]!.ty"));
        assert!(output.contains("type: spec.interfaces[1]!.methods[0]!.args[0]!.ty"));
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

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(err.to_string().contains("ownerId"));
    }

    #[test]
    fn interface_name_colliding_with_import_errors() {
        let spec = counter_spec_with_interface_name(
            "fromHex",
            vec![ContractSpecMethod {
                name: spec_string("get"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::U64,
            }],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(err.to_string().contains("fromHex"));
    }

    #[test]
    fn interface_name_colliding_with_event_decoder_errors() {
        let spec = counter_spec_with_interface_name("decodeEvent", vec![], true);

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(err.to_string().contains("decodeEvent"));
    }

    #[test]
    fn named_type_shadowing_emitted_uint8_array_errors() {
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

        // The declaration lands at `demo.v1.Uint8Array`, in the very
        // namespace whose `bytes` fields are typed `Uint8Array`, so it
        // captures the global rather than sitting harmlessly beside it.
        let error = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("captures the standard library's `Uint8Array`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn event_shadowing_emitted_record_errors() {
        let mut spec = spec_with_events();
        let mut event = ping_event_def();
        event.name = spec_id("Record");
        event.topics.0[0].ty = ContractType::String;
        spec.events = vec![event].try_into().unwrap();

        // A topic-only event's filter aliases `Record<string, never>`, and
        // that alias is emitted in the same namespace as the event interface
        // -- so an event named `Record` captures it, leaving the filter
        // referring to a non-generic interface.
        let error = generate_ts_bindings(&spec, "events.spec.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("captures the standard library's `Record`"),
            "unexpected error: {error}"
        );
    }

    fn spec_id_at(krate: &str, module_path: &str, declared: &str) -> ContractSpecId {
        ContractSpecId {
            crate_name: spec_string(krate),
            major: spec_string("1"),
            module_path: spec_string(module_path),
            declared: spec_string(declared),
        }
    }

    /// A struct in `demo` holding a type declared in crate `other`.
    fn cross_crate_spec(holder_module: &str) -> ContractSpec {
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

    #[test]
    fn embeds_a_type_declared_in_another_crate() {
        let output = generate_ts_bindings(&cross_crate_spec("demo"), "x.spec.xdr").unwrap();

        // The reference is written from the other crate's root down, and that
        // root is a top-level namespace, so it resolves from inside `demo`.
        assert!(
            output.contains(r#"export interface Holder { "inner": other.v1.Thing; }"#),
            "cross-crate reference missing:\n{output}"
        );
        assert!(output.contains("export namespace other {"));
    }

    #[test]
    fn module_named_after_a_referenced_crate_errors() {
        // `Holder` moves into `demo::other`, so its own namespace is now named
        // `other`. The reference it carries still reads `other.v1.Thing`, but
        // `other` now resolves to the enclosing module namespace, which has no
        // `v1` -- the binding would not type-check.
        let error =
            generate_ts_bindings(&cross_crate_spec("demo::other"), "x.spec.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("captures the \"other\" crate's own namespace"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn type_alias_colliding_with_a_sibling_module_errors() {
        let mut spec = spec_with_events();
        spec.events = vec![].try_into().unwrap();
        spec.type_defs = vec![
            // A union renders as `export type foo = ...`.
            ContractTypeDef::TypeDefUnion {
                union_def: ContractUnionDef {
                    name: spec_id_at("demo", "demo", "foo"),
                    cases: vec![ContractUnionCase {
                        name: spec_string("Ok"),
                        discriminant: 0,
                        case_type: ContractUnionCaseType::UnionCaseVoid,
                    }]
                    .try_into()
                    .unwrap(),
                },
            },
            // ... and this one needs `export namespace foo` at the same level.
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id_at("demo", "demo::foo", "Bar"),
                    fields: vec![].try_into().unwrap(),
                },
            },
        ]
        .try_into()
        .unwrap();

        // A type alias cannot merge with a namespace the way an interface can.
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
        assert!(
            error.to_string().contains("needs a namespace of that name"),
            "unexpected error: {error}"
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

        let output = generate_ts_bindings(&spec, "reuse.spec.xdr").unwrap();
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

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(err.to_string().contains("erc20.v1"));
    }

    #[test]
    fn strict_mode_binding_names_are_rejected() {
        for name in ["eval", "arguments"] {
            let spec = counter_spec_with_interface_name(name, vec![], false);

            let error = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
            assert!(error.to_string().contains(name));
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
            }],
            false,
        );

        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
        assert!(output.contains("export interface CounterSymbolArgs"));
        assert!(output.contains("  return: string;"));
        assert!(output.contains("  symbol: defineContractMethod<"));
    }

    #[test]
    fn intrinsic_type_names_are_rejected() {
        for name in ["any", "bigint", "infer", "keyof", "string", "unknown"] {
            let spec = counter_spec_with_interface_name(name, vec![], false);

            let error = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn invalid_method_name_errors() {
        let spec = counter_spec_with_methods(
            vec![ContractSpecMethod {
                name: spec_string("transfer-from"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::Void,
            }],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(err.to_string().contains("transfer-from"));
    }

    #[test]
    fn proto_method_name_errors() {
        let spec = counter_spec_with_methods(
            vec![ContractSpecMethod {
                name: spec_string("__proto__"),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::Void,
            }],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
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
            }],
            false,
        );

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
        assert!(err.to_string().contains("owner-id"));
    }

    fn counter_spec_with_methods(
        methods: Vec<ContractSpecMethod>,
        include_events: bool,
    ) -> ContractSpec {
        counter_spec_with_interface_name("Counter", methods, include_events)
    }

    fn counter_spec_with_interface_name(
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
        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();

        // Header
        assert!(output.contains("DO NOT EDIT"));
        assert!(output.contains("counter.spec.xdr"));
        assert!(output.contains("Counter interface v0.1.0"));

        // Imports
        assert!(output.contains("@groundhogchain/protocol"));
        assert!(output.contains("@groundhogchain/client"));
        assert!(output.contains("@groundhogchain/contract-spec"));

        // Entrypoints block
        assert!(output.contains("const entrypoints = {"));
        assert!(output.contains("  Counter: {"));
        assert!(output.contains("} as const;"));

        // Spec literal
        assert!(output.contains("export const spec: ContractSpec"));

        assert!(output.contains("const codecs = createContractCodecContext(spec.type_defs);"));

        assert!(output.contains("export const Counter = {"));
        assert!(output.contains("export type CounterGetArgs = void;"));
        assert!(output.contains("export type CounterGetResult = bigint;"));
        assert!(output.contains("export interface CounterIncrementArgs"));
        assert!(output.contains("amount: bigint"));
        assert!(output.contains("export type CounterIncrementResult = void;"));
        assert!(output.contains("export type CounterDeclaredException = never;"));
        assert!(output.contains(
            "export type CounterException = ContractException<CounterDeclaredException>;"
        ));
        assert!(output.contains("  get: defineContractMethod<"));
        assert!(output.contains("  increment: defineContractMethod<"));
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

    /// ERC20-shaped contract with error enum, throws, and multiple methods.
    fn erc20_spec() -> ContractSpec {
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

    #[test]
    fn generates_erc20_bindings() {
        let spec = erc20_spec();
        let output = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap();

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
            output.contains("export const ERC20Error ="),
            "ERC20Error enum declaration missing"
        );
        assert!(
            output.contains("\"AlreadyInitialized\": 1n"),
            "AlreadyInitialized variant missing"
        );
        assert!(
            output.contains("\"Internal\": 100n"),
            "Internal variant missing"
        );
        assert!(output.contains("export type ERC20Error = (typeof ERC20Error)"));

        // Interface object methods
        assert!(
            output.contains("export const ERC20 = {"),
            "ERC20 interface object missing"
        );
        assert!(
            output.contains("  transfer: defineContractMethod<"),
            "transfer method missing"
        );
        assert!(
            output.contains("  getBalance: defineContractMethod<"),
            "getBalance method missing"
        );
        assert!(
            output.contains("  getBalanceOf: defineContractMethod<"),
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
            output.contains("export interface ERC20TransferArgs")
                && output.contains("to: ContractAddress")
                && output.contains("amount: bigint"),
            "transfer 'to' arg type mismatch"
        );
        assert!(
            output.contains("export interface ERC20GetBalanceOfArgs")
                && output.contains("accountId: ContractAddress"),
            "getBalanceOf 'accountId' arg type mismatch"
        );
        assert!(
            output.contains("entrypoint: entrypoints.ERC20.getBalanceOf"),
            "getBalanceOf should read the nested camelCase entrypoint key"
        );
        assert!(
            output.contains("\"get_balance_of\""),
            "source method name should be preserved in the spec"
        );
        assert!(
            output.contains("name: s(\"account_id\")"),
            "source arg name should be preserved in the spec literal"
        );
        assert!(output.contains("abiName: \"account_id\""));
        assert!(output.contains("publicName: \"accountId\""));
        assert!(output.contains("export type ERC20DeclaredException ="));
        assert!(output.contains("name: \"ERC20Error\";"));
        assert!(output.contains("value: demo.v1.ERC20Error;"));
        assert!(
            output.contains(
                "export type ERC20Exception = ContractException<ERC20DeclaredException>;"
            )
        );
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
                            named: spec_id("Tree"),
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

    #[test]
    fn generates_recursive_type_bindings() {
        let spec = recursive_type_spec();
        let output = generate_ts_bindings(&spec, "recursive.spec.xdr").unwrap();

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
            output.contains("\"left\": demo.v1.Tree | null;"),
            "Tree.left field missing"
        );
        assert!(
            output.contains("\"right\": demo.v1.Tree | null;"),
            "Tree.right field missing"
        );

        assert!(
            output.contains("export interface RecursiveTypeContractDepthArgs")
                && output.contains("tree: demo.v1.Tree"),
            "depth 'tree' arg type mismatch"
        );
    }

    #[test]
    fn expands_arrays_up_to_three_elements() {
        for (len, expected) in [
            (0, "readonly []"),
            (1, "readonly [bigint]"),
            (2, "readonly [bigint, bigint]"),
            (3, "readonly [bigint, bigint, bigint]"),
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

        let output = emit_type_def(&def).unwrap();
        assert!(output.contains("\"Negative\": -2n"));
        assert!(output.contains("\"Zero\": 0n"));
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

        let error = generate_ts_bindings(&spec, "duplicate-enum.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("enum \"DuplicateValues\" has duplicate value 1")
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

        let error = generate_ts_bindings(&spec, "duplicate-union.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("union \"DuplicateDiscriminants\" has duplicate discriminant -1")
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

        let error = generate_ts_bindings(&spec, "empty-union.xdr").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("union \"Empty\" must declare at least one case")
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

        let output = generate_ts_bindings(&spec, "events.spec.xdr").unwrap();
        assert!(output.contains("\"owner-address\": ContractAddress"));
        assert!(output.contains("\"id\": FixedBytes<32>"));
        assert!(output.contains("\"owner-address\"?: ContractEventFilterValue<ContractAddress>"));
        assert!(output.contains("\"id\"?: ContractEventFilterValue<FixedBytes<32>>"));
        assert!(output.contains("representation: \"address\""));
        assert!(output.contains("representation: \"fixedBytes32\""));
        assert!(output.contains("defineContractEvent<"));
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

        let output = generate_ts_bindings(&spec, "events.spec.xdr").unwrap();

        assert!(output.contains("\"unsigned\": bigint"));
        assert!(output.contains("\"signed\": bigint"));
        assert!(output.contains("\"unsigned\"?: ContractEventFilterValue<bigint>"));
        assert!(output.contains("\"signed\"?: ContractEventFilterValue<bigint>"));
        assert!(output.contains("representation: \"u256\""));
        assert!(output.contains("representation: \"i256\""));
        assert!(output.contains("{ v: ContractTypeKind.U256 }"));
        assert!(output.contains("{ v: ContractTypeKind.I256 }"));
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
            let output = generate_ts_bindings(&spec, "scalar.spec.xdr").unwrap();
            assert!(output.contains(&format!("\"tag\": {ts_type}")));
            assert!(output.contains(&format!("\"tag\"?: ContractEventFilterValue<{ts_type}>")));
            assert!(output.contains(&format!("representation: \"{representation}\"")));
            assert!(!output.contains("original value cannot be recovered"));
        }
    }

    #[test]
    fn documents_hashed_string_and_bytes_topics() {
        for ty in [ContractType::String, ContractType::Bytes] {
            let mut spec = spec_with_events();
            spec.events.0[0].topics.0[0].ty = ty;
            let output = generate_ts_bindings(&spec, "hashed.spec.xdr").unwrap();
            assert!(output.contains("\"tag\": Uint8Array"));
            assert!(output.contains("original value cannot be recovered"));
            assert!(output.contains("representation: \"hash\""));
            assert!(output.contains("export type PingEventFilter = Record<string, never>;"));
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

        let error = generate_ts_bindings(&spec, "events.spec.xdr").unwrap_err();
        assert!(error.to_string().contains(
            "events \"First\" and \"Second\" have duplicate signature 0xabababababababababababababababababababababababababababababababab"
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

        let error = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap_err();
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
            }],
            false,
        );

        let error = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
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

        let output = generate_ts_bindings(&counter_spec(), "line1\nline2.spec.xdr").unwrap();
        assert!(output.starts_with("// Generated from line1\\nline2.spec.xdr"));
    }

    #[test]
    fn generation_is_deterministic() {
        let spec = erc20_spec();
        let first = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap();
        let second = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap();
        assert_eq!(first, second);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the snapshot fixture keeps its complete ABI shape visible in one place"
    )]
    fn typed_contract_spec() -> ContractSpec {
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
                        name: spec_string("get_values"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::Tuple {
                            tuple: vec![ContractType::U128, ContractType::U128, ContractType::U64]
                                .try_into()
                                .unwrap(),
                        },
                    },
                    ContractSpecMethod {
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

    #[test]
    fn generated_bindings_match_snapshots() {
        for (name, source_name, spec) in [
            ("erc20", "erc20.spec.xdr", erc20_spec()),
            (
                "recursive_type",
                "recursive_type.spec.xdr",
                recursive_type_spec(),
            ),
            (
                "typed_contract",
                "typed_contract.spec.xdr",
                typed_contract_spec(),
            ),
        ] {
            let output = generate_ts_bindings(&spec, source_name).unwrap();
            assert_codegen_snapshot(name, &output);
        }
    }

    fn assert_codegen_snapshot(name: &str, output: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/codegen")
            .join(format!("{name}.ts"));
        if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, output).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read codegen snapshot {}: {error}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        });
        assert_eq!(
            output,
            expected,
            "codegen snapshot {} changed",
            path.display()
        );
    }
}
