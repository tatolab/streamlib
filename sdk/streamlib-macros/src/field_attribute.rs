// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Taking one attribute an author writes on a field off it, for a macro that reads it.

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Meta};

/// Removes every `#[name …]` from `attributes` and returns the one written; `None` when none
/// is, and `refusal_of_a_second` at the second when two are.
pub(crate) fn take_the_one_attribute_named(
    attributes: &mut Vec<Attribute>,
    name: &str,
    refusal_of_a_second: &str,
) -> syn::Result<Option<Attribute>> {
    let mut taken = None;
    let mut kept_attributes = Vec::with_capacity(attributes.len());
    for attribute in attributes.drain(..) {
        if !attribute.path().is_ident(name) {
            kept_attributes.push(attribute);
            continue;
        }
        if taken.is_some() {
            return Err(syn::Error::new(attribute.span(), refusal_of_a_second));
        }
        taken = Some(attribute);
    }
    *attributes = kept_attributes;
    Ok(taken)
}

const DESCRIPTION_ATTRIBUTE: &str = "description";

/// Removes the field's `#[description = …]` and returns its value, refusing
/// any spelling but a string literal or the path of a `const`.
pub(crate) fn take_the_declared_description(
    attributes: &mut Vec<Attribute>,
) -> syn::Result<Option<Expr>> {
    let Some(attribute) = take_the_one_attribute_named(
        attributes,
        DESCRIPTION_ATTRIBUTE,
        "a field declares one `#[description = …]`",
    )?
    else {
        return Ok(None);
    };
    let Meta::NameValue(name_value) = &attribute.meta else {
        return Err(description_spelling_refusal(attribute.span()));
    };
    match &name_value.value {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(_),
            ..
        })
        | Expr::Path(_) => Ok(Some(name_value.value.clone())),
        other => Err(description_spelling_refusal(other.span())),
    }
}

fn description_spelling_refusal(span: Span) -> syn::Error {
    syn::Error::new(
        span,
        "`#[description = …]` takes a string literal or the path of a `const` holding one — \
         `#[description = \"…\"]` or `#[description = MY_KEY_DESCRIPTION]`",
    )
}
