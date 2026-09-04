//! `#[derive(Error)]`, covering the subset of `thiserror` this workspace uses.
//!
//! Supported on enums:
//!
//! - `#[error("...")]` on a variant, with the variant's fields in scope. Tuple
//!   fields are also available positionally, so `{0}` works.
//! - `#[error("...", extra = EXPR)]` to pass further arguments to the format.
//! - `#[error(transparent)]` to defer `Display` to the single inner field and
//!   `source` to that field's own source.
//! - `#[from]` on a field, which additionally generates the `From` impl.
//! - `#[source]` on a field, which only marks it as the source.
//! - a field named `source` with no attribute at all, which is taken as the
//!   source. An explicit `#[from]` or `#[source]` anywhere in the variant wins
//!   over it.
//!
//! Supported on structs: `#[error("...")]` only, giving a `Display` impl and an
//! `std::error::Error` impl with no source. `#[from]`, `#[source]`, a field
//! named `source` and `#[error(transparent)]` are rejected on a struct.
//!
//! Unsupported anywhere, because nothing here needs them: generics, lifetimes,
//! `#[backtrace]`, unions, more than one `#[from]` or `#[source]` per variant,
//! `#[from]` on a variant that has more than one field, and `#[source]` on a
//! variant that is already `#[error(transparent)]`.
//!
//! `#[error]` is read on an enum variant and on a struct itself, and `#[from]`
//! and `#[source]` on a field of a variant. In any other position each of them
//! is a compile error rather than a silent no-op.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use std::{iter::Peekable, str::Chars};
use syn::{
    parse_macro_input, punctuated::Punctuated, spanned::Spanned, Data, DeriveInput, Expr, Fields,
    Ident, LitStr, Token, Variant,
};

#[proc_macro_derive(Error, attributes(error, from, source))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// How a variant renders itself.
enum Display {
    /// A format string plus any extra arguments given after it.
    Format(LitStr, Vec<Expr>),
    /// Defer to the inner field.
    Transparent,
}

/// Which field, if any, is this variant's source, and how it was marked.
struct Source {
    index: usize,
    ident: Option<Ident>,
    marker: Marker,
}

/// What made a field the source.
enum Marker {
    /// `#[from]`, which also generates the `From` impl.
    From,
    /// An explicit `#[source]`, carrying the attribute's span.
    Source(Span),
    /// A field named `source` carrying no attribute.
    FieldName,
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "#[derive(Error)] does not support generics or lifetimes",
        ));
    }
    match &input.data {
        Data::Enum(data) => expand_enum(input, data),
        Data::Struct(data) => expand_struct(input, data),
        Data::Union(_) => Err(syn::Error::new(
            input.ident.span(),
            "#[derive(Error)] does not support unions",
        )),
    }
}

fn expand_enum(input: &DeriveInput, data: &syn::DataEnum) -> syn::Result<TokenStream2> {
    reject_error_attr(
        &input.attrs,
        "#[error] on an enum is not read, it belongs on each variant",
    )?;
    reject_source_attr(
        &input.attrs,
        "#[from] or #[source] on an enum is not read, it belongs on a field of a variant",
    )?;

    let name = &input.ident;
    let mut display_arms = Vec::new();
    let mut source_arms = Vec::new();
    let mut from_impls = Vec::new();
    let mut needs_as_dyn_error = false;

    for variant in &data.variants {
        reject_source_attr(
            &variant.attrs,
            "#[from] or #[source] on a variant is not read, it belongs on one of its fields",
        )?;
        for field in &variant.fields {
            reject_error_attr(
                &field.attrs,
                "#[error] on a field is not read, it belongs on the variant",
            )?;
        }
        let display = parse_display(variant)?;
        let source = parse_source(variant)?;
        if let (
            Display::Transparent,
            Some(Source {
                marker: Marker::Source(span),
                ..
            }),
        ) = (&display, &source)
        {
            return Err(syn::Error::new(
                *span,
                "#[error(transparent)] cannot be combined with #[source] on the same variant",
            ));
        }

        let bindings = bindings_for(&variant.fields);
        let pattern = pattern_for(&variant.fields, &bindings);

        display_arms.push(display_arm(variant, &display, &bindings, &pattern)?);
        needs_as_dyn_error |= matches!(display, Display::Transparent) || source.is_some();
        source_arms.push(source_arm(
            variant,
            &display,
            source.as_ref(),
            &bindings,
            &pattern,
        ));
        from_impls.extend(from_impl(name, variant, source.as_ref()));
    }

    let as_dyn_error = if needs_as_dyn_error {
        as_dyn_error_helper()
    } else {
        TokenStream2::new()
    };

    Ok(quote! {
        impl ::core::fmt::Display for #name {
            // `__f` is reserved: a field of the error may itself be called `f`.
            #[allow(unused_variables)]
            fn fmt(&self, __f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self {
                    #(#display_arms)*
                }
            }
        }

        impl ::std::error::Error for #name {
            #[allow(unused_variables)]
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                #as_dyn_error

                match self {
                    #(#source_arms)*
                }
            }
        }

        #(#from_impls)*
    })
}

