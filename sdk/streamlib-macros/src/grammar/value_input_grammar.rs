// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The keys of a value input, `input("name", value = <type>, default = …)`, which only a node
//! written as one function declares.

use syn::{Expr, LitStr, Type};

use super::PortDirection;

/// What a value input's value is chosen from.
pub(crate) enum ValueInputChosenFrom {
    /// Any value of its type.
    AnyValueOfItsType,
    /// One of the names `options = ["…", …]` lists, for a String.
    Options(Vec<LitStr>),
    /// One of the bundles its stream's node types declare, as
    /// `among_the_bundles_its_stream_declares = true` states, for an `Option<PinType>`.
    TheBundlesItsStreamDeclares,
}

/// The keys only a value input states.
const VALUE_INPUT_KEYS: [&str; 5] = [
    "description",
    "value",
    "default",
    "options",
    "among_the_bundles_its_stream_declares",
];

/// Note `key` in `first_port_key` when it is the first stated that a port takes and a value
/// input does not.
pub(super) fn note_the_first_port_key(
    first_port_key: &mut Option<(String, proc_macro2::Span)>,
    key: &str,
    key_span: proc_macro2::Span,
) {
    if first_port_key.is_none() && !VALUE_INPUT_KEYS.contains(&key) {
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
    let Some(value_type) = value_type else {
        return Err(syn::Error::new(
            name_span,
            format!(
                "input `{name}` declares a `default` and no `value = <type>` — a port takes no \
                 `default`, and a value input states the type of the value it holds"
            ),
        ));
    };
    if let Some((key, key_span)) = first_port_key {
        return Err(syn::Error::new(
            key_span,
            format!(
                "value input `{name}` declares `{key}`, which a port takes — a value input \
                 states its `value = <type>`, its `default = …`, its `description`, and what \
                 its value is chosen from, `options = [\"…\", …]` or \
                 `among_the_bundles_its_stream_declares = true`, and nothing else"
            ),
        ));
    }
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

/// What the value input `{name}` is chosen from, as `options` and
/// `among_the_bundles_its_stream_declares` state it; refused when it states both, or a list of no
/// options.
pub(super) fn value_input_chosen_from(
    name: &str,
    options: Option<(proc_macro2::Span, Vec<LitStr>)>,
    among_the_bundles_its_stream_declares: Option<proc_macro2::Span>,
) -> syn::Result<ValueInputChosenFrom> {
    match (options, among_the_bundles_its_stream_declares) {
        (Some(_), Some(marker)) => Err(syn::Error::new(
            marker,
            format!(
                "value input `{name}` is chosen from `options = […]` or among the bundles its \
                 stream declares, not both"
            ),
        )),
        (Some((span, options)), None) if options.is_empty() => Err(syn::Error::new(
            span,
            format!(
                "`options` lists the names value input `{name}` is chosen from, and lists none"
            ),
        )),
        (Some((_, options)), None) => Ok(ValueInputChosenFrom::Options(options)),
        (None, Some(_)) => Ok(ValueInputChosenFrom::TheBundlesItsStreamDeclares),
        (None, None) => Ok(ValueInputChosenFrom::AnyValueOfItsType),
    }
}

/// Refuse `key`, which only a value input states, on the port `{name}`, which states no
/// `value = <type>`.
pub(super) fn reject_a_choice_on_a_port(
    name: &str,
    key: &str,
    key_span: proc_macro2::Span,
) -> syn::Error {
    syn::Error::new(
        key_span,
        format!(
            "port `{name}` declares `{key}`, which a value input states — a port carries what \
             its wire sends; declare `value = <type>` and `default = …` for an input holding a \
             value chosen from a list"
        ),
    )
}
