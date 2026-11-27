// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A port whose pin type an input of its node chooses: `pin_type = from_setting("<setting>")` on
//! a node written as a struct, `pin_type = from_input(<value input>)` on one written as one
//! function.

use streamlib_node_schema::cast_exposed_name_to_url_safe;
use streamlib_node_schema::pin_type::{DeclaredPinType, PinAnnotations, PinDeclaration, PinFlow};
use syn::parse::ParseStream;
use syn::{Ident, LitStr, parenthesized};

/// The pin of a port typed by the input `input_name`, as `pin_type = from_setting("…")` or
/// `pin_type = from_input(…)` states it: a value, or of the flow it states, of the type the input
/// chooses.
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

/// The input whose chosen pin type a port carries, as the port names it.
#[derive(Clone)]
pub(crate) enum PinTypeChosenBy {
    /// A setting of a node written as a struct, as `from_setting("<setting>")` names it.
    Setting(String),
    /// A value input of a node written as one function, as `from_input(<value input>)` names
    /// it, spanned where it is named, with its name cast as every exposed name is.
    ValueInput { named: Ident, cast_name: String },
}

impl PinTypeChosenBy {
    /// The name of the input, as the node's descriptor names it.
    pub(crate) fn input_name(&self) -> &str {
        match self {
            Self::Setting(setting_name) => setting_name,
            Self::ValueInput { cast_name, .. } => cast_name,
        }
    }
}

impl std::fmt::Display for PinTypeChosenBy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setting(setting_name) => write!(formatter, "from_setting(\"{setting_name}\")"),
            Self::ValueInput { named, .. } => write!(formatter, "from_input({named})"),
        }
    }
}

/// Parse `from_setting("<setting>")` or `from_input(<value input>)` into the input it names,
/// refusing any other call with `refusal` of the function it names.
pub(super) fn parse_the_input_choosing_a_pin_type(
    content: ParseStream<'_>,
    refusal: impl FnOnce(&Ident) -> String,
) -> syn::Result<PinTypeChosenBy> {
    if !content.peek(Ident) || content.fork().parse::<Ident>()? != "from_input" {
        return parse_from_setting(content, refusal).map(PinTypeChosenBy::Setting);
    }
    content.parse::<Ident>()?;
    let arguments;
    parenthesized!(arguments in content);
    let named: Ident = arguments.parse()?;
    let cast_name = cast_exposed_name_to_url_safe(&named.to_string())
        .map_err(|casts_to_nothing| syn::Error::new(named.span(), casts_to_nothing))?
        .into_owned();
    Ok(PinTypeChosenBy::ValueInput { named, cast_name })
}

/// Parse `from_setting("<setting>")` into the setting it names, refusing any other call with
/// `refusal` of the function it names.
pub(super) fn parse_from_setting(
    content: ParseStream<'_>,
    refusal: impl FnOnce(&Ident) -> String,
) -> syn::Result<String> {
    let function: Ident = content.parse()?;
    if function != "from_setting" {
        return Err(syn::Error::new(function.span(), refusal(&function)));
    }
    let arguments;
    parenthesized!(arguments in content);
    let setting_name: LitStr = arguments.parse()?;
    Ok(setting_name.value())
}
