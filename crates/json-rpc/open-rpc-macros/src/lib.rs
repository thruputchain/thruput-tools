//! Proc macro for generating `OpenRPC` specs from documentation-only API traits.
//!
//! Place `#[open_rpc]` on a trait whose methods each take one request DTO. The
//! macro consumes the trait and emits a companion struct `{TraitName}OpenRpc`
//! with a `pub fn module_doc(builder: &mut <resolved-open-rpc-crate>::ModuleBuilder<'_>)`
//! method that appends the `OpenRPC` method descriptors at runtime. It emits no
//! transport; use explicit whole-object server and client adapters.

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, FnArg, ItemTrait, LitStr, Meta, ReturnType, TraitItem, Type, parse_macro_input,
};

/// Attribute macro that generates an `OpenRPC` module companion struct for a
/// documentation-only API trait.
///
/// The attribute takes no arguments. Each method needs `#[method(name = "...")]`
/// and exactly one request DTO, whose fields are described directly with
/// `paramStructure` and `x-params-schema`. The macro consumes the trait, so
/// combining it with jsonrpsee's `#[rpc]` is an error.
///
/// # Example
///
/// ```ignore
/// #[open_rpc]
/// pub trait DemoApi {
///     /// Echo the input.
///     #[method(name = "echo")]
///     async fn echo(&self, request: EchoRequest) -> RpcResult<String>;
/// }
/// ```
///
/// This generates `pub struct DemoApiOpenRpc;` with a
/// `module_doc(...)` method.
///
/// Documentation must not also generate wrapped jsonrpsee transport:
///
/// ```compile_fail
/// use thruput_open_rpc_macros::open_rpc;
/// use jsonrpsee::{core::RpcResult, proc_macros::rpc};
/// #[open_rpc]
/// #[rpc(server)]
/// trait Native {
///     #[method(name = "lookup")]
///     async fn lookup(&self, request: String) -> RpcResult<String>;
/// }
/// ```
///
/// Placing `#[rpc]` first bypasses this macro, because jsonrpsee removes
/// other trait attributes when it expands. Requiring the companion in the
/// document builder catches that mistake, because it was never generated:
///
/// ```compile_fail
/// use thruput_open_rpc_macros::open_rpc;
/// use jsonrpsee::{core::RpcResult, proc_macros::rpc};
/// #[rpc(server)]
/// #[open_rpc]
/// trait Native {
///     #[method(name = "lookup")]
///     async fn lookup(&self, request: String) -> RpcResult<String>;
/// }
/// let _ = NativeOpenRpc::module_doc;
/// ```
///
/// The attribute rejects arguments:
///
/// ```compile_fail
/// use thruput_open_rpc_macros::open_rpc;
/// #[open_rpc(params = "object")]
/// trait Native {
///     #[method(name = "lookup")]
///     async fn lookup(&self, request: String) -> jsonrpsee::core::RpcResult<String>;
/// }
/// ```
#[proc_macro_attribute]
pub fn open_rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    if !attr.is_empty() {
        return syn::Error::new_spanned(attr, "open_rpc takes no arguments")
            .into_compile_error()
            .into();
    }
    let item_trait = parse_macro_input!(item as ItemTrait);
    let open_rpc_path = match resolve_open_rpc_path() {
        Ok(path) => path,
        Err(e) => return e.into_compile_error().into(),
    };

    match expand(&item_trait, &open_rpc_path) {
        Ok(output) => output.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn expand(item_trait: &ItemTrait, open_rpc_path: &TokenStream2) -> syn::Result<TokenStream2> {
    reject_transport(item_trait)?;
    validate_methods(item_trait)?;
    Ok(generate_companion(item_trait, open_rpc_path))
}

fn reject_transport(item_trait: &ItemTrait) -> syn::Result<()> {
    match item_trait.attrs.iter().find(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|part| part.ident == "rpc")
    }) {
        Some(attr) => Err(syn::Error::new_spanned(
            attr,
            "open_rpc is documentation-only; remove #[rpc] and use explicit whole-object adapters",
        )),
        None => Ok(()),
    }
}

fn validate_methods(item_trait: &ItemTrait) -> syn::Result<()> {
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
                "methods require #[method(name = \"...\")]",
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
                "methods require exactly one request DTO (use an empty struct for noarg methods)",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

fn generate_companion(item_trait: &ItemTrait, open_rpc_path: &TokenStream2) -> TokenStream2 {
    let struct_name = format_ident!("{}OpenRpc", item_trait.ident);

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
            let name = extract_method_name(&method.attrs)?;
            let summary = extract_doc_summary(&method.attrs);
            let request = request_type(&method.sig);

            // Extract return type: unwrap RpcResult<T> -> T
            let result_type = extract_result_type(&method.sig.output);
            let result_name = type_display_name(&result_type);

            let summary_tokens = if let Some(s) = summary {
                quote! { Some(#s.to_owned()) }
            } else {
                quote! { None }
            };

            Some(quote! {
                let (params, named_params) =
                    #open_rpc_path::named_params::<#request>(builder.generator());
                let result = {
                    let generator = builder.generator();
                    #open_rpc_path::result_descriptor::<#result_type>(generator, #result_name)
                };
                builder.add_method(#open_rpc_path::MethodDoc {
                    name: #name.to_owned(),
                    summary: #summary_tokens,
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

/// The single request DTO type; `validate_methods` checked the arity.
fn request_type(signature: &syn::Signature) -> &Type {
    signature
        .inputs
        .iter()
        .find_map(|arg| match arg {
            FnArg::Typed(arg) => Some(&*arg.ty),
            FnArg::Receiver(_) => None,
        })
        .expect("bug: request arity was checked")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_consumes_the_trait() {
        let input = syn::parse_quote! {
            trait Native {
                #[method(name = "getNetworkInfo")]
                async fn info(&self, request: EmptyRequest) -> RpcResult<Info>;
            }
        };
        let output = expand(&input, &quote!(docs)).expect("valid declaration");
        let file: syn::File = syn::parse2(output).expect("valid generated Rust");
        assert_eq!(file.items.len(), 2);
        assert!(matches!(&file.items[0], syn::Item::Struct(item) if item.ident == "NativeOpenRpc"));
        assert!(matches!(&file.items[1], syn::Item::Impl(_)));
    }

    #[test]
    fn transport_generation_is_rejected() {
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
            let error = expand(&input, &quote!(docs)).expect_err("transport must fail");
            assert_eq!(
                error.to_string(),
                "open_rpc is documentation-only; remove #[rpc] and use explicit whole-object adapters"
            );
        }
    }

    #[test]
    fn invalid_method_declarations_fail() {
        for (method, message) in [
            (
                quote!(
                    fn unnamed(&self, a: A);
                ),
                "methods require #[method(name = \"...\")]",
            ),
            (
                quote!(
                    #[method(name = "empty")]
                    fn empty(&self);
                ),
                "methods require exactly one request DTO (use an empty struct for noarg methods)",
            ),
            (
                quote!(
                    #[method(name = "many")]
                    fn many(&self, a: A, b: B);
                ),
                "methods require exactly one request DTO (use an empty struct for noarg methods)",
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
                expand(&input, &quote!(docs))
                    .expect_err("invalid declaration")
                    .to_string(),
                message
            );
        }
    }
}
