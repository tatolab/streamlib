// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The keys of a value input, `input("name", value = <type>, default = …)`, which only a node
//! written as one function declares.

use syn::{Expr, Type};

use super::PortDirection;

/// Note `key` in `first_port_key` when it is the first stated that a port takes and a value
/// input does not.
pub(super) fn note_the_first_port_key(
    first_port_key: &mut Option<(String, proc_macro2::Span)>,
    key: &str,
    key_span: proc_macro2::Span,
) {
    if first_port_key.is_none() && !matches!(key, "description" | "value" | "default") {
        *first_port_key = Some((key.to_owned(), key_span));
    }
}

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

/// The type and default the value input `input("{name}", value = …, default = …)` declares:
/// both stated, and no key a port takes beside them, `first_port_key` naming the first stated.
pub(super) fn value_input_type_and_default(
    name: &str,
    name_span: proc_macro2::Span,
    value_type: Option<Type>,
    default: Option<Expr>,
    first_port_key: Option<(String, proc_macro2::Span)>,
) -> syn::Result<(Type, Expr)> {
    if let Some((key, key_span)) = first_port_key {
        return Err(syn::Error::new(
            key_span,
            format!(
                "value input `{name}` declares `{key}`, which a port takes — a value input \
                 states its `value = <type>`, its `default = …` and its `description`, and \
                 nothing else"
            ),
        ));
    }
    let Some(value_type) = value_type else {
        return Err(syn::Error::new(
            name_span,
            format!(
                "input `{name}` declares a `default` and no `value = <type>` — a value input \
                 states the type of the value it holds"
            ),
        ));
    };
    let Some(default) = default else {
        return Err(syn::Error::new(
            name_span,
            format!(
                "value input `{name}` declares no `default = …` — a value input holds its \
                 default until it is set"
            ),
        ));
    };
    Ok((value_type, default))
}
