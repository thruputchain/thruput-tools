//! Derives the per-variant metadata the CLI publishes for every error: its
//! stable machine-readable code, its user-facing hint, and the structured
//! details its `--json` payload carries.
//!
//! The metadata is declared beside the variant it describes, next to the
//! `thiserror` `#[error(...)]` attribute, so adding a variant is one edit in
//! one place instead of an edit plus an arm in each hand-maintained match.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Data, DeriveInput, Error, Expr, Fields, Ident, LitStr, Meta, Token, Variant,
    parse_macro_input,
};

/// Generates `code`, `hint`, `details_json`, and the `CODES` catalog from
/// per-variant `#[code]`, `#[hint]`, and `#[details]` attributes.
///
/// - `#[code("...")]` overrides the code, which defaults to the variant name
///   in `snake_case`.
/// - `#[hint("text {field}", args...)]` formats a hint the way `#[error(...)]`
///   formats a message; `#[hint(text = CONST)]` reuses a `&'static str` shared
///   by several variants; `#[hint(expr = ...)]` evaluates an expression
///   yielding `Option<Cow<'static, str>>` with the variant's fields in scope.
/// - `#[details]` emits every named field under its `camelCase` key,
///   `#[details(a, b)]` emits the named subset, and `#[details(expr = ...)]`
///   evaluates an expression yielding `Option<serde_json::Value>`.
///
/// A variant with no `#[hint]` has no hint; a variant with no `#[details]`
/// carries no structured details.
#[proc_macro_derive(ErrorMeta, attributes(code, hint, details))]
pub fn derive_error_meta(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Enum(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "ErrorMeta can only be derived for enums",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &input.generics,
            "ErrorMeta does not support generic enums",
        ));
    }

    let ident = &input.ident;
    let mut catalog = Vec::new();
    let mut code_arms = Vec::new();
    let mut hint_arms = Vec::new();
    let mut details_arms = Vec::new();

    for variant in &data.variants {
        let meta = VariantMeta::parse(variant)?;
        let name = variant.ident.to_string();
        let code = meta.code;
        catalog.push(quote!((#name, #code)));

        let unbound = pattern(variant, false);
        code_arms.push(quote!(#unbound => #code,));
        hint_arms.push(arm(variant, meta.hint.as_ref().map(Hint::body)));
        details_arms.push(arm(
            variant,
            meta.details
                .as_ref()
                .map(|details| details.body(variant))
                .transpose()?,
        ));
    }

    Ok(quote! {
        #[automatically_derived]
        impl #ident {
            /// Every variant's name paired with its stable code, in
            /// declaration order.
            ///
            /// Codes are a machine-readable contract, so pin this catalog
            /// with a snapshot test: renaming a variant renames its derived
            /// code, and the test is what makes that visible.
            pub const CODES: &'static [(&'static str, &'static str)] = &[#(#catalog,)*];

            /// Stable machine-readable error code.
            #[allow(clippy::too_many_lines)]
            pub fn code(&self) -> &'static str {
                match self { #(#code_arms)* }
            }

            /// Actionable hint for the user, when one applies.
            #[allow(clippy::too_many_lines)]
            pub fn hint(&self) -> ::core::option::Option<::std::borrow::Cow<'static, str>> {
                match self { #(#hint_arms)* }
            }

            /// Structured details for `--json` error payloads, when the
            /// variant carries any.
            #[allow(clippy::too_many_lines)]
            pub fn details_json(&self) -> ::core::option::Option<::serde_json::Value> {
                match self { #(#details_arms)* }
            }
        }
    })
}

/// One variant's parsed metadata attributes.
struct VariantMeta {
    code: String,
    hint: Option<Hint>,
    details: Option<Details>,
}

impl VariantMeta {
    fn parse(variant: &Variant) -> syn::Result<Self> {
        let mut code = None;
        let mut hint = None;
        let mut details = None;
        for attr in &variant.attrs {
            if attr.path().is_ident("code") {
                reject_duplicate(attr, code.is_some())?;
                code = Some(attr.parse_args::<LitStr>()?.value());
            } else if attr.path().is_ident("hint") {
                reject_duplicate(attr, hint.is_some())?;
                hint = Some(Hint::parse(attr)?);
            } else if attr.path().is_ident("details") {
                reject_duplicate(attr, details.is_some())?;
                details = Some(Details::parse(attr)?);
            }
        }
        Ok(Self {
            code: code.unwrap_or_else(|| snake_case(&variant.ident.to_string())),
            hint,
            details,
        })
    }
}

fn reject_duplicate(attr: &Attribute, seen: bool) -> syn::Result<()> {
    if seen {
        return Err(Error::new_spanned(attr, "duplicate attribute"));
    }
    Ok(())
}

/// How a variant's hint text is produced.
enum Hint {
    /// A format string over the variant's fields, as `#[error(...)]` writes one.
    Format { lit: LitStr, args: Vec<Expr> },
    /// A `&'static str` constant shared by several variants.
    Text(Expr),
    /// An expression yielding the hint, with the variant's fields in scope.
    Expr(Expr),
}

impl Hint {
    fn parse(attr: &Attribute) -> syn::Result<Self> {
        attr.parse_args_with(|input: ParseStream| {
            if input.peek(LitStr) {
                let lit = rewrite_positional(&input.parse::<LitStr>()?);
                let mut args = Vec::new();
                while input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                    if input.is_empty() {
                        break;
                    }
                    args.push(input.parse()?);
                }
                return Ok(Self::Format { lit, args });
            }
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let expr: Expr = input.parse()?;
            match key.to_string().as_str() {
                "text" => Ok(Self::Text(expr)),
                "expr" => Ok(Self::Expr(expr)),
                other => Err(Error::new(
                    key.span(),
                    format!(
                        "unknown hint form `{other}`; \
                         expected a format string, `text = <const>`, or `expr = <expression>`"
                    ),
                )),
            }
        })
    }

    fn body(&self) -> Body {
        match self {
            Self::Format { lit, args } => {
                if args.is_empty() && !has_placeholder(&lit.value()) {
                    Body::unbound(quote!(::core::option::Option::Some(
                        ::std::borrow::Cow::Borrowed(#lit)
                    )))
                } else {
                    Body::bound(quote!(::core::option::Option::Some(
                        ::std::borrow::Cow::Owned(::std::format!(#lit #(, #args)*))
                    )))
                }
            }
            Self::Text(expr) => Body::unbound(quote!(::core::option::Option::Some(
                ::std::borrow::Cow::Borrowed(#expr)
            ))),
            Self::Expr(expr) => Body::bound(quote!(#expr)),
        }
    }
}

/// How a variant's structured JSON details are produced.
enum Details {
    /// Every named field, under its `camelCase` key.
    All,
    /// The named subset, under their `camelCase` keys.
    Fields(Vec<Ident>),
    /// An expression yielding the payload, with the variant's fields in scope.
    Expr(Expr),
}

impl Details {
    fn parse(attr: &Attribute) -> syn::Result<Self> {
        if matches!(attr.meta, Meta::Path(_)) {
            return Ok(Self::All);
        }
        attr.parse_args_with(|input: ParseStream| {
            if input.peek(Ident) && input.peek2(Token![=]) {
                let key: Ident = input.parse()?;
                input.parse::<Token![=]>()?;
                let expr: Expr = input.parse()?;
                if key == "expr" {
                    return Ok(Self::Expr(expr));
                }
                return Err(Error::new(
                    key.span(),
                    "unknown details form; expected field names or `expr = <expression>`",
                ));
            }
            let fields = Punctuated::<Ident, Token![,]>::parse_terminated(input)?;
            Ok(Self::Fields(fields.into_iter().collect()))
        })
    }

    fn body(&self, variant: &Variant) -> syn::Result<Body> {
        let selected = match self {
            Self::Expr(expr) => return Ok(Body::bound(quote!(#expr))),
            Self::All => named_fields(variant)?,
            Self::Fields(fields) => {
                let declared = named_fields(variant)?;
                for field in fields {
                    if !declared.iter().any(|declared| declared == field) {
                        return Err(Error::new_spanned(
                            field,
                            format!("`{field}` is not a field of this variant"),
                        ));
                    }
                }
                fields.clone()
            }
        };
        let keys = selected
            .iter()
            .map(|field| camel_case(&field.to_string()))
            .collect::<Vec<_>>();
        Ok(Body::bound(quote!(::core::option::Option::Some(
            ::serde_json::json!({ #(#keys: #selected,)* })
        ))))
    }
}

/// A generated match-arm body and whether it reads the variant's fields.
struct Body {
    tokens: TokenStream2,
    binds_fields: bool,
}

impl Body {
    fn bound(tokens: TokenStream2) -> Self {
        Self {
            tokens,
            binds_fields: true,
        }
    }

    fn unbound(tokens: TokenStream2) -> Self {
        Self {
            tokens,
            binds_fields: false,
        }
    }
}

fn arm(variant: &Variant, body: Option<Body>) -> TokenStream2 {
    let Some(body) = body else {
        let pattern = pattern(variant, false);
        return quote!(#pattern => ::core::option::Option::None,);
    };
    let pattern = pattern(variant, body.binds_fields);
    let tokens = body.tokens;
    if body.binds_fields {
        quote!(#[allow(unused_variables)] #pattern => #tokens,)
    } else {
        quote!(#pattern => #tokens,)
    }
}

fn pattern(variant: &Variant, bind: bool) -> TokenStream2 {
    let name = &variant.ident;
    if !bind {
        return quote!(Self::#name { .. });
    }
    match &variant.fields {
        Fields::Named(fields) => {
            let names = fields.named.iter().map(field_ident);
            quote!(Self::#name { #(#names,)* .. })
        }
        Fields::Unnamed(fields) => {
            let names = (0..fields.unnamed.len()).map(|index| format_ident!("_{}", index));
            quote!(Self::#name(#(#names),*))
        }
        Fields::Unit => quote!(Self::#name),
    }
}

fn named_fields(variant: &Variant) -> syn::Result<Vec<Ident>> {
    match &variant.fields {
        Fields::Named(fields) => Ok(fields
            .named
            .iter()
            .map(|field| field_ident(field).clone())
            .collect()),
        _ => Err(Error::new_spanned(
            &variant.ident,
            "field details need named fields; use `details(expr = ...)` instead",
        )),
    }
}

fn field_ident(field: &syn::Field) -> &Ident {
    field
        .ident
        .as_ref()
        .expect("bug: named field always has an identifier")
}

/// Rewrites `thiserror`-style positional placeholders onto the tuple-field
/// bindings the generated match arm introduces, so `{0}` reads the first
/// field exactly as it does in `#[error(...)]`.
fn rewrite_positional(lit: &LitStr) -> LitStr {
    let source = lit.value();
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(current) = chars.next() {
        out.push(current);
        if current != '{' {
            continue;
        }
        match chars.peek() {
            Some('{') => {
                out.push('{');
                chars.next();
            }
            Some(next) if next.is_ascii_digit() => out.push('_'),
            _ => {}
        }
    }
    LitStr::new(&out, lit.span())
}

fn has_placeholder(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(current) = chars.next() {
        if current == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                continue;
            }
            return true;
        }
    }
    false
}

fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    for (index, character) in name.char_indices() {
        if character.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    for character in name.chars() {
        if character == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(character.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{camel_case, has_placeholder, rewrite_positional, snake_case};
    use syn::LitStr;

    #[test]
    fn snake_case_splits_on_capitals_and_keeps_digit_runs() {
        assert_eq!(snake_case("CargoNotFound"), "cargo_not_found");
        assert_eq!(snake_case("Rv32ElfNotFound"), "rv32_elf_not_found");
        assert_eq!(snake_case("Io"), "io");
    }

    #[test]
    fn camel_case_keys_match_the_json_wire_convention() {
        assert_eq!(camel_case("retry_after_seconds"), "retryAfterSeconds");
        assert_eq!(camel_case("path"), "path");
    }

    #[test]
    fn positional_placeholders_address_tuple_bindings() {
        let lit = LitStr::new(
            "remove `{0}` or {{literal}} {1:?}",
            proc_macro2::Span::call_site(),
        );
        assert_eq!(
            rewrite_positional(&lit).value(),
            "remove `{_0}` or {{literal}} {_1:?}"
        );
    }

    #[test]
    fn escaped_braces_are_not_placeholders() {
        assert!(!has_placeholder("no placeholders {{here}}"));
        assert!(has_placeholder("one {here}"));
    }
}
