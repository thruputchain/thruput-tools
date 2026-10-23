//! Proc macro for generating `OpenRPC` specs from jsonrpsee traits.
//!
//! Place `#[open_rpc(namespace = "demo", tag = "Demo")]` above
//! the jsonrpsee `#[rpc(...)]` attribute on a trait.  The macro emits a
//! companion struct `{TraitName}OpenRpc` with a
//! `pub fn module_doc(builder: &mut <resolved-open-rpc-crate>::ModuleBuilder<'_>)`
//! method that appends the `OpenRPC` method descriptors at runtime.
//!
//! Positional mode passes the trait through for `#[rpc]`. Object mode emits
//! only documentation; use explicit whole-object server and client adapters.
//! Always place `#[open_rpc]` first: jsonrpsee removes other trait attributes
//! when it expands, so placing `#[rpc]` first bypasses this macro entirely.

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, FnArg, ItemTrait, LitStr, Meta, ReturnType, TraitItem, Type, parse_macro_input,
};

/// Attribute macro that generates an `OpenRPC` module companion struct
/// for a jsonrpsee `#[rpc]` trait.
///
/// # Arguments
///
/// - `namespace` (optional; omit for bare names) -- the method prefix, e.g. `"demo"`.
/// - `tag` (optional) -- grouping label applied to every method.
/// - `params = "object"` -- describe the fields of a single request DTO directly,
///   with `paramStructure` and `x-params-schema`. Emits only the companion, not
///   the trait or transport. Combining this mode with `#[rpc]` is an error.
///   Use explicit whole-object dispatch/client adapters.
///
/// This attribute must be outermost. In particular, jsonrpsee consumes an
/// `open_rpc` attribute placed below `rpc`, bypassing these checks.
///
/// # Example
///
/// ```ignore
/// #[open_rpc(namespace = "demo", tag = "Demo")]
/// #[rpc(client, server, namespace = "demo")]
/// pub trait DemoApi {
///     /// Echo the input.
///     #[method(name = "echo")]
///     async fn echo(&self, value: String) -> RpcResult<String>;
/// }
/// ```
///
/// This generates `pub struct DemoApiOpenRpc;` with a
/// `module_doc(...)` method.
///
/// Object documentation must not also generate wrapped jsonrpsee transport:
///
/// ```compile_fail
/// use thruput_open_rpc_macros::open_rpc;
/// use jsonrpsee::{core::RpcResult, proc_macros::rpc};
/// #[open_rpc(params = "object")]
/// #[rpc(server)]
/// trait Native {
///     #[method(name = "lookup")]
///     async fn lookup(&self, request: String) -> RpcResult<String>;
/// }
/// ```
///
/// Positional documentation and transport must name the same namespace:
///
/// ```compile_fail
/// use thruput_open_rpc_macros::open_rpc;
/// use jsonrpsee::{core::RpcResult, proc_macros::rpc};
/// #[open_rpc(namespace = "one")]
/// #[rpc(server, namespace = "two")]
/// trait Legacy {
///     #[method(name = "lookup")]
///     async fn lookup(&self, request: String) -> RpcResult<String>;
/// }
/// ```
///
/// Incorrect attribute order bypasses this macro. Requiring the companion in
/// the document builder catches that mistake, because it was never generated:
///
/// ```compile_fail
/// use thruput_open_rpc_macros::open_rpc;
/// use jsonrpsee::{core::RpcResult, proc_macros::rpc};
/// #[rpc(server)]
/// #[open_rpc(params = "object")]
/// trait Native {
///     #[method(name = "lookup")]
///     async fn lookup(&self, request: String) -> RpcResult<String>;
/// }
/// let _ = NativeOpenRpc::module_doc;
/// ```
#[proc_macro_attribute]
pub fn open_rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_trait = parse_macro_input!(item as ItemTrait);
    let args = match parse_open_rpc_args(attr.into()) {
        Ok(a) => a,
        Err(e) => return e.into_compile_error().into(),
    };
    let open_rpc_path = match resolve_open_rpc_path() {
        Ok(path) => path,
        Err(e) => return e.into_compile_error().into(),
    };

    match expand(&item_trait, &args, &open_rpc_path) {
        Ok(output) => output.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn expand(
    item_trait: &ItemTrait,
    args: &OpenRpcArgs,
    open_rpc_path: &TokenStream2,
) -> syn::Result<TokenStream2> {
    validate_transport(item_trait, args)?;
    if args.object_params {
        validate_object_methods(item_trait)?;
        return Ok(generate_companion(item_trait, args, open_rpc_path));
    }
    let companion = generate_companion(item_trait, args, open_rpc_path);
    Ok(quote! { #item_trait #companion })
}

fn validate_transport(item_trait: &ItemTrait, args: &OpenRpcArgs) -> syn::Result<()> {
    for attr in &item_trait.attrs {
        if attr
            .path()
            .segments
            .last()
            .is_none_or(|part| part.ident != "rpc")
        {
            continue;
        }
        if args.object_params {
            return Err(syn::Error::new_spanned(
                attr,
                "object params are documentation-only; remove #[rpc] and use explicit whole-object adapters",
            ));
        }
        let options = attr.parse_args_with(
            syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
        )?;
        let mut namespace = None;
        for option in options {
            if let Meta::NameValue(value) = option
                && value.path.is_ident("namespace")
            {
                if namespace.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate rpc namespace"));
                }
                let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(value),
                    ..
                }) = value.value
                else {
                    return Err(syn::Error::new_spanned(
                        value,
                        "expected a string namespace",
                    ));
                };
                namespace = Some(value.value());
            }
        }
        if namespace.as_deref().unwrap_or_default() != args.namespace {
            return Err(syn::Error::new_spanned(
                attr,
                "open_rpc and rpc namespaces must match",
            ));
        }
    }
    Ok(())
}

