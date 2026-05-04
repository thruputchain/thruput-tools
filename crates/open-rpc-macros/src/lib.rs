//! Proc macro for generating `OpenRPC` specs from jsonrpsee traits.
//!
//! Place `#[open_rpc(namespace = "groundhog", tag = "Core API")]` above
//! the jsonrpsee `#[rpc(...)]` attribute on a trait.  The macro emits a
//! companion struct `{TraitName}OpenRpc` with a
//! `pub fn module_doc() -> groundhog_open_rpc::Module` method that
//! builds the `OpenRPC` method descriptors at runtime.
//!
//! The original trait is passed through unchanged so that `#[rpc]` can
//! process it normally.

use proc_macro::TokenStream;
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
/// - `namespace` (required) -- the method prefix, e.g. `"groundhog"`.
/// - `tag` (optional) -- grouping label applied to every method.
///
/// # Example
///
/// ```ignore
/// #[open_rpc(namespace = "groundhog", tag = "Core API")]
/// #[rpc(client, server, namespace = "groundhog")]
/// pub trait GroundhogApi {
///     /// Submit a signed transaction.
///     #[method(name = "sendRawTransaction")]
///     async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash>;
/// }
/// ```
///
/// This generates `pub struct GroundhogApiOpenRpc;` with a
/// `module_doc()` method.
#[proc_macro_attribute]
pub fn open_rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_trait = parse_macro_input!(item as ItemTrait);
    let args = match parse_open_rpc_args(attr.into()) {
        Ok(a) => a,
        Err(e) => return e.into_compile_error().into(),
    };

    let companion = generate_companion(&item_trait, &args);

    let mut output = quote! { #item_trait };
    output.extend(companion);
    output.into()
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

struct OpenRpcArgs {
    namespace: String,
    tag: Option<String>,
}

fn parse_open_rpc_args(tokens: TokenStream2) -> syn::Result<OpenRpcArgs> {
    let mut namespace: Option<String> = None;
    let mut tag: Option<String> = None;

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("namespace") {
            let value: LitStr = meta.value()?.parse()?;
            namespace = Some(value.value());
            Ok(())
        } else if meta.path.is_ident("tag") {
            let value: LitStr = meta.value()?.parse()?;
            tag = Some(value.value());
            Ok(())
        } else {
            Err(meta.error("expected `namespace` or `tag`"))
        }
    });

    syn::parse::Parser::parse2(parser, tokens)?;

    let namespace = namespace.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "`namespace` argument is required",
        )
    })?;

    Ok(OpenRpcArgs { namespace, tag })
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

fn generate_companion(item_trait: &ItemTrait, args: &OpenRpcArgs) -> TokenStream2 {
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
            let full_name = format!("{namespace}_{rpc_name}");
            let summary = extract_doc_summary(&method.attrs);
            let tag_tokens = if let Some(t) = &args.tag {
                quote! {
                    vec![groundhog_open_rpc::Tag { name: #t.to_owned() }]
                }
            } else {
                quote! { vec![] }
            };

            // Build param descriptors (skip &self)
            let param_tokens: Vec<TokenStream2> = method
                .sig
                .inputs
                .iter()
                .filter_map(|arg| {
                    if let FnArg::Typed(pat_type) = arg {
                        let name = pat_to_name(&pat_type.pat);
                        let ty = &*pat_type.ty;
                        Some(quote! {
                            groundhog_open_rpc::param_descriptor::<#ty>(#name)
                        })
                    } else {
                        None // skip self
                    }
                })
                .collect();

            // Extract return type: unwrap RpcResult<T> -> T
            let result_type = extract_result_type(&method.sig.output);
            let result_name = type_display_name(&result_type);

            let summary_tokens = if let Some(s) = summary {
                quote! { Some(#s.to_owned()) }
            } else {
                quote! { None }
            };

            Some(quote! {
                module.add_method(groundhog_open_rpc::MethodDoc {
                    name: #full_name.to_owned(),
                    summary: #summary_tokens,
                    tags: #tag_tokens,
                    params: vec![#(#param_tokens),*],
                    result: groundhog_open_rpc::result_descriptor::<#result_type>(#result_name),
                });
            })
        })
        .collect();

    quote! {
        /// Generated `OpenRPC` metadata for the companion trait.
        #[derive(Debug)]
        pub struct #struct_name;

        impl #struct_name {
            /// Build the [`groundhog_open_rpc::Module`] describing every
            /// RPC method in the companion trait.
            pub fn module_doc() -> groundhog_open_rpc::Module {
                let mut module = groundhog_open_rpc::Module::new();
                #(#method_stmts)*
                module
            }
        }
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
/// applied to `#[rpc]` traits that use jsonrpsee's `RpcResult`.
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
