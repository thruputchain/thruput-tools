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
            // Render the element once; nested tuple expansion grows exponentially.
            Ok(format!(
                "__gh_client_contract.FixedArray<{inner}, {}>",
                array.len
            ))
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
    let mut checked_event_types = BTreeSet::new();
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
            if type_carries_signed_token(&field.ty, &type_def_index, &mut checked_event_types) {
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
/// through `type_defs`. Share visited definitions across event fields: a false
/// result exhausts the reachable graph, and a true result stops validation.
/// The work list also handles long chains and cycles without recursion.
fn type_carries_signed_token<'a>(
    ty: &'a ContractType,
    type_defs: &BTreeMap<&ContractSpecId, &'a ContractTypeDef>,
    visited: &mut BTreeSet<&'a ContractSpecId>,
) -> bool {
    let mut pending = vec![ty];
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
mod tests;
