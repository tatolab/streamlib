// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Taking one attribute an author writes on a field off it, for a macro that reads it.

use syn::Attribute;
use syn::spanned::Spanned;

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