fn validate_object_methods(item_trait: &ItemTrait) -> syn::Result<()> {
    for item in &item_trait.items {
        let TraitItem::Fn(method) = item else {
            continue;
        };
        let mut has_name = false;
        for attr in &method.attrs {
            if !attr.path().is_ident("method") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("name") {
                    return Err(meta.error("documentation-only methods support only `name`"));
                }
                if has_name {
                    return Err(meta.error("duplicate method name"));
                }
                let value: LitStr = meta.value()?.parse()?;
                if value.value().is_empty() {
                    return Err(meta.error("method name must not be empty"));
                }
                has_name = true;
                Ok(())
            })?;
        }
        if !has_name {
            return Err(syn::Error::new_spanned(
                method,
                "object methods require #[method(name = \"...\")]",
            ));
        }
        if method
            .sig
            .inputs
            .iter()
            .filter(|arg| matches!(arg, FnArg::Typed(_)))
            .count()
            != 1
        {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "object params require exactly one request DTO (use an empty struct for noarg methods)",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

struct OpenRpcArgs {
    namespace: String,
    tag: Option<String>,
    object_params: bool,
}

fn parse_open_rpc_args(tokens: TokenStream2) -> syn::Result<OpenRpcArgs> {
    let mut namespace: Option<String> = None;
    let mut tag: Option<String> = None;
    let mut object_params = false;
    let mut seen_params = false;

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("namespace") {
            if namespace.is_some() {
                return Err(meta.error("duplicate namespace"));
            }
            let value: LitStr = meta.value()?.parse()?;
            namespace = Some(value.value());
            Ok(())
        } else if meta.path.is_ident("tag") {
            if tag.is_some() {
                return Err(meta.error("duplicate tag"));
            }
            let value: LitStr = meta.value()?.parse()?;
            tag = Some(value.value());
            Ok(())
        } else if meta.path.is_ident("params") {
            if seen_params {
                return Err(meta.error("duplicate params mode"));
            }
            seen_params = true;
            let value: LitStr = meta.value()?.parse()?;
            if value.value() != "object" {
                return Err(meta.error("expected params = \"object\""));
            }
            object_params = true;
            Ok(())
        } else {
            Err(meta.error("expected `namespace`, `tag` or `params`"))
        }
    });

    syn::parse::Parser::parse2(parser, tokens)?;

    Ok(OpenRpcArgs {
        namespace: namespace.unwrap_or_default(),
        tag,
        object_params,
    })
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

