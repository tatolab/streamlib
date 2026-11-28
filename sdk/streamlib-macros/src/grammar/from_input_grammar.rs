// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `from_input(<value input>)`: the value input of its node a port's pin type, or a fact of its
//! stream, is read from, as `pin_type = from_input(…)` or `rate = from_input(…)` names it.

use streamlib_node_schema::cast_exposed_name_to_url_safe;
use streamlib_node_schema::pin_type::{DeclaredPinType, PinAnnotations, PinDeclaration, PinFlow};
use syn::parse::ParseStream;
use syn::{Ident, LitStr, parenthesized};

/// The pin of a port typed by the input `input_name`, as `pin_type = from_input(…)` states it: a
/// value, or of the flow it states, of the type the input chooses.
pub(super) fn pin_typed_by_the_input_choosing_it(
    input_name: &str,
    flow: Option<(proc_macro2::Span, PinFlow)>,
) -> PinDeclaration {
    PinDeclaration {
        pin_type: DeclaredPinType::SetByInput {
            input_name: input_name.to_owned(),
        },
        flow: flow.map_or(PinFlow::Value, |(_, flow)| flow),
        annotations: PinAnnotations::default(),
        former_names: Vec::new(),
        may_be_unset: false,
    }
}

/// The value input `from_input(<value input>)` names: spanned where it is named, with its name
/// cast as every exposed name is.
#[derive(Clone)]
pub(crate) struct ValueInputNamedByFromInput {
    pub(crate) named: Ident,
    pub(crate) cast_name: String,
}

/// A fact of a port's stream, `rate` or `frame_rate`, that `from_input(<value input>)` reads
/// from a value input of its node.
#[derive(Clone)]
pub(crate) struct StreamFactReadFromAValueInput {
    pub(crate) fact_key: &'static str,
    pub(crate) read_from: ValueInputNamedByFromInput,
}

/// Whether what follows is a call, `<function>(…)`.
pub(super) fn peeks_a_call(content: ParseStream<'_>) -> bool {
    content.peek(Ident) && content.peek2(syn::token::Paren)
}

impl std::fmt::Display for ValueInputNamedByFromInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "from_input({})", self.named)
    }
}

/// Parse `from_input(<value input>)` into the value input it names, refusing any other call with
/// `refusal` of the function it names, and the retired `from_setting("…")` with the spelling that
/// replaced it.
pub(super) fn parse_from_input(
    content: ParseStream<'_>,
    refusal: impl FnOnce(&Ident) -> String,
) -> syn::Result<ValueInputNamedByFromInput> {
    let function: Ident = content.parse()?;
    let arguments;
    parenthesized!(arguments in content);
    if function == "from_setting" {
        let named = arguments
            .parse::<LitStr>()
            .map_or_else(|_| "<value input>".to_owned(), |named| named.value());
        return Err(syn::Error::new(
            function.span(),
            format!(
                "`from_setting(\"{named}\")` is retired: a node has no settings, and a port names \
                 the value input it follows by its identifier, `from_input({named})`"
            ),
        ));
    }
    if function != "from_input" {
        return Err(syn::Error::new(function.span(), refusal(&function)));
    }
    let named: Ident = arguments.parse()?;
    let cast_name = cast_exposed_name_to_url_safe(&named.to_string())
        .map_err(|casts_to_nothing| syn::Error::new(named.span(), casts_to_nothing))?
        .into_owned();
    Ok(ValueInputNamedByFromInput { named, cast_name })
}
