//! TypeScript binding code generation from contract specs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use thruput_contract_spec::{
    ContractEnumDef, ContractEventDef, ContractExceptionType, ContractField, ContractSpec,
    ContractSpecId, ContractSpecInterface, ContractSpecMethod, ContractStructDef, ContractType,
    ContractTypeDef, ContractUnionCaseType, ContractUnionDef,
};
use thruput_contract_util::semver::try_major_version_from_string;
use thruput_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};

use crate::CliError;

/// Where a spec id lands in the generated TypeScript: the namespaces it nests
/// under, then the declaration's own name.
struct TsIdPath {
    namespaces: Vec<String>,
    name: String,
}

impl TsIdPath {
    fn segments(&self) -> impl Iterator<Item = &String> {
        self.namespaces.iter().chain(std::iter::once(&self.name))
    }

    /// The fully qualified reference, e.g. `thruput_erc20_if.v0_1.ERC20Error`.
    fn qualified(&self) -> String {
        self.segments()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Its name in the `__gh_codegen` namespace: the segments joined by `$`,
    /// e.g. `thruput_erc20_if$v0_1$ERC20Error`. No spec name contains `$`, and
    /// an escaped segment starts with one, so distinct paths never share it.
    fn alias(&self) -> String {
        self.segments()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("$")
    }

    /// A reference to it that no spec name can capture, wherever it is written.
    fn codegen_ref(&self) -> String {
        format!("{CODEGEN_NAMESPACE}.{}", self.alias())
    }
}

// Bound recursive value rendering and namespace indentation separately from type depth.
const MAX_NAMESPACE_DEPTH: usize = 64;

/// Map a spec id onto its TypeScript path.
///
/// The crate name's `-` becomes `_` and the major nests as its own segment, so
/// distinct ids never share a path.
fn ts_id_path(id: &ContractSpecId) -> Result<TsIdPath, CliError> {
    let crate_name = spec_string_text(&id.crate_name)?;
    let major = spec_string_text(&id.major)?;
    let module_path = spec_string_text(&id.module_path)?;
    let declared = spec_string_text(&id.declared)?;

    // A declared name may contain `::`; nesting on it would not be injective.
    if declared.contains("::") {
        return Err(CliError::TsBindingsFailed(format!(
            "spec type `{}` has `::` in its declared name, which a generated \
             binding cannot tell apart from a module of that path; drop it from \
             the `name = \"...\"` attribute",
            thruput_client::codec::spec_id_text(id)?
        )));
    }
    if [crate_name, major, module_path, declared]
        .iter()
        .any(|part| part.contains('$'))
    {
        return Err(CliError::TsBindingsFailed(format!(
            "{} has \"$\" in its name, which the generated TypeScript keeps for the names it \
             escapes. Rename it.",
            thruput_client::codec::spec_id_text(id)?
        )));
    }

    let krate = crate_name.replace('-', "_");
    let mut namespaces = vec![
        escape_ts_root(&krate),
        format!("v{}", major.replace('.', "_")),
    ];

    let mut segments = module_path.split("::").filter(|s| !s.is_empty()).peekable();
    if segments.peek() == Some(&krate.as_str()) {
        segments.next();
    }
    for segment in segments {
        if namespaces.len() == MAX_NAMESPACE_DEPTH {
            return Err(CliError::TsBindingsFailed(format!(
                "spec namespace exceeds nesting limit ({MAX_NAMESPACE_DEPTH})"
            )));
        }
        namespaces.push(escape_ts_name(segment));
    }

    let path = TsIdPath {
        namespaces,
        name: escape_ts_name(declared),
    };
    if let Some(segment) = path.segments().find(|segment| !is_ts_identifier(segment)) {
        return Err(CliError::TsBindingsFailed(format!(
            "{} would be \"{}\" in the generated TypeScript, where \"{segment}\" is not a \
             valid identifier. Rename it.",
            thruput_client::codec::spec_id_text(id)?,
            path.qualified()
        )));
    }
    Ok(path)
}

/// Where an interface's types and methods land in the generated TypeScript:
/// its name, then its major, as in `ERC20.v0_1`. Name and major are what its
/// entrypoints hash, so two interfaces never share them.
fn ts_interface_path(iface: &ContractSpecInterface) -> Result<[String; 2], CliError> {
    let name = spec_string_text(&iface.name)?;
    let vers = spec_string_text(&iface.vers)?;
    if name.contains('$') {
        return Err(CliError::TsBindingsFailed(format!(
            "interface \"{name}\" has \"$\" in its name, which the generated TypeScript keeps \
             for the names it escapes. Rename it."
        )));
    }
    let root = escape_ts_root(name);
    validate_ts_identifier(&root, &format!("interface \"{name}\""))?;
    let major = try_major_version_from_string(vers).ok_or_else(|| {
        CliError::TsBindingsFailed(format!(
            "interface \"{name}\" has version \"{vers}\", which is not a semantic version"
        ))
    })?;
    let major = format!("v{}", major.replace('.', "_"));
    validate_ts_identifier(&major, &format!("interface \"{name}\" version \"{vers}\""))?;
    Ok([root, major])
}

/// A spec name as the bindings declare it. A reserved word or `__proto__`
/// cannot name a namespace, a type or a value object's key, and a declaration
/// named `globalThis` would capture the built-in types written through it, so
/// each takes a leading `$`, which no spec name contains.
fn escape_ts_name(name: &str) -> String {
    if is_ts_reserved_word(name) || ["__proto__", "globalThis"].contains(&name) {
        format!("${name}")
    } else {
        name.to_owned()
    }
}

/// [`escape_ts_name`] for a crate or interface name, which the bindings
/// declare at top level. There the spec literal and the event decoder, which
/// the generated code also declares, are escaped too.
fn escape_ts_root(name: &str) -> String {
    if ["spec", "decodeEvent"].contains(&name) {
        format!("${name}")
    } else {
        escape_ts_name(name)
    }
}

/// A declaration the bindings emit under a spec id's path.
struct Declaration<'a> {
    path: TsIdPath,
    /// How error messages name it.
    what: String,
    /// What it adds to its crate's value object, if anything.
    value: Option<Value<'a>>,
}

/// A runtime value, which the bindings keep out of the type namespaces so
/// those erase: it lives in its crate's value object, under the same path.
#[derive(Clone, Copy)]
enum Value<'a> {
    /// An enum's variants.
    Enum(&'a ContractEnumDef),
    /// An event's descriptor, and the event's index in the spec.
    Event(usize, &'a ContractEventDef),
}

/// Every declaration the bindings emit under a spec id's path.
fn declarations(spec: &ContractSpec) -> Result<Vec<Declaration<'_>>, CliError> {
    let mut out = Vec::new();
    for def in spec.type_defs.iter() {
        let path = ts_id_path(type_def_id(def)?)?;
        out.push(Declaration {
            what: type_def_label(def)?,
            value: match def {
                ContractTypeDef::TypeDefEnum { enum_def } => Some(Value::Enum(enum_def)),
                _ => None,
            },
            path,
        });
    }
    for (index, event) in spec.events.iter().enumerate() {
        let path = ts_id_path(&event.name)?;
        out.push(Declaration {
            what: declaration_label("event", &event.name)?,
            value: Some(Value::Event(index, event)),
            path,
        });
    }
    Ok(out)
}

/// Every runtime module a binding imports, whole, under the alias it writes
/// that module's names through. No runtime name is ever written bare, so no
/// spec name can capture one. A binding that uses no name from a module still
/// imports it: `noUnusedLocals` skips an unused import whose name starts with `_`.
const RUNTIME_MODULES: &[(&str, &str)] = &[
    ("__gh_protocol", "@thruputchain/protocol"),
    ("__gh_protocol_utils", "@thruputchain/protocol/utils"),
    ("__gh_contract_spec", "@thruputchain/contract-spec"),
    ("__gh_client_contract", "@thruputchain/client/contract"),
];