fn generate_companion(
    item_trait: &ItemTrait,
    args: &OpenRpcArgs,
    open_rpc_path: &TokenStream2,
) -> TokenStream2 {
    let struct_name = format_ident!("{}OpenRpc", item_trait.ident);
    let namespace = &args.namespace;

    let method_stmts: Vec<TokenStream2> = item_trait
        .items
        .iter()
        .filter_map(|item| {
            if let TraitItem::Fn(method) = item {
                Some(method)
            } else {
                None
            }
        })
        .filter_map(|method| {
            let rpc_name = extract_method_name(&method.attrs)?;
            let full_name = if namespace.is_empty() {
                rpc_name
            } else {
                format!("{namespace}_{rpc_name}")
            };
            let summary = extract_doc_summary(&method.attrs);
            let open_rpc_path = open_rpc_path.clone();
            let tag_tokens = if let Some(t) = &args.tag {
                quote! {
                    vec![#open_rpc_path::Tag { name: #t.to_owned() }]
                }
            } else {
                quote! { vec![] }
            };

            let params_tokens = generate_params(&method.sig, args.object_params, &open_rpc_path);

            // Extract return type: unwrap RpcResult<T> -> T
            let result_type = extract_result_type(&method.sig.output);
            let result_name = type_display_name(&result_type);

            let summary_tokens = if let Some(s) = summary {
                quote! { Some(#s.to_owned()) }
            } else {
                quote! { None }
            };
            let open_rpc_path = open_rpc_path.clone();

            Some(quote! {
                #params_tokens
                let result = {
                    let generator = builder.generator();
                    #open_rpc_path::result_descriptor::<#result_type>(generator, #result_name)
                };
                builder.add_method(#open_rpc_path::MethodDoc {
                    name: #full_name.to_owned(),
                    summary: #summary_tokens,
                    tags: #tag_tokens,
                    params,
                    named_params,
                    result,
                });
            })
        })
        .collect();

    quote! {
        /// Generated `OpenRPC` metadata for the companion trait.
        #[derive(Debug)]
        pub struct #struct_name;

        impl #struct_name {
            /// Append every RPC method in the companion trait to the
            /// provided module builder.
            pub fn module_doc(builder: &mut #open_rpc_path::ModuleBuilder<'_>) {
                #(#method_stmts)*
            }
        }
    }
}

fn generate_params(
    signature: &syn::Signature,
    object_params: bool,
    open_rpc_path: &TokenStream2,
) -> TokenStream2 {
    if object_params {
        let request = signature
            .inputs
            .iter()
            .find_map(|arg| match arg {
                FnArg::Typed(arg) => Some(&arg.ty),
                FnArg::Receiver(_) => None,
            })
            .expect("bug: object request arity was checked");
        return quote! {
            let (params, named_params) =
                #open_rpc_path::named_params::<#request>(builder.generator());
            let named_params = Some(named_params);
        };
    }

    let param_pushes = signature.inputs.iter().filter_map(|arg| {
        if let FnArg::Typed(pat_type) = arg {
            let name = pat_to_name(&pat_type.pat);
            let ty = &*pat_type.ty;
            Some(quote! {
                params.push({
                    let generator = builder.generator();
                    #open_rpc_path::param_descriptor::<#ty>(generator, #name)
                });
            })
        } else {
            None // skip self
        }
    });
    quote! {
        let mut params = Vec::new();
        #(#param_pushes)*
        let named_params = None;
    }
}

fn resolve_open_rpc_path() -> syn::Result<TokenStream2> {
    match crate_name("thruput-open-rpc") {
        Ok(FoundCrate::Itself) => Ok(quote!(crate)),
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{name}");
            Ok(quote!(#ident))
        }
        Err(err) => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("failed to resolve `thruput-open-rpc` dependency: {err}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Attribute helpers
// ---------------------------------------------------------------------------

/// Extract the `name = "..."` value from `#[method(name = "foo")]`.
fn extract_method_name(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("method") {
            continue;
        }

        let mut name = None;
        let result = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: LitStr = meta.value()?.parse()?;
                name = Some(value.value());
            }
            // Silently skip other keys (e.g. `blocking`)
            Ok(())
        });

        if result.is_ok()
            && let Some(n) = name
        {
            return Some(n);
        }
    }
    None
}

/// Join `#[doc = "..."]` attributes into a single summary string.
fn extract_doc_summary(attrs: &[Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter_map(|attr| {
            if !attr.path().is_ident("doc") {
                return None;
            }
            if let Meta::NameValue(nv) = &attr.meta
                && let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
            {
                return Some(s.value());
            }
            None
        })
        .collect();

    if lines.is_empty() {
        return None;
    }

    let joined = lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned();

    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

// ---------------------------------------------------------------------------
// Type helpers
// ---------------------------------------------------------------------------

/// Extract the inner `T` from `-> RpcResult<T>`. Falls back to `()`
/// for methods without an explicit return type.
fn extract_result_type(output: &ReturnType) -> Type {
    match output {
        ReturnType::Default => syn::parse_quote! { () },
        ReturnType::Type(_, ty) => unwrap_rpc_result(ty),
    }
}

/// Given a type like `RpcResult<Foo>`, return `Foo`.  If the outermost
/// type is not recognized as `RpcResult`, return it unchanged.
///
/// This matches the last path segment by name — the same approach
/// jsonrpsee's own proc macro uses. Proc macros see tokens, not
/// resolved types, so there is no way to structurally verify this is
/// `jsonrpsee::core::RpcResult`. Safe because this macro is only
/// applied to RPC declarations that use jsonrpsee's `RpcResult` convention.
fn unwrap_rpc_result(ty: &Type) -> Type {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        let ident = segment.ident.to_string();
        if ident == "RpcResult"
            && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
        {
            return inner.clone();
        }
    }
    ty.clone()
}

/// Produce a short display name from a type (used as the result
/// descriptor name).  For simple paths this is the last segment; for
/// anything exotic it falls back to `"result"`.
fn type_display_name(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                segment.ident.to_string()
            } else {
                "result".to_owned()
            }
        }
        Type::Tuple(t) if t.elems.is_empty() => "unit".to_owned(),
        _ => "result".to_owned(),
    }
}

