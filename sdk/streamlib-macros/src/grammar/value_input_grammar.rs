// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The keys of a value input, `input("name", value = <type>, default = …)`, which only a node
//! written as one function declares.

use syn::Ident;

use super::{DeclaredDescription, ParsedValueInput, PortDirection};

/// Refuse `key`, which only a value input states, on `output("{name}", …)`.
pub(super) fn reject_a_value_input_key_on_an_output(
    direction: PortDirection,
    name: &str,
    key: &str,
    key_span: proc_macro2::Span,
) -> syn::Result<()> {
    if direction == PortDirection::Output {
        return Err(syn::Error::new(
            key_span,
            format!(
                "`{key}` is declared by a value input, not `output(\"{name}\", ...)` — an \
                 output writes what its node sends, and holds no value of its own"
            ),
        ));
    }
    Ok(())
}

/// The value input `input("{name}", value = …, default = …)` declares: its type and default
/// both stated, and no key a port takes beside them.
#[allow(
    clippy::too_many_arguments,
    reason = "each is one part of the declaration the value input is read from"
)]
pub(super) fn value_input_declared(
    name: String,
    name_span: proc_macro2::Span,
    declared_marker_ident: Ident,
    cast_name: String,
    description: Option<DeclaredDescription>,
    value_type: Option<(proc_macro2::Span, syn::Type)>,
    value_default: Option<(proc_macro2::Span, syn::Expr)>,
    port_keys_stated: &[(String, proc_macro2::Span)],
) -> syn::Result<ParsedValueInput> {
    if let Some((key, key_span)) = port_keys_stated.first() {
        return Err(syn::Error::new(
            *key_span,
            format!(
                "value input `{name}` declares `{key}`, which a port takes — a value input \
                 states its `value = <type>`, its `default = …` and its `description`, and \
                 nothing else"
            ),
        ));
    }
    let Some((_, value_type)) = value_type else {
        return Err(syn::Error::new(
            name_span,
            format!(
                "input `{name}` declares a `default` and no `value = <type>` — a value input \
                 states the type of the value it holds"
            ),
        ));
    };
    let Some((_, default)) = value_default else {
        return Err(syn::Error::new(
            name_span,
            format!(
                "value input `{name}` declares no `default = …` — a value input holds its \
                 default until it is set"
            ),
        ));
    };
    Ok(ParsedValueInput {
        declared_ident: declared_marker_ident,
        cast_name,
        description,
        value_type,
        default,
    })
}