/// The names the generator declares for itself carry this prefix, which
/// `validate_spec` keeps every spec name out of.
const RESERVED_PREFIX: &str = "__gh_";

/// The namespace a binding declares last, naming every spec type the
/// namespaces before it refer to. A reference written inside a namespace
/// resolves its first segment to the nearest declaration of that name, so a
/// module named after a crate would capture a reference written from the
/// crate's root; nothing can capture this name.
const CODEGEN_NAMESPACE: &str = "__gh_codegen";

/// The id a type def declares itself under.
fn type_def_id(def: &ContractTypeDef) -> Result<&ContractSpecId, CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => Ok(&struct_def.name),
        ContractTypeDef::TypeDefEnum { enum_def } => Ok(&enum_def.name),
        ContractTypeDef::TypeDefUnion { union_def } => Ok(&union_def.name),
        _ => Err(CliError::TsBindingsFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

/// How errors name a declared type or event: its kind, then its id as the SDK's
/// own errors write it, e.g. `enum my_token::errors::TokenError (my_token@1)`.
fn declaration_label(kind: &str, id: &ContractSpecId) -> Result<String, CliError> {
    Ok(format!(
        "{kind} {}",
        thruput_client::codec::spec_id_text(id)?
    ))
}

/// [`declaration_label`] for a type def.
fn type_def_label(def: &ContractTypeDef) -> Result<String, CliError> {
    let kind = match def {
        ContractTypeDef::TypeDefStruct { .. } => "struct",
        ContractTypeDef::TypeDefEnum { .. } => "enum",
        ContractTypeDef::TypeDefUnion { .. } => "union",
        _ => "type",
    };
    declaration_label(kind, type_def_id(def)?)
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
        ContractType::Bytes => Ok("globalThis.Uint8Array".into()),
        ContractType::Address => Ok("__gh_protocol.ContractAddress".into()),
        ContractType::Option { option_inner } => {
            Ok(format!("{} | null", ts_type_ref(option_inner.as_ref())?))
        }
        ContractType::Vector { vector } => {
            let element = vector.element.as_ref();
            if matches!(element, ContractType::U8) {
                return Ok("globalThis.Uint8Array".into());
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
                return Ok(format!("__gh_client_contract.FixedBytes<{}>", array.len));
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
                Ok(format!(
                    "__gh_client_contract.FixedArray<{inner}, {}>",
                    array.len
                ))
            }
        }
        ContractType::Tuple { tuple } => {
            let items: Vec<String> = tuple.iter().map(ts_type_ref).collect::<Result<_, _>>()?;
            Ok(format!("readonly [{}]", items.join(", ")))
        }
        ContractType::Named { named } => Ok(ts_id_path(named)?.codegen_ref()),
        ContractType::SignedToken {
            signed_token_preimage,
        } => match signed_token_preimage.preimage_type.as_deref() {
            Some(inner) => Ok(format!(
                "__gh_client_contract.SignedToken<{}>",
                ts_type_ref(inner)?
            )),
            None => Ok("__gh_client_contract.SignedToken".into()),
        },
        _ => Err(CliError::TsBindingsFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

/// Write rendered declarations grouped under the namespaces they nest in.
fn emit_namespaced(out: &mut String, items: &[(Vec<String>, String)]) {
    let mut by_namespace: BTreeMap<&[String], Vec<&String>> = BTreeMap::new();
    for (namespaces, item) in items {
        by_namespace
            .entry(namespaces.as_slice())
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
            for line in item.lines() {
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
}

/// Emit every declaration's types, nested under its id's namespaces. The
/// namespaces hold nothing else, so they erase to nothing.
fn emit_type_declarations(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let mut items = Vec::new();
    for def in spec.type_defs.iter() {
        let path = ts_id_path(type_def_id(def)?)?;
        items.push((path.namespaces, emit_type_def(def)?));
    }
    for event in spec.events.iter() {
        let path = ts_id_path(&event.name)?;
        items.push((path.namespaces, emit_event_types(event)?));
    }
    emit_namespaced(out, &items);
    Ok(())
}

/// Emit a TypeScript type declaration for a named contract type definition.
fn emit_type_def(def: &ContractTypeDef) -> Result<String, CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => emit_struct(struct_def),
        ContractTypeDef::TypeDefEnum { enum_def } => emit_enum(enum_def),
        ContractTypeDef::TypeDefUnion { union_def } => emit_union(union_def),
        _ => Err(CliError::TsBindingsFailed(
            "contract spec contains an unsupported type definition kind".into(),
        )),
    }
}

/// Emit a TypeScript interface for a contract struct definition.
fn emit_struct(def: &ContractStructDef) -> Result<String, CliError> {
    let name = escape_ts_name(spec_string_text(&def.name.declared)?);
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

/// Emit the type of a contract enum's values, which its crate's value object
/// holds.
fn emit_enum(def: &ContractEnumDef) -> Result<String, CliError> {
    let name = escape_ts_name(spec_string_text(&def.name.declared)?);
    let values: Vec<String> = def
        .variants
        .iter()
        .map(|variant| format!("{}n", variant.value))
        .collect();
    let values = if values.is_empty() {
        "never".to_owned()
    } else {
        values.join(" | ")
    };
    Ok(format!("export type {name} = {values};"))
}

/// Emit a TypeScript discriminated union for a contract union definition.
fn emit_union(def: &ContractUnionDef) -> Result<String, CliError> {
    let name = escape_ts_name(spec_string_text(&def.name.declared)?);
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
                return Err(CliError::TsBindingsFailed(
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
    let interface_groups = interface_groups(spec)?;
    let has_methods = spec
        .interfaces
        .iter()
        .any(|iface| !iface.methods.is_empty());
    emit_header(&mut out, source_name, &interfaces);

    // 2. Imports
    emit_imports(&mut out);

    let _ = writeln!(out);
    let _ = writeln!(out, "const __gh_s = __gh_contract_spec.contractSpecString;");

    // 3. Entrypoints
    if has_methods {
        emit_entrypoints(
            &mut out,
            &interface_groups,
            &public_method_names_by_interface,
        )?;
    }

    // 4. Named type declarations, nested under the namespaces their ids map to
    emit_type_declarations(&mut out, spec)?;

    // 5. Method argument, result, and exception types, nested under each
    // interface's name and major
    emit_interface_types(
        &mut out,
        &interface_groups,
        &public_method_names_by_interface,
    )?;

    // 6. Spec literal
    let _ = writeln!(out);
    emit_spec_literal(&mut out, spec)?;

    // 7. Shared codec context
    if has_methods || !spec.events.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);"
        );
    }

    // 8. Method descriptors, one object per interface name
    for group in &interface_groups {
        let _ = writeln!(out);
        emit_interface_object(&mut out, group, &public_method_names_by_interface)?;
    }

    // 9. Enum values and event descriptors, one object per crate
    emit_value_objects(&mut out, spec)?;

    // 10. Event union and combined decoder
    if !spec.events.is_empty() {
        let _ = writeln!(out);
        emit_decode_event(&mut out, spec)?;
    }

    // 11. The names the namespaces refer to spec types through
    emit_codegen_namespace(&mut out, spec)?;

    Ok(out)
}

/// The interfaces sharing one name, which the bindings declare once: a
/// namespace and an object holding each interface under its major.
struct InterfaceGroup<'a> {
    root: String,
    /// Each interface's major segment, index in the spec, and the interface.
    versions: Vec<(String, usize, &'a ContractSpecInterface)>,
}

/// The spec's interfaces grouped by name, in the order each name first appears.
fn interface_groups(spec: &ContractSpec) -> Result<Vec<InterfaceGroup<'_>>, CliError> {
    let mut groups: Vec<InterfaceGroup> = Vec::new();
    for (index, iface) in spec.interfaces.iter().enumerate() {
        let [root, major] = ts_interface_path(iface)?;
        let version = (major, index, iface);
        match groups.iter_mut().find(|group| group.root == root) {
            Some(group) => group.versions.push(version),
            None => groups.push(InterfaceGroup {
                root,
                versions: vec![version],
            }),
        }
    }
    Ok(groups)
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

/// Emit the import block: every runtime module, whole, under its alias.
fn emit_imports(out: &mut String) {
    let _ = writeln!(out);
    for (alias, module) in RUNTIME_MODULES {
        let _ = writeln!(out, "import * as {alias} from \"{module}\";");
    }
}

/// Emit the `__gh_entrypoints` const, holding each method's entrypoint ID
/// under its interface's name and major.
fn emit_entrypoints(
    out: &mut String,
    interface_groups: &[InterfaceGroup],
    public_method_names: &[Vec<String>],
) -> Result<(), CliError> {
    let _ = writeln!(out);
    let _ = writeln!(out, "const __gh_entrypoints = {{");
    for group in interface_groups {
        let _ = writeln!(out, "  {}: {{", group.root);
        for (major, iface_index, iface) in &group.versions {
            let iface_name = spec_string_text(&iface.name)?;
            let iface_vers = spec_string_text(&iface.vers)?;
            let _ = writeln!(out, "    {major}: {{");
            for (method, public_method_name) in
                iface.methods.iter().zip(&public_method_names[*iface_index])
            {
                let sig = NormalizedMethodSignature::new(spec_string_text(&method.name)?);
                let id = contract_entrypoint_id(Some(iface_name), iface_vers, &sig);
                let hex = hex::encode(id.as_ref());
                let _ = writeln!(
                    out,
                    "      {public_method_name}: __gh_protocol.contractEntrypointID(\
                     __gh_protocol_utils.fromHex(\"{hex}\")),",
                );
            }
            let _ = writeln!(out, "    }},");
        }
        let _ = writeln!(out, "  }},");
    }
    let _ = writeln!(out, "}} as const;");
    Ok(())
}

/// Emit the full `ContractSpec` object literal.
fn emit_spec_literal(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let _ = writeln!(
        out,
        "export const spec: __gh_client_contract.ContractSpec = {{"
    );

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
        "{{ crate_name: __gh_s({}), major: __gh_s({}), module_path: __gh_s({}), declared: __gh_s({}) }}",
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
    let _ = writeln!(
        out,
        "      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex(\"{sig_hex}\")),"
    );

    let _ = writeln!(out, "      topics: [");
    for field in event.topics.iter() {
        let fname = spec_string_text(&field.name)?;
        let ty_lit = spec_type_to_ts_literal(&field.ty)?;
        let _ = writeln!(
            out,
            "        {{ name: __gh_s({}), ty: {ty_lit} }},",
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
            "        {{ name: __gh_s({}), ty: {ty_lit} }},",
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
    let _ = writeln!(out, "      name: __gh_s({}),", ts_string_literal(name));
    let _ = writeln!(out, "      vers: __gh_s({}),", ts_string_literal(vers));

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
    let _ = writeln!(out, "          name: __gh_s({}),", ts_string_literal(name));

    // args
    let _ = writeln!(out, "          args: [");
    for arg in method.args.iter() {
        let arg_name = spec_string_text(&arg.name)?;
        let ty_lit = spec_type_to_ts_literal(&arg.ty)?;
        let _ = writeln!(
            out,
            "            {{ name: __gh_s({}), ty: {ty_lit} }},",
            ts_string_literal(arg_name),
        );
    }
    let _ = writeln!(out, "          ],");

    // return_type
    let ret_lit = spec_type_to_ts_literal(&method.return_type)?;
    let _ = writeln!(out, "          return_type: {ret_lit},");
    if let Some((_, effect)) = method_effect(method)? {
        let _ = writeln!(
            out,
            "          effect: __gh_client_contract.ContractMethodEffect.{effect},"
        );
    } else {
        let _ = writeln!(out, "          effect: null,");
    }

    let _ = writeln!(out, "        }},");
    Ok(())
}

/// Emit a single type-def entry inside the spec literal.
fn emit_spec_type_def_literal(out: &mut String, def: &ContractTypeDef) -> Result<(), CliError> {
    match def {
        ContractTypeDef::TypeDefStruct { struct_def } => {
            let name = ts_spec_id_literal(&struct_def.name)?;
            let _ = writeln!(out, "    {{");
            let _ = writeln!(
                out,
                "      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,"
            );
            let _ = writeln!(out, "      struct_def: {{");
            let _ = writeln!(out, "        name: {name},");
            let _ = writeln!(out, "        fields: [");
            for field in struct_def.fields.iter() {
                let fname = spec_string_text(&field.name)?;
                let ty_lit = spec_type_to_ts_literal(&field.ty)?;
                let _ = writeln!(
                    out,
                    "          {{ name: __gh_s({}), ty: {ty_lit} }},",
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
            let _ = writeln!(
                out,
                "      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,"
            );
            let _ = writeln!(out, "      enum_def: {{");
            let _ = writeln!(out, "        name: {name},");
            let _ = writeln!(out, "        variants: [");
            for variant in enum_def.variants.iter() {
                let vname = spec_string_text(&variant.name)?;
                let _ = writeln!(
                    out,
                    "          {{ name: __gh_s({}), value: {}n }},",
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
            let _ = writeln!(
                out,
                "      v: __gh_contract_spec.ContractTypeDefKind.TypeDefUnion,"
            );
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
            return Err(CliError::TsBindingsFailed(
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
            let _ = writeln!(
                out,
                "            name: __gh_s({}),",
                ts_string_literal(name)
            );
            let _ = writeln!(out, "            discriminant: {discriminant}n,");
            let _ = writeln!(
                out,
                "            case_type: \
                 {{ v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseVoid }},"
            );
            let _ = writeln!(out, "          }},");
        }
        ContractUnionCaseType::UnionCaseTuple { tuple } => {
            let _ = writeln!(out, "          {{");
            let _ = writeln!(
                out,
                "            name: __gh_s({}),",
                ts_string_literal(name)
            );
            let _ = writeln!(out, "            discriminant: {discriminant}n,");
            let items: Vec<String> = tuple
                .iter()
                .map(spec_type_to_ts_literal)
                .collect::<Result<_, _>>()?;
            let _ = writeln!(
                out,
                "            case_type: {{ v: \
                 __gh_contract_spec.ContractUnionCaseKind.UnionCaseTuple, tuple: [{}] }},",
                items.join(", "),
            );
            let _ = writeln!(out, "          }},");
        }
        ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
            let _ = writeln!(out, "          {{");
            let _ = writeln!(
                out,
                "            name: __gh_s({}),",
                ts_string_literal(name)
            );
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
                    "{{ name: __gh_s({}), ty: {ty_lit} }}",
                    ts_string_literal(fname)
                );
            }
            fields_str.push(']');
            let _ = writeln!(
                out,
                "            case_type: {{ v: \
                 __gh_contract_spec.ContractUnionCaseKind.UnionCaseStruct, \
                 struct_fields: {fields_str} }},",
            );
            let _ = writeln!(out, "          }},");
        }
        _ => {
            return Err(CliError::TsBindingsFailed(
                "contract spec contains an unsupported union case kind".into(),
            ));
        }
    }
    Ok(())
}

/// Serialize a `ContractType` as a TypeScript object literal suitable for
/// the spec constant (e.g. `{ v: __gh_contract_spec.ContractTypeKind.U64 }`).
fn spec_type_to_ts_literal(ty: &ContractType) -> Result<String, CliError> {
    match ty {
        ContractType::Void => Ok("{ v: __gh_contract_spec.ContractTypeKind.Void }".into()),
        ContractType::Bool => Ok("{ v: __gh_contract_spec.ContractTypeKind.Bool }".into()),
        ContractType::U8 => Ok("{ v: __gh_contract_spec.ContractTypeKind.U8 }".into()),
        ContractType::U32 => Ok("{ v: __gh_contract_spec.ContractTypeKind.U32 }".into()),
        ContractType::I32 => Ok("{ v: __gh_contract_spec.ContractTypeKind.I32 }".into()),
        ContractType::U64 => Ok("{ v: __gh_contract_spec.ContractTypeKind.U64 }".into()),
        ContractType::I64 => Ok("{ v: __gh_contract_spec.ContractTypeKind.I64 }".into()),
        ContractType::U128 => Ok("{ v: __gh_contract_spec.ContractTypeKind.U128 }".into()),
        ContractType::I128 => Ok("{ v: __gh_contract_spec.ContractTypeKind.I128 }".into()),
        ContractType::U256 => Ok("{ v: __gh_contract_spec.ContractTypeKind.U256 }".into()),
        ContractType::I256 => Ok("{ v: __gh_contract_spec.ContractTypeKind.I256 }".into()),
        ContractType::String => Ok("{ v: __gh_contract_spec.ContractTypeKind.String }".into()),
        ContractType::Bytes => Ok("{ v: __gh_contract_spec.ContractTypeKind.Bytes }".into()),
        ContractType::Address => Ok("{ v: __gh_contract_spec.ContractTypeKind.Address }".into()),
        ContractType::Option { option_inner } => {
            let inner_lit = spec_type_to_ts_literal(option_inner.as_ref())?;
            Ok(format!(
                "{{ v: __gh_contract_spec.ContractTypeKind.Option, option_inner: \
                 {inner_lit} }}"
            ))
        }
        ContractType::Vector { vector } => {
            let elem = vector.element.as_ref();
            let elem_lit = spec_type_to_ts_literal(elem)?;
            Ok(format!(
                "{{ v: __gh_contract_spec.ContractTypeKind.Vector, vector: \
                 {{ element: {elem_lit}, max_len: {}n }} }}",
                vector.max_len,
            ))
        }
        ContractType::Array { array } => {
            let elem = array.element.as_ref();
            let elem_lit = spec_type_to_ts_literal(elem)?;
            Ok(format!(
                "{{ v: __gh_contract_spec.ContractTypeKind.Array, array: \
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
                "{{ v: __gh_contract_spec.ContractTypeKind.Tuple, tuple: [{}] }}",
                items.join(", "),
            ))
        }
        ContractType::Named { named } => {
            // The whole id, matching what the type defs in this same literal
            // declare themselves under. A bare declared name here would never
            // match one of those, so no named reference would resolve.
            Ok(format!(
                "{{ v: __gh_contract_spec.ContractTypeKind.Named, named: {} }}",
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
                "{{ v: __gh_contract_spec.ContractTypeKind.SignedToken, signed_token_preimage: \
                 {{ preimage_type: {preimage} }} }}"
            ))
        }
        _ => Err(CliError::TsBindingsFailed(
            "contract spec contains an unsupported contract type".into(),
        )),
    }
}

/// Emit the exported object holding the methods of the interfaces that share
/// one name, each under its major, as in `ERC20.v0_1.transfer`. It merges with
/// the namespace of the same path holding their types.
fn emit_interface_object(
    out: &mut String,
    group: &InterfaceGroup,
    public_method_names: &[Vec<String>],
) -> Result<(), CliError> {
    let _ = writeln!(out, "export const {} = {{", group.root);
    for (major, iface_index, iface) in &group.versions {
        let _ = writeln!(out, "  {major}: {{");
        let path = format!("{}.{major}", group.root);
        for (method_index, method) in iface.methods.iter().enumerate() {
            let rendered = render_interface_method(
                iface,
                &path,
                *iface_index,
                method,
                method_index,
                &public_method_names[*iface_index][method_index],
            )?;
            for line in rendered.lines() {
                let _ = writeln!(out, "    {line}");
            }
        }
        let _ = writeln!(out, "  }},");
    }
    let _ = writeln!(out, "}} as const;");
    Ok(())
}

/// Emit each interface's argument, result and exception types, nested under
/// its name and major. The names are the generator's own, so none collides
/// with a spec type, which nests under its crate.
fn emit_interface_types(
    out: &mut String,
    interface_groups: &[InterfaceGroup],
    public_method_names: &[Vec<String>],
) -> Result<(), CliError> {
    let mut items = Vec::new();
    for group in interface_groups {
        for (major, iface_index, iface) in &group.versions {
            items.push((
                vec![group.root.clone(), major.clone()],
                render_interface_types(iface, &public_method_names[*iface_index])?,
            ));
        }
    }
    emit_namespaced(out, &items);
    Ok(())
}

fn render_interface_types(
    iface: &ContractSpecInterface,
    public_method_names: &[String],
) -> Result<String, CliError> {
    let mut out = String::new();
    for (method, public_method_name) in iface.methods.iter().zip(public_method_names) {
        let stem = method_type_stem(public_method_name);
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

    if iface.throws.is_empty() {
        let _ = writeln!(out, "export type DeclaredException = never;");
    } else {
        let _ = writeln!(out, "export type DeclaredException =");
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
        "export type Exception = __gh_client_contract.ContractException<DeclaredException>;"
    );
    Ok(out)
}

fn method_effect(
    method: &ContractSpecMethod,
) -> Result<Option<(&'static str, &'static str)>, CliError> {
    use thruput_contract_spec::ContractMethodEffect as Effect;
    match method.effect {
        None => Ok(None),
        Some(Effect::EffectPure) => Ok(Some(("pure", "EffectPure"))),
        Some(Effect::EffectReadOnly) => Ok(Some(("readOnly", "EffectReadOnly"))),
        Some(Effect::EffectStateChanging) => Ok(Some(("stateChanging", "EffectStateChanging"))),
        Some(effect) => Err(CliError::TsBindingsFailed(format!(
            "unsupported method effect {}; update the toolchain and rebuild the spec",
            effect.0
        ))),
    }
}

/// Render one contract method descriptor. `path` is where its interface's
/// types sit, read from the top level, where only a root can claim a name.
fn render_interface_method(
    iface: &ContractSpecInterface,
    path: &str,
    iface_index: usize,
    method: &ContractSpecMethod,
    method_index: usize,
    public_method_name: &str,
) -> Result<String, CliError> {
    let stem = method_type_stem(public_method_name);
    let mut out = String::new();
    if let Some((effect, _)) = method_effect(method)? {
        let _ = writeln!(
            out,
            "/** Declared effect: {effect}. Author declaration; not compiler/runtime enforced. */"
        );
    }
    let _ = writeln!(
        out,
        "{public_method_name}: __gh_client_contract.defineContractMethod<"
    );
    let _ = writeln!(out, "  {path}.{stem}Args,");
    let _ = writeln!(out, "  {path}.{stem}Result,");
    let _ = writeln!(out, "  {path}.DeclaredException");
    let _ = writeln!(out, ">({{");
    let _ = writeln!(
        out,
        "  interfaceName: {},",
        ts_string_literal(spec_string_text(&iface.name)?)
    );
    let _ = writeln!(
        out,
        "  interfaceVersion: {},",
        ts_string_literal(spec_string_text(&iface.vers)?)
    );
    let _ = writeln!(
        out,
        "  abiName: {},",
        ts_string_literal(spec_string_text(&method.name)?)
    );
    let _ = writeln!(
        out,
        "  publicName: {},",
        ts_string_literal(public_method_name)
    );
    let _ = writeln!(
        out,
        "  entrypoint: __gh_entrypoints.{path}.{public_method_name},"
    );
    let _ = writeln!(
        out,
        "  effect: spec.interfaces[{iface_index}]!.methods[{method_index}]!.effect,"
    );
    let _ = writeln!(out, "  args: [");
    for (arg_index, arg) in method.args.iter().enumerate() {
        let abi_name = spec_string_text(&arg.name)?;
        let public_name = to_camel_case(abi_name);
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      abiName: {},", ts_string_literal(abi_name));
        let _ = writeln!(
            out,
            "      publicName: {},",
            ts_string_literal(&public_name)
        );
        let _ = writeln!(
            out,
            "      type: spec.interfaces[{iface_index}]!.methods[{method_index}]!.args[{arg_index}]!.ty,"
        );
        let _ = writeln!(out, "    }},");
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(
        out,
        "  returnType: spec.interfaces[{iface_index}]!.methods[{method_index}]!.return_type,"
    );
    let _ = writeln!(out, "  throws: [");
    for (exception_index, exception) in iface.throws.iter().enumerate() {
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      selector: 0x{:x}n,", exception.selector);
        let _ = writeln!(out, "      name: {},", exception_name(&exception.ty)?);
        let _ = writeln!(
            out,
            "      type: spec.interfaces[{iface_index}]!.throws[{exception_index}]!.ty,"
        );
        let _ = writeln!(out, "    }},");
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  codecs: __gh_codecs,");
    let _ = writeln!(out, "}}),");
    Ok(out)
}

/// Emit an event's decoded and filter types.
fn emit_event_types(event: &ContractEventDef) -> Result<String, CliError> {
    let declared = spec_string_text(&event.name.declared)?;
    let name = escape_ts_name(declared);
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
                "  {}?: __gh_client_contract.ContractEventFilterValue<{ty}>;",
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

    let mut out = String::new();
    let _ = writeln!(
        out,
        "export interface {name} extends __gh_client_contract.DecodedContractEvent<"
    );
    let _ = writeln!(out, "  {},", ts_string_literal(declared));
    let _ = writeln!(out, "  {{ {} }},", topic_fields.join("; "));
    let _ = writeln!(out, "  {{ {} }}", payload_fields.join("; "));
    let _ = writeln!(out, "> {{}}");
    let _ = writeln!(out);
    if filter_fields.is_empty() {
        let _ = writeln!(
            out,
            "export type {name}Filter = globalThis.Record<string, never>;"
        );
    } else {
        let _ = writeln!(out, "export interface {name}Filter {{");
        for field in filter_fields {
            let _ = writeln!(out, "{field}");
        }
        let _ = writeln!(out, "}}");
    }
    Ok(out)
}

/// A node of a crate's value object: the value declared at its path, if any,
/// and the nodes below it that lead to values.
#[derive(Default)]
struct ValueNode<'a> {
    value: Option<Value<'a>>,
    children: BTreeMap<String, ValueNode<'a>>,
}

/// Emit one object per crate holding its enums' variants and its events'
/// descriptors, each under the path the namespaces give its types.
fn emit_value_objects(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let mut crates: BTreeMap<String, ValueNode> = BTreeMap::new();
    for decl in declarations(spec)? {
        let Some(value) = decl.value else {
            continue;
        };
        let mut node = crates.entry(decl.path.namespaces[0].clone()).or_default();
        for segment in decl.path.namespaces[1..]
            .iter()
            .chain(std::iter::once(&decl.path.name))
        {
            node = node.children.entry(segment.clone()).or_default();
        }
        node.value = Some(value);
    }
    for (root, node) in &crates {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "export const {root} = {} as const;",
            render_value_node(node)?
        );
    }
    Ok(())
}

/// Render a value node as an object literal, or as an event's descriptor.
/// `validate_spec` keeps values out of the modules named after events.
fn render_value_node(node: &ValueNode) -> Result<String, CliError> {
    if let Some(Value::Event(index, event)) = node.value {
        return render_event_descriptor(event, index);
    }
    let mut out = String::from("{\n");
    if let Some(Value::Enum(def)) = node.value {
        for variant in def.variants.iter() {
            let key = ts_property_key(spec_string_text(&variant.name)?)?;
            let _ = writeln!(out, "  {key}: {}n,", variant.value);
        }
    }
    for (name, child) in &node.children {
        let child = render_value_node(child)?.replace('\n', "\n  ");
        let _ = writeln!(out, "  {name}: {child},");
    }
    out.push('}');
    Ok(out)
}

/// Render an event's descriptor. It sits at top level, where no spec name can
/// take `spec`, and names its event's types from their crate's root down.
fn render_event_descriptor(event: &ContractEventDef, index: usize) -> Result<String, CliError> {
    let name = ts_string_literal(spec_string_text(&event.name.declared)?);
    let path = ts_id_path(&event.name)?.qualified();
    let mut out = String::new();
    let _ = writeln!(out, "__gh_client_contract.defineContractEvent<");
    let _ = writeln!(out, "  {name},");
    let _ = writeln!(out, "  {path}[\"topics\"],");
    let _ = writeln!(out, "  {path}[\"fields\"],");
    let _ = writeln!(out, "  {path}Filter");
    let _ = writeln!(out, ">({{");
    let _ = writeln!(out, "  name: {name},");
    let _ = writeln!(out, "  signature: spec.events[{index}]!.signature,");
    let _ = writeln!(out, "  topics: [");
    for (field_index, field) in event.topics.iter().enumerate() {
        let fname = spec_string_text(&field.name)?;
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      name: {},", ts_string_literal(fname));
        let _ = writeln!(
            out,
            "      type: spec.events[{index}]!.topics[{field_index}]!.ty,"
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
            "    {{ name: {}, type: spec.events[{index}]!.fields[{field_index}]!.ty }},",
            ts_string_literal(fname)
        );
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  codecs: __gh_codecs,");
    out.push_str("})");
    Ok(out)
}

/// Name of the per-contract event union: the first interface name plus
/// `Event`. The generated file is per-contract, so all events share it.
fn event_union_name(spec: &ContractSpec) -> Result<String, CliError> {
    let first_iface = spec.interfaces.iter().next().ok_or_else(|| {
        CliError::TsBindingsFailed("contract spec with events has no interfaces".into())
    })?;
    Ok(format!("{}Event", spec_string_text(&first_iface.name)?))
}

/// Emit the per-contract event union and combined decoder.
fn emit_decode_event(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    let union_name = event_union_name(spec)?;

    // Qualified from the top level, where every crate root resolves to itself.
    let event_names: Vec<String> = spec
        .events
        .iter()
        .map(|e| Ok(ts_id_path(&e.name)?.qualified()))
        .collect::<Result<_, CliError>>()?;

    let _ = writeln!(
        out,
        "export type {union_name} = {};",
        event_names.join(" | "),
    );
    let _ = writeln!(out);
    // The parameter's name is the generator's own: a crate named like it would
    // otherwise lose its descriptors to it in the body.
    let _ = writeln!(
        out,
        "export function decodeEvent(__gh_event: {{ topics: globalThis.Uint8Array[]; \
         data: globalThis.Uint8Array }}): {union_name} | null {{"
    );
    let chain = event_names
        .iter()
        .map(|n| format!("{n}.decode(__gh_event)"))
        .collect::<Vec<_>>()
        .join(" ?? ");
    let _ = writeln!(out, "  return {chain};");
    let _ = writeln!(out, "}}");
    Ok(())
}

/// Emit the namespace every reference to a spec type goes through, with one
/// alias per type def. Its own references resolve at top level, where only a
/// root can claim a name.
fn emit_codegen_namespace(out: &mut String, spec: &ContractSpec) -> Result<(), CliError> {
    if spec.type_defs.is_empty() {
        return Ok(());
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "// The types the namespaces above refer to, under names no declaration can capture."
    );
    let _ = writeln!(out, "export namespace {CODEGEN_NAMESPACE} {{");
    for def in spec.type_defs.iter() {
        let path = ts_id_path(type_def_id(def)?)?;
        let _ = writeln!(
            out,
            "  export type {} = {};",
            path.alias(),
            path.qualified()
        );
    }
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
    let has_events = !spec.events.is_empty();
    // A crate's value object puts every value under its path. An event's
    // descriptor cannot also hold the values of a module named after it, and
    // an enum's variants share one object with them, so no key may repeat.
    let declared = declarations(spec)?;
    for decl in &declared {
        let Some(value) = decl.value else {
            continue;
        };
        let at: Vec<String> = decl
            .path
            .namespaces
            .iter()
            .chain(std::iter::once(&decl.path.name))
            .cloned()
            .collect();
        for inner in declared.iter().filter(|inner| inner.value.is_some()) {
            let key = if inner.path.namespaces == at {
                &inner.path.name
            } else if inner.path.namespaces.starts_with(&at) {
                &inner.path.namespaces[at.len()]
            } else {
                continue;
            };
            let path = decl.path.qualified();
            match value {
                Value::Event(..) => {
                    return Err(CliError::TsBindingsFailed(format!(
                        "{} and {} both need a value under \"{path}\" in the generated \
                         TypeScript: the event's descriptor is there, and it cannot also hold \
                         \"{key}\". Rename either, or move one out of that module.",
                        decl.what, inner.what
                    )));
                }
                Value::Enum(def) => {
                    for variant in def.variants.iter() {
                        if spec_string_text(&variant.name)? == key {
                            return Err(CliError::TsBindingsFailed(format!(
                                "{} has a variant \"{key}\", and {} also needs \
                                 \"{path}.{key}\" in the generated TypeScript, where an enum's \
                                 variants share one object with the values of the module named \
                                 after it. Rename either.",
                                decl.what, inner.what
                            )));
                        }
                    }
                }
            }
        }
    }

    // Runtime names are written through their module's alias, and the file's
    // internal bindings carry the reserved prefix, so only its exported
    // bindings need a name to themselves. A root named after one is escaped.
    let mut generated = vec!["spec"];
    if has_events {
        generated.push("decodeEvent");
    }
    for name in generated {
        reserve_ts_value_name(
            &mut top_level_values,
            name.to_owned(),
            "generated binding".into(),
        )?;
    }
    // Roots are reserved once, since defs share them; names are reserved per
    // qualified path, where a def and an event would merge.
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
        reserve_ts_value_name(
            &mut namespaced_values,
            path.qualified(),
            type_def_label(def)?,
        )?;
        defined_types.insert(id.clone());
    }

    // Interfaces sharing a name share its root, each under its own major.
    let mut interface_versions: BTreeMap<[String; 2], &str> = BTreeMap::new();
    for (iface_index, iface) in spec.interfaces.iter().enumerate() {
        let iface_name = spec_string_text(&iface.name)?;
        let iface_vers = spec_string_text(&iface.vers)?;
        let path = ts_interface_path(iface)?;
        if !interface_versions.keys().any(|[root, _]| *root == path[0]) {
            reserve_ts_value_name(
                &mut top_level_values,
                path[0].clone(),
                format!("interface \"{iface_name}\""),
            )?;
        }
        if let Some(existing) = interface_versions.insert(path.clone(), iface_vers) {
            return Err(CliError::TsBindingsFailed(format!(
                "interface \"{iface_name}\" versions {existing} and {iface_vers} would both be \
                 \"{}\" in the generated TypeScript",
                path.join(".")
            )));
        }
        validate_interface_method_name_collisions(
            iface_name,
            iface,
            &public_method_names[iface_index],
        )?;
        let mut selectors = BTreeSet::new();
        for exception in iface.throws.iter() {
            if !selectors.insert(exception.selector) {
                return Err(CliError::TsBindingsFailed(format!(
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
        for method in iface.methods.iter() {
            let method_name = spec_string_text(&method.name)?;
            validate_method_arg_name_collisions(method_name, method)?;
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
        let event_label = declaration_label("event", &event.name)?;
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
            (qualified.clone(), event_label.clone()),
            (
                format!("{qualified}Filter"),
                format!("the filter type of {event_label}"),
            ),
        ] {
            reserve_ts_value_name(&mut namespaced_values, name, source)?;
        }
        if let Some(existing) =
            event_signatures.insert(event.signature.into_inner(), event_label.clone())
        {
            return Err(CliError::TsBindingsFailed(format!(
                "{existing} and {event_label} have duplicate signature 0x{}",
                hex::encode(event.signature.into_inner())
            )));
        }
        if event.topics.len() > 2 {
            return Err(CliError::TsBindingsFailed(format!(
                "{event_label} declares {} topics including its signature; protocol maximum is 3",
                event.topics.len() + 1
            )));
        }
        validate_fields(
            event.topics.iter(),
            &defined_types,
            &format!("{event_label} topics"),
        )?;
        validate_fields(
            event.fields.iter(),
            &defined_types,
            &format!("{event_label} fields"),
        )?;
        // A token's handle never travels in event data, and the event
        // tooling rejects one; refuse here rather than emit an event spec it
        // cannot decode. Named definitions are resolved so a token nested in
        // a struct or union the event refers to is caught too.
        for field in event.topics.iter().chain(event.fields.iter()) {
            if type_carries_signed_token(&field.ty, &type_def_index) {
                return Err(CliError::TsBindingsFailed(format!(
                    "{event_label} field \"{}\" carries a signed token, which events cannot log",
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

    let nested: Vec<(&String, &String)> = declared
        .iter()
        .flat_map(|decl| {
            decl.path
                .namespaces
                .iter()
                .skip(1)
                .chain(std::iter::once(&decl.path.name))
                .map(|name| (name, &decl.what))
        })
        .collect();
    if let Some((name, source)) = top_level_values
        .iter()
        .chain(nested.iter().copied())
        .find(|(name, _)| name.starts_with(RESERVED_PREFIX))
    {
        return Err(CliError::TsBindingsFailed(format!(
            "generated TypeScript name \"{name}\" for {source} starts with \"{RESERVED_PREFIX}\", \
             which the generator reserves for the names it declares itself. Rename it without \
             that prefix."
        )));
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
    let mut stems: BTreeMap<String, &str> = BTreeMap::new();
    for (method_index, method) in iface.methods.iter().enumerate() {
        let method_name = spec_string_text(&method.name)?;
        let public_method_name = public_method_names[method_index].as_str();
        validate_ts_property_identifier(
            public_method_name,
            &format!("method \"{iface_name}.{method_name}\""),
        )?;
        if let Some(existing) = methods.get(public_method_name) {
            return Err(CliError::TsBindingsFailed(format!(
                "interface \"{iface_name}\" method names \"{existing}\" and \"{method_name}\" both map to \"{public_method_name}\""
            )));
        }
        methods.insert(public_method_name, method_name);
        // Names differing only in their first letter's case share a stem.
        let stem = method_type_stem(public_method_name);
        if let Some(existing) = stems.get(&stem) {
            return Err(CliError::TsBindingsFailed(format!(
                "interface \"{iface_name}\" methods \"{existing}\" and \"{method_name}\" would \
                 both declare \"{stem}Args\" and \"{stem}Result\" in the generated TypeScript"
            )));
        }
        stems.insert(stem, method_name);
    }
    Ok(())
}

fn reserve_ts_value_name(
    names: &mut BTreeMap<String, String>,
    name: String,
    source: String,
) -> Result<(), CliError> {
    if let Some(existing) = names.get(&name) {
        return Err(CliError::TsBindingsFailed(format!(
            "generated TypeScript value name \"{name}\" collides between {existing} and {source}"
        )));
    }
    names.insert(name, source);
    Ok(())
}

fn validate_ts_identifier(name: &str, source: &str) -> Result<(), CliError> {
    if !is_ts_identifier(name) || name == "__proto__" {
        return Err(CliError::TsBindingsFailed(format!(
            "generated TypeScript identifier \"{name}\" for {source} is invalid"
        )));
    }
    Ok(())
}

fn validate_ts_property_identifier(name: &str, source: &str) -> Result<(), CliError> {
    if !is_ts_identifier_name(name) || name == "__proto__" {
        return Err(CliError::TsBindingsFailed(format!(
            "generated TypeScript property name \"{name}\" for {source} is invalid"
        )));
    }
    Ok(())
}

fn is_ts_identifier(name: &str) -> bool {
    !is_ts_reserved_word(name) && is_ts_identifier_name(name)
}

/// Whether `name` is a word the bindings cannot declare a namespace, type or
/// strict-mode binding by: a reserved word, or a type TypeScript predefines.
fn is_ts_reserved_word(name: &str) -> bool {
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
    RESERVED.contains(&name)
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
        ContractTypeDef::TypeDefStruct { struct_def } => validate_fields(
            struct_def.fields.iter(),
            defined_types,
            &type_def_label(def)?,
        ),
        ContractTypeDef::TypeDefEnum { enum_def } => {
            let name = type_def_label(def)?;
            let mut members = BTreeSet::new();
            let mut values = BTreeSet::new();
            for variant in enum_def.variants.iter() {
                let variant_name = spec_string_text(&variant.name)?;
                validate_property_name(variant_name, &name)?;
                if !members.insert(variant_name) {
                    return Err(CliError::TsBindingsFailed(format!(
                        "{name} has duplicate member \"{variant_name}\""
                    )));
                }
                if !values.insert(variant.value) {
                    return Err(CliError::TsBindingsFailed(format!(
                        "{name} has duplicate value {}",
                        variant.value
                    )));
                }
            }
            Ok(())
        }
        ContractTypeDef::TypeDefUnion { union_def } => {
            let name = type_def_label(def)?;
            if union_def.cases.is_empty() {
                return Err(CliError::TsBindingsFailed(format!(
                    "{name} must declare at least one case"
                )));
            }
            let mut members = BTreeSet::new();
            let mut discriminants = BTreeSet::new();
            for case in union_def.cases.iter() {
                let case_name = spec_string_text(&case.name)?;
                if !members.insert(case_name) {
                    return Err(CliError::TsBindingsFailed(format!(
                        "{name} has duplicate case \"{case_name}\""
                    )));
                }
                if !discriminants.insert(case.discriminant) {
                    return Err(CliError::TsBindingsFailed(format!(
                        "{name} has duplicate discriminant {}",
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
                                &format!("{name} case \"{case_name}\" tuple item {index}"),
                            )?;
                        }
                    }
                    ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                        validate_fields(
                            struct_fields.iter(),
                            defined_types,
                            &format!("{name} case \"{case_name}\""),
                        )?;
                    }
                    _ => {
                        return Err(CliError::TsBindingsFailed(format!(
                            "{name} case \"{case_name}\" has an unsupported kind"
                        )));
                    }
                }
            }
            Ok(())
        }
        _ => Err(CliError::TsBindingsFailed(
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
            return Err(CliError::TsBindingsFailed(format!(
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
        ContractType::Void => Err(CliError::TsBindingsFailed(format!(
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
                Err(CliError::TsBindingsFailed(format!(
                    "{location} references unresolved named type \"{}\"",
                    thruput_client::codec::spec_id_text(named)?
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
        _ => Err(CliError::TsBindingsFailed(format!(
            "{location} uses an unsupported contract type"
        ))),
    }
}

fn validate_property_name(name: &str, location: &str) -> Result<(), CliError> {
    if name == "__proto__" {
        return Err(CliError::TsBindingsFailed(format!(
            "{location} contains unsafe property name \"{name}\""
        )));
    }
    Ok(())
}

/// The public method name with its first letter capitalized, which names the
/// method's `Args` and `Result` types in its interface's namespace.
fn method_type_stem(public_method_name: &str) -> String {
    let mut chars = public_method_name.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
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
        "address" => "__gh_protocol.ContractAddress".into(),
        "fixedBytes32" => "__gh_client_contract.FixedBytes<32>".into(),
        "u32" | "u64" | "u128" | "u256" | "i32" | "i64" | "i128" | "i256" => "bigint".into(),
        "bool" => "boolean".into(),
        _ => "globalThis.Uint8Array".into(),
    }
}

/// Whether `ty` contains a signed token anywhere, resolving named definitions
/// through `type_defs`. Visit each named definition once per search. A work
/// list bounds stack use even when named references form a long chain.
fn type_carries_signed_token(
    ty: &ContractType,
    type_defs: &BTreeMap<&ContractSpecId, &ContractTypeDef>,
) -> bool {
    let mut pending = vec![ty];
    let mut visited = BTreeSet::new();
    while let Some(ty) = pending.pop() {
        match ty {
            ContractType::SignedToken { .. } => return true,
            ContractType::Option { option_inner } => pending.push(option_inner),
            ContractType::Vector { vector } => pending.push(&vector.element),
            ContractType::Array { array } => pending.push(&array.element),
            ContractType::Tuple { tuple } => pending.extend(tuple.iter()),
            ContractType::Named { named } if visited.insert(named) => match type_defs.get(named) {
                Some(ContractTypeDef::TypeDefStruct { struct_def }) => {
                    pending.extend(struct_def.fields.iter().map(|field| &field.ty));
                }
                Some(ContractTypeDef::TypeDefUnion { union_def }) => {
                    for case in union_def.cases.iter() {
                        match &case.case_type {
                            ContractUnionCaseType::UnionCaseTuple { tuple } => {
                                pending.extend(tuple.iter());
                            }
                            ContractUnionCaseType::UnionCaseStruct { struct_fields } => {
                                pending.extend(struct_fields.iter().map(|field| &field.ty));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    false
}

fn ts_property_key(name: &str) -> Result<String, CliError> {
    validate_property_name(name, "emitted TypeScript object")?;
    Ok(ts_string_literal(name))
}

/// Read a spec string as text for emission; accepts any UTF-8, unlike the codec's validator.
fn spec_string_text(value: &thruput_contract_spec::ContractSpecString) -> Result<&str, CliError> {
    std::str::from_utf8(value.as_ref()).map_err(|error| {
        CliError::TsBindingsFailed(format!("contract spec contains invalid UTF-8: {error}"))
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
            return Err(CliError::TsBindingsFailed(format!(
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
    use thruput_contract_spec::{
        ContractArrayType, ContractEnumDef, ContractEnumVariant, ContractEventDef, ContractSpec,
        ContractSpecInterface, ContractSpecMethod, ContractSpecString, ContractStructDef,
        ContractUnionCase, ContractVectorType,
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
            let bytes = xdrs::txs::encode(&spec).unwrap();
            let spec = crate::contract_spec::load_contract_spec_from_xdr(&bytes).unwrap();
            let result = generate_ts_bindings(&spec, "namespace.spec.xdr");
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
        let output = generate_ts_bindings(&spec, "wallet.spec.xdr").unwrap();
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
            let err = generate_ts_bindings(&spec, "wallet.spec.xdr").unwrap_err();
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
        let bytes = xdrs::txs::encode(&spec).unwrap();
        let mut spec = crate::contract_spec::load_contract_spec_from_xdr(&bytes).unwrap();
        generate_ts_bindings(&spec, "shared.spec.xdr").unwrap();

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
            generate_ts_bindings(&spec, "shared.spec.xdr")
                .unwrap_err()
                .to_string()
                .contains("carries a signed token")
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
        assert!(!type_carries_signed_token(
            &ContractType::Named {
                named: spec_id("Node0")
            },
            &index,
        ));
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
        ));
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
        let output = generate_ts_bindings(&spec, "wallet.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec_with_events(), "counter.spec.xdr").unwrap();

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
        let output = generate_ts_bindings(&spec_with_events(), "counter.spec.xdr").unwrap();

        assert!(output.contains("const __gh_s = __gh_contract_spec.contractSpecString;"));
        assert!(output.contains("name: __gh_s(\"Counter\")"));
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
        let error = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
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
        let output = generate_ts_bindings(&spec, "erc20.spec.xdr").unwrap();
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

        let output = generate_ts_bindings(&spec, "token.spec.xdr").unwrap();
        assert!(output.contains("export const ERC20 = {"));
        assert!(output.contains("export const ERC20Metadata = {"));
        assert!(output.contains("    initialize: __gh_client_contract.defineContractMethod<"));
        assert!(output.contains("entrypoint: __gh_entrypoints.ERC20.v0_1.initialize,"));
        assert!(output.contains("entrypoint: __gh_entrypoints.ERC20Metadata.v0_1.initialize,"));
        assert!(output.contains("type: spec.interfaces[0]!.methods[0]!.args[0]!.ty"));
        assert!(output.contains("type: spec.interfaces[1]!.methods[0]!.args[0]!.ty"));
    }

    /// An interface named `name` at version `vers` with one method.
    fn interface(name: &str, vers: &str, method: &str) -> ContractSpecInterface {
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

    fn spec_with_interfaces(interfaces: Vec<ContractSpecInterface>) -> ContractSpec {
        ContractSpec {
            interfaces: interfaces.try_into().unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        }
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
        let output = generate_ts_bindings(&spec, "token.spec.xdr").unwrap();
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
        let error = generate_ts_bindings(&spec, "token.spec.xdr").unwrap_err();
        assert_eq!(
            error.to_string(),
            "cannot generate TypeScript bindings: interface \"Token\" versions 1.0.0 and 1.2.0 \
             would both be \"Token.v1\" in the generated TypeScript"
        );
    }

    #[test]
    fn an_interface_version_must_be_semantic() {
        let spec = spec_with_interfaces(vec![interface("Token", "1", "balance")]);
        let error = generate_ts_bindings(&spec, "token.spec.xdr").unwrap_err();
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
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
        let error =
            generate_ts_bindings(&spec_with_interfaces(vec![iface]), "x.spec.xdr").unwrap_err();
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
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
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

        let err = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap_err();
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

        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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
            let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec, "events.spec.xdr").unwrap();
        assert!(
            output.contains("export type RecordFilter = globalThis.Record<string, never>;"),
            "filter must reach the global:\n{output}"
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
        let output = generate_ts_bindings(&cross_crate_spec("demo::other"), "x.spec.xdr").unwrap();
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
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
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

        let output = generate_ts_bindings(&spec, "events.spec.xdr").unwrap();
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
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
            let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
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

    fn struct_at(name: ContractSpecId) -> ContractTypeDef {
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

    fn enum_at(name: ContractSpecId) -> ContractTypeDef {
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

    fn union_at(name: ContractSpecId) -> ContractTypeDef {
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
    fn beside_a_module_holding(outer: ContractTypeDef, inner: ContractTypeDef) -> ContractSpec {
        let mut spec = spec_with_events();
        spec.events = vec![].try_into().unwrap();
        spec.type_defs = vec![outer, inner].try_into().unwrap();
        spec
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
            let output = generate_ts_bindings(&beside_a_module_holding(outer, inner), "x.spec.xdr")
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
            generate_ts_bindings(&spec, "x.spec.xdr")
                .unwrap_or_else(|error| panic!("{module}: {error}"));
        }
        let spec = beside_a_module_holding(
            enum_at(spec_id_at("demo", "demo", "foo")),
            enum_at(spec_id_at("demo", "demo::foo", "Bar")),
        );
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
        assert!(
            output.contains("    foo: {\n      \"A\": 1n,\n      Bar: {\n"),
            "{output}"
        );
        let mut spec = spec_with_events();
        spec.events.0[0].name = spec_id_at("demo", "demo::foo", "Ping");
        spec.type_defs = vec![enum_at(spec_id_at("demo", "demo", "foo"))]
            .try_into()
            .unwrap();
        generate_ts_bindings(&spec, "x.spec.xdr").unwrap();

        // Unless a variant takes the key a module's value needs.
        let spec = beside_a_module_holding(
            enum_at(spec_id_at("demo", "demo", "foo")),
            enum_at(spec_id_at("demo", "demo::foo", "A")),
        );
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
            let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
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
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
        assert_eq!(
            error.to_string(),
            "cannot generate TypeScript bindings: demo::A$b (demo@1) has \"$\" in its name, which \
             the generated TypeScript keeps for the names it escapes. Rename it."
        );

        let spec = counter_spec_with_interface_name("Count$r", vec![], false);
        let error = generate_ts_bindings(&spec, "x.spec.xdr").unwrap_err();
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
            let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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

        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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
                effect: None,
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
                effect: None,
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

    /// `get() -> U64` and `increment(amount: U64) -> Void`.
    fn counter_spec_methods() -> Vec<ContractSpecMethod> {
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

    #[test]
    fn effects_are_documented_and_preserved_without_changing_entrypoints() {
        use thruput_contract_spec::ContractMethodEffect as Effect;
        let mut spec = counter_spec();
        let baseline = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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
            let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();
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
            generate_ts_bindings(&spec, "counter.spec.xdr")
                .unwrap_err()
                .to_string()
                .contains("unsupported method effect")
        );
    }

    /// Build a minimal counter contract spec with [`counter_spec_methods`].
    fn counter_spec() -> ContractSpec {
        counter_spec_with_methods(counter_spec_methods(), false)
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
            let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
            assert!(output.contains(imports), "{output}");
            let lines = output.lines().filter(|line| line.starts_with("import "));
            assert_eq!(lines.count(), RUNTIME_MODULES.len(), "{output}");
        }
    }

    #[test]
    fn generates_full_binding_for_simple_contract() {
        let spec = counter_spec();
        let output = generate_ts_bindings(&spec, "counter.spec.xdr").unwrap();

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

        assert_eq!(
            emit_type_def(&def).unwrap(),
            "export type SignedValues = -2n | 0n;"
        );
        let mut spec = counter_spec();
        spec.type_defs = vec![def].try_into().unwrap();
        let output = generate_ts_bindings(&spec, "x.spec.xdr").unwrap();
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

        let error = generate_ts_bindings(&spec, "duplicate-enum.xdr").unwrap_err();
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

        let error = generate_ts_bindings(&spec, "duplicate-union.xdr").unwrap_err();
        assert!(
            error.to_string().contains(
                "union demo::DuplicateDiscriminants (demo@1) has duplicate discriminant -1"
            )
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

        let output = generate_ts_bindings(&spec, "events.spec.xdr").unwrap();
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

        let output = generate_ts_bindings(&spec, "events.spec.xdr").unwrap();

        assert!(output.contains("\"unsigned\": bigint"));
        assert!(output.contains("\"signed\": bigint"));
        assert!(
            output.contains("\"unsigned\"?: __gh_client_contract.ContractEventFilterValue<bigint>")
        );
        assert!(
            output.contains("\"signed\"?: __gh_client_contract.ContractEventFilterValue<bigint>")
        );
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
            let output = generate_ts_bindings(&spec, "scalar.spec.xdr").unwrap();
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
            let output = generate_ts_bindings(&spec, "hashed.spec.xdr").unwrap();
            assert!(output.contains("\"tag\": globalThis.Uint8Array"));
            assert!(output.contains("original value cannot be recovered"));
            assert!(output.contains("representation: \"hash\""));
            assert!(
                output.contains("export type PingEventFilter = globalThis.Record<string, never>;")
            );
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
                effect: None,
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
    fn shadowed_names_spec() -> ContractSpec {
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
    fn namespace_merges_spec() -> ContractSpec {
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
    fn escaped_names_spec() -> ContractSpec {
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
            (
                "shadowed_names",
                "shadowed_names.spec.xdr",
                shadowed_names_spec(),
            ),
            (
                "namespace_merges",
                "namespace_merges.spec.xdr",
                namespace_merges_spec(),
            ),
            (
                "escaped_names",
                "escaped_names.spec.xdr",
                escaped_names_spec(),
            ),
        ] {
            let output = generate_ts_bindings(&spec, source_name).unwrap();
            assert_codegen_snapshot(name, &output);
            // The artifact is checked in beside the binding so CI can run the
            // real `thruput codegen ts` over it and type-check the result.
            assert_spec_artifact_snapshot(name, &spec);
        }
    }

    /// Keep the checked-in `.spec.xdr` artifact in step with the spec the
    /// snapshot above is generated from, so the type-check job and these
    /// tests are never reasoning about different inputs.
    fn assert_spec_artifact_snapshot(name: &str, spec: &ContractSpec) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/codegen")
            .join(format!("{name}.spec.xdr"));
        let encoded = xdrs::txs::encode(spec).expect("bug: fixture spec should encode");
        if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &encoded).unwrap();
            return;
        }
        let expected = std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read spec artifact {}: {error}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        });
        assert_eq!(
            encoded,
            expected,
            "spec artifact {} changed",
            path.display()
        );
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