/// Best-effort extraction of a parameter name from a pattern.
fn pat_to_name(pat: &syn::Pat) -> String {
    match pat {
        syn::Pat::Ident(pi) => pi.ident.to_string(),
        _ => "param".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_args() -> OpenRpcArgs {
        parse_open_rpc_args(quote!(params = "object")).expect("valid object mode")
    }

    #[test]
    fn object_mode_consumes_the_trait() {
        let input = syn::parse_quote! {
            trait Native {
                #[method(name = "getNetworkInfo")]
                async fn info(&self, request: EmptyRequest) -> RpcResult<Info>;
            }
        };
        let output =
            expand(&input, &object_args(), &quote!(docs)).expect("valid object declaration");
        let file: syn::File = syn::parse2(output).expect("valid generated Rust");
        assert_eq!(file.items.len(), 2);
        assert!(matches!(&file.items[0], syn::Item::Struct(item) if item.ident == "NativeOpenRpc"));
        assert!(matches!(&file.items[1], syn::Item::Impl(_)));
    }

    #[test]
    fn object_mode_rejects_transport_generation() {
        for attribute in [
            quote!(#[rpc(server)]),
            quote!(#[jsonrpsee::proc_macros::rpc(client)]),
        ] {
            let input = syn::parse2(quote! {
                #attribute
                trait Native {
                    #[method(name = "getNetworkInfo")]
                    async fn info(&self, request: EmptyRequest) -> RpcResult<Info>;
                }
            })
            .expect("valid trait syntax");
            let error =
                expand(&input, &object_args(), &quote!(docs)).expect_err("transport must fail");
            assert_eq!(
                error.to_string(),
                "object params are documentation-only; remove #[rpc] and use explicit whole-object adapters"
            );
        }
    }

    #[test]
    fn positional_mode_preserves_the_trait_and_checks_namespaces() {
        let input: ItemTrait = syn::parse_quote! {
            #[rpc(client, server, namespace = "demo")]
            trait Demo {
                #[method(name = "echo")]
                async fn echo(&self, value: String) -> RpcResult<String>;
            }
        };
        let args = parse_open_rpc_args(quote!(namespace = "demo")).expect("valid namespace");
        let output = expand(&input, &args, &quote!(docs)).expect("matching namespaces");
        let file: syn::File = syn::parse2(output).expect("valid generated Rust");
        assert!(
            matches!(&file.items[0], syn::Item::Trait(item) if quote!(#item).to_string() == quote!(#input).to_string())
        );
        let missing = parse_open_rpc_args(quote!()).expect("bare namespace");
        assert_eq!(
            expand(&input, &missing, &quote!(docs))
                .expect_err("namespace mismatch")
                .to_string(),
            "open_rpc and rpc namespaces must match"
        );
    }

    #[test]
    fn duplicate_configuration_and_unknown_modes_fail() {
        for (args, message) in [
            (
                quote!(namespace = "one", namespace = "two"),
                "duplicate namespace",
            ),
            (
                quote!(params = "object", params = "object"),
                "duplicate params mode",
            ),
            (quote!(params = "array"), "expected params = \"object\""),
            (quote!(tag = "one", tag = "two"), "duplicate tag"),
        ] {
            assert_eq!(
                parse_open_rpc_args(args)
                    .err()
                    .expect("invalid args")
                    .to_string(),
                message
            );
        }
    }

    #[test]
    fn invalid_object_method_declarations_fail() {
        for (method, message) in [
            (
                quote!(
                    #[method(name = "empty")]
                    fn empty(&self);
                ),
                "object params require exactly one request DTO (use an empty struct for noarg methods)",
            ),
            (
                quote!(
                    #[method(name = "many")]
                    fn many(&self, a: A, b: B);
                ),
                "object params require exactly one request DTO (use an empty struct for noarg methods)",
            ),
            (
                quote!(
                    #[method(name = "a", blocking)]
                    fn a(&self, a: A);
                ),
                "documentation-only methods support only `name`",
            ),
            (
                quote!(
                    #[method(name = "a", name = "b")]
                    fn a(&self, a: A);
                ),
                "duplicate method name",
            ),
            (
                quote!(
                    #[method(name = "")]
                    fn a(&self, a: A);
                ),
                "method name must not be empty",
            ),
        ] {
            let input = syn::parse2(quote!(trait Native { #method })).expect("valid trait syntax");
            assert_eq!(
                expand(&input, &object_args(), &quote!(docs))
                    .expect_err("invalid declaration")
                    .to_string(),
                message
            );
        }
    }
}