/// The `Display` arm for one variant.
fn display_arm(
    variant: &Variant,
    display: &Display,
    bindings: &[Ident],
    pattern: &TokenStream2,
) -> syn::Result<TokenStream2> {
    let body = match display {
        Display::Transparent => {
            if bindings.len() != 1 {
                return Err(syn::Error::new(
                    variant.ident.span(),
                    "transparent needs exactly one field",
                ));
            }
            let inner = &bindings[0];
            quote! { ::core::fmt::Display::fmt(#inner, __f) }
        }
        // Tuple fields have no names to capture, so rewrite `{0}` and `{}` to
        // the `_0`, `_1`, ... bindings the pattern introduces. Named fields
        // already resolve as inline captures.
        Display::Format(literal, extra) => {
            let literal = rewrite_positional(literal, has_positional_extra(extra))?;
            quote! { ::core::write!(__f, #literal #(, #extra)*) }
        }
    };
    let variant_name = &variant.ident;
    Ok(quote! { Self::#variant_name #pattern => #body, })
}

/// The `Error::source` arm for one variant.
fn source_arm(
    variant: &Variant,
    display: &Display,
    source: Option<&Source>,
    bindings: &[Ident],
    pattern: &TokenStream2,
) -> TokenStream2 {
    let variant_name = &variant.ident;
    match (display, source) {
        // A transparent variant adds no level to the chain, so it reports the
        // inner error's own source rather than the inner error.
        (Display::Transparent, _) => {
            let inner = &bindings[0];
            quote! { Self::#variant_name #pattern => ::std::error::Error::source(#inner.as_dyn_error()), }
        }
        (_, Some(source)) => {
            let field = &bindings[source.index];
            quote! { Self::#variant_name #pattern => ::core::option::Option::Some(#field.as_dyn_error()), }
        }
        (_, None) => quote! { Self::#variant_name { .. } => ::core::option::Option::None, },
    }
}

/// The `From` impl a `#[from]` field asks for, if the variant marks one.
fn from_impl(name: &Ident, variant: &Variant, source: Option<&Source>) -> Option<TokenStream2> {
    let source = source?;
    if !matches!(source.marker, Marker::From) {
        return None;
    }
    let variant_name = &variant.ident;
    let field = &variant.fields.iter().nth(source.index).expect("checked").ty;
    let construct = match &source.ident {
        Some(ident) => quote! { Self::#variant_name { #ident: value } },
        None => quote! { Self::#variant_name(value) },
    };
    Some(quote! {
        impl ::core::convert::From<#field> for #name {
            fn from(value: #field) -> Self {
                #construct
            }
        }
    })
}

/// A source may be a concrete error or a boxed trait object, and only the former
/// coerces on its own, so `source()` reaches both through this trait.
fn as_dyn_error_helper() -> TokenStream2 {
    quote! {
        trait AsDynError<'a> {
            fn as_dyn_error(&self) -> &(dyn ::std::error::Error + 'a);
        }
        impl<'a, T: ::std::error::Error + 'a> AsDynError<'a> for T {
            fn as_dyn_error(&self) -> &(dyn ::std::error::Error + 'a) {
                self
            }
        }
        impl<'a> AsDynError<'a> for dyn ::std::error::Error + 'a {
            fn as_dyn_error(&self) -> &(dyn ::std::error::Error + 'a) {
                self
            }
        }
        impl<'a> AsDynError<'a> for dyn ::std::error::Error + ::core::marker::Send + 'a {
            fn as_dyn_error(&self) -> &(dyn ::std::error::Error + 'a) {
                self
            }
        }
        impl<'a> AsDynError<'a> for dyn ::std::error::Error + ::core::marker::Send + ::core::marker::Sync + 'a {
            fn as_dyn_error(&self) -> &(dyn ::std::error::Error + 'a) {
                self
            }
        }
    }
}

/// `#[error]` is only read on an enum variant and on a struct itself, so in any
/// other position it would quietly do nothing.
fn reject_error_attr(attrs: &[syn::Attribute], message: &str) -> syn::Result<()> {
    match attrs.iter().find(|attr| attr.path().is_ident("error")) {
        Some(attr) => Err(syn::Error::new(attr.span(), message)),
        None => Ok(()),
    }
}

/// `#[from]` and `#[source]` are only read on a field, so in any other position
/// they would quietly do nothing.
fn reject_source_attr(attrs: &[syn::Attribute], message: &str) -> syn::Result<()> {
    let misplaced = attrs
        .iter()
        .find(|attr| attr.path().is_ident("from") || attr.path().is_ident("source"));
    match misplaced {
        Some(attr) => Err(syn::Error::new(attr.span(), message)),
        None => Ok(()),
    }
}

/// Whether any extra argument is positional. A named one (`name = EXPR`) parses
/// as an assignment and does not claim an implicit `{}` slot.
fn has_positional_extra(extra: &[Expr]) -> bool {
    extra.iter().any(|arg| !matches!(arg, Expr::Assign(_)))
}

/// Rewrites implicit and numbered placeholders to the `_0`, `_1`, ... bindings,
/// leaving named ones and `{{`/`}}` escapes alone.
fn rewrite_positional(literal: &LitStr, has_positional_extra: bool) -> syn::Result<LitStr> {
    let source = literal.value();
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut next_implicit = 0usize;

    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                out.push_str("{{");
                chars.next();
            }
            '{' => out.push_str(&rewrite_placeholder(
                &mut chars,
                literal,
                has_positional_extra,
                &mut next_implicit,
            )?),
            '}' if chars.peek() == Some(&'}') => {
                out.push_str("}}");
                chars.next();
            }
            _ => out.push(c),
        }
    }

    Ok(LitStr::new(&out, literal.span()))
}

/// Rewrites one placeholder, with `chars` sitting just after its opening `{`.
fn rewrite_placeholder(
    chars: &mut Peekable<Chars<'_>>,
    literal: &LitStr,
    has_positional_extra: bool,
    next_implicit: &mut usize,
) -> syn::Result<String> {
    let mut placeholder = String::new();
    let mut closed = false;
    for c in chars.by_ref() {
        if c == '}' {
            closed = true;
            break;
        }
        placeholder.push(c);
    }
    if !closed {
        return Err(syn::Error::new(
            literal.span(),
            "unterminated format placeholder, a `{` has no matching `}`",
        ));
    }
    let (name, spec) = match placeholder.find(':') {
        Some(i) => (&placeholder[..i], &placeholder[i..]),
        None => (placeholder.as_str(), ""),
    };
    let name = if name.is_empty() {
        // With a positional argument present, `{}` consumes one of those rather
        // than a field, so leave it for `format!` to resolve.
        if has_positional_extra {
            return Ok(format!("{{{placeholder}}}"));
        }
        let name = format!("_{next_implicit}");
        *next_implicit += 1;
        name
    } else if name.chars().all(|c| c.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name.to_string()
    };
    Ok(format!("{{{name}{spec}}}"))
}

/// Structs only get `Display` and a source-less `Error`, so anything that would
/// need a source or a `From` is refused instead of being quietly dropped.
fn expand_struct(input: &DeriveInput, data: &syn::DataStruct) -> syn::Result<TokenStream2> {
    let no_source =
        "#[derive(Error)] does not support #[from] or #[source] on a struct, use an enum";
    reject_source_attr(&input.attrs, no_source)?;
    for field in &data.fields {
        reject_error_attr(
            &field.attrs,
            "#[error] on a field is not read, it belongs on the struct",
        )?;
        reject_source_attr(&field.attrs, no_source)?;
        if let Some(ident) = &field.ident {
            if ident == "source" {
                return Err(syn::Error::new(
                    ident.span(),
                    "#[derive(Error)] does not support a `source` field on a struct, use an enum",
                ));
            }
        }
    }

    let display = parse_display_from(&input.attrs, input.ident.span())?;
    let Display::Format(literal, extra) = display else {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[derive(Error)] does not support #[error(transparent)] on a struct, use an enum",
        ));
    };

    let name = &input.ident;
    let bindings = bindings_for(&data.fields);
    let pattern = pattern_for(&data.fields, &bindings);
    let literal = rewrite_positional(&literal, has_positional_extra(&extra))?;

    Ok(quote! {
        impl ::core::fmt::Display for #name {
            // `__f` is reserved: a field of the error may itself be called `f`.
            #[allow(unused_variables)]
            fn fmt(&self, __f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                let Self #pattern = self;
                ::core::write!(__f, #literal #(, #extra)*)
            }
        }

        impl ::std::error::Error for #name {}
    })
}

fn parse_display(variant: &Variant) -> syn::Result<Display> {
    parse_display_from(&variant.attrs, variant.ident.span())
}

fn parse_display_from(attrs: &[syn::Attribute], span: Span) -> syn::Result<Display> {
    let mut found = None;
    for attr in attrs {
        if !attr.path().is_ident("error") {
            continue;
        }
        if found.is_some() {
            return Err(syn::Error::new(attr.span(), "duplicate #[error] attribute"));
        }
        // `#[error(transparent)]` or `#[error("literal", extra...)]`
        if let Ok(ident) = attr.parse_args::<Ident>() {
            if ident == "transparent" {
                found = Some(Display::Transparent);
                continue;
            }
            return Err(syn::Error::new(ident.span(), "expected a format string"));
        }
        let args = attr.parse_args_with(Punctuated::<Expr, Token![,]>::parse_terminated)?;
        let mut args = args.into_iter();
        let Some(Expr::Lit(literal)) = args.next() else {
            return Err(syn::Error::new(attr.span(), "expected a format string"));
        };
        let syn::Lit::Str(literal) = literal.lit else {
            return Err(syn::Error::new(attr.span(), "expected a format string"));
        };
        found = Some(Display::Format(literal, args.collect()));
    }
    found.ok_or_else(|| syn::Error::new(span, "missing #[error] attribute"))
}

fn parse_source(variant: &Variant) -> syn::Result<Option<Source>> {
    let mut tagged: Option<Source> = None;
    let mut named: Option<Source> = None;
    for (index, field) in variant.fields.iter().enumerate() {
        for attr in &field.attrs {
            let from = attr.path().is_ident("from");
            if !from && !attr.path().is_ident("source") {
                continue;
            }
            if tagged.is_some() {
                return Err(syn::Error::new(
                    attr.span(),
                    "only one #[from] or #[source] field per variant",
                ));
            }
            // The generated `From` builds the variant from that one field, so
            // any other field would have no value to take.
            if from && variant.fields.len() != 1 {
                return Err(syn::Error::new(
                    attr.span(),
                    "#[from] needs a variant with exactly one field",
                ));
            }
            tagged = Some(Source {
                index,
                ident: field.ident.clone(),
                marker: if from {
                    Marker::From
                } else {
                    Marker::Source(attr.span())
                },
            });
        }
        let is_named_source = field.ident.as_ref().is_some_and(|ident| ident == "source");
        if named.is_none() && is_named_source {
            named = Some(Source {
                index,
                ident: field.ident.clone(),
                marker: Marker::FieldName,
            });
        }
    }
    Ok(tagged.or(named))
}

fn bindings_for(fields: &Fields) -> Vec<Ident> {
    match fields {
        Fields::Unit => Vec::new(),
        Fields::Unnamed(f) => (0..f.unnamed.len())
            .map(|i| format_ident!("_{}", i))
            .collect(),
        Fields::Named(f) => f
            .named
            .iter()
            .map(|field| field.ident.clone().expect("named"))
            .collect(),
    }
}

fn pattern_for(fields: &Fields, bindings: &[Ident]) -> TokenStream2 {
    match fields {
        Fields::Unit => quote! {},
        Fields::Unnamed(_) => quote! { ( #(#bindings),* ) },
        Fields::Named(_) => quote! { { #(#bindings),* } },
    }
}
