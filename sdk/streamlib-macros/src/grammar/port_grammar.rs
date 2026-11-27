// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_node_schema::{
    ArrayOutputPortResidency, AudioWindowContract, AudioWindowContractDeclaredValues,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    DELIVERY_PROFILE_DECLARATION_VALUES, ImageOutputPortFrameRate, MAX_INPUT_PORT_BORROW_QUOTA,
    NodeValueLifetime, cast_exposed_name_to_url_safe,
    pin_type::{PinAnnotations, PinDeclaration, PinFlow, PinType, TexturePixelFormat},
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
use syn::ext::IdentExt;
use syn::parse::ParseStream;
use syn::{Ident, LitInt, LitStr, Token, parenthesized};
use unicode_normalization::UnicodeNormalization;

use super::pin_type_chosen_by_an_input::{
    PinTypeChosenBy, parse_from_setting, parse_the_input_choosing_a_pin_type,
    pin_typed_by_the_input_choosing_it,
};
use super::string_literal_list::{parse_string_literal_list, parse_string_literals};
use super::value_input_grammar::{
    note_the_first_port_key, reject_a_choice_on_a_port, reject_a_value_input_key_on_an_output,
    value_input_chosen_from, value_input_type_and_default,
};
use super::{DeclaredDescription, ParsedPort, ParsedValueInput, PortDirection};

/// What one `input(...)` or `output(...)` declares: a port, or a value input of a node written
/// as one function.
pub(crate) enum DeclaredPort {
    Port(ParsedPort),
    Value(ParsedValueInput),
}

/// Parse an `input(...)` / `output(...)` port body.
///
/// `<name-string>[, delivery_profile = "...", borrow_quota = <n>, description = "...",
/// audio_window(...) | audio_window = match_device, pin_type = <PinType> |
/// pin_type = from_setting("<setting>") | pin_type = from_input(<value input>), lifetime = kept | reset, flow = <PinFlow>, former_names = ["…"], may_be_unset = true,
/// takes_a_value = true, <stream fact key> = …]`, or, for a value input of a node written as
/// one function, `<name-string>, value = <Type>, default = <expression>[, description = "...",
/// options = ["…", …] | among_the_bundles_its_stream_declares = true]`.
///
/// A port states its pin type. A Texture port states the pixel formats it carries with
/// `formats = […]`, and an output states what it knows of its stream as the annotations of its
/// pin: a Texture's `frame_rate`, an Audio's `rate` and `dtype`, a Tensor's `residency`. A port
/// may state `flow = …` where its type allows another flow than its default.
///
/// `delivery_profile` is a consumer-side setting: optional on an `input(...)` with a pin, which
/// reads by its pin's default when it states none, **required** on every other `input(...)`,
/// and rejected with a spanned error on an `output(...)` rather than silently dropped.
/// `audio_window` is likewise consumer-side and likewise rejected on an output — a producer
/// publishes what it has, and only a consumer states what it needs. `lifetime` is the input's too:
/// whether a value set live on it is kept across Stop or reset there, kept when it is not stated.
/// `former_names` lists the names a renamed port had, each of which still finds it, and
/// `may_be_unset` lets an input hold no value; both are stated on a port with a pin, and
/// `may_be_unset` on an input only. `takes_a_value` lets an input take a value set on it as well
/// as a wire, on an input only.
pub(super) fn parse_port(
    input: ParseStream<'_>,
    direction: PortDirection,
) -> syn::Result<DeclaredPort> {
    let content;
    parenthesized!(content in input);

    let name_lit: LitStr = content.parse()?;
    // rustc's lexer NFC-normalizes an identifier and proc_macro2's fallback
    // does not, so the spelling is normalized first for the two to agree.
    let name: String = name_lit.value().nfc().collect();
    // `parse_str` skips whitespace and comments and lexes `r#`, none of which
    // the marker's `Ident` can carry, so only a spelling that parses back to
    // itself is one.
    let mut declared_marker_ident = match syn::parse_str::<Ident>(&name) {
        Ok(ident) if ident == name && !name.starts_with("r#") => ident,
        _ => {
            return Err(syn::Error::new(
                name_lit.span(),
                format!(
                    "port {name:?} is not a plain Rust identifier — the declared name is the \
                     port marker's type (`{}::<name>`), so it can carry no space, punctuation, \
                     keyword or `r#` prefix",
                    direction.port_marker_module_ident()
                ),
            ));
        }
    };
    declared_marker_ident.set_span(name_lit.span());
    let cast_name = cast_exposed_name_to_url_safe(&name)
        .map_err(|casts_to_nothing| syn::Error::new(name_lit.span(), casts_to_nothing))?
        .into_owned();

    let mut description = None;
    let mut delivery_profile = None;
    let mut borrow_quota = None;
    let mut audio_window: Option<(proc_macro2::Span, AudioWindowContract)> = None;
    let mut pin_type: Option<(proc_macro2::Span, PinType)> = None;
    let mut pin_type_chosen_by_an_input: Option<(proc_macro2::Span, PinTypeChosenBy)> = None;
    let mut lifetime = NodeValueLifetime::Kept;
    let mut flow: Option<(proc_macro2::Span, PinFlow)> = None;
    let mut former_names: Option<(proc_macro2::Span, Vec<String>)> = None;
    let mut may_be_unset: Option<proc_macro2::Span> = None;
    let mut takes_a_value = false;
    let mut stream_facts = StreamFactsStated::default();
    let mut value_type: Option<syn::Type> = None;
    let mut value_default: Option<syn::Expr> = None;
    let mut options: Option<(proc_macro2::Span, Vec<LitStr>)> = None;
    let mut among_the_bundles_its_stream_declares: Option<proc_macro2::Span> = None;
    // The first key but `description`, `value` and `default`, which a value input takes no
    // other of.
    let mut first_port_key: Option<(String, proc_macro2::Span)> = None;

    while !content.is_empty() {
        content.parse::<Token![,]>()?;
        if content.is_empty() {
            break;
        }
        reject_positional_port_schema(&content, &name, direction)?;
        let key: Ident = content.parse()?;
        let key_span = key.span();
        let key_name = key.to_string();
        note_the_first_port_key(&mut first_port_key, &key_name, key_span);

        // `audio_window` is the one port key with a call form —
        // `audio_window(sample_rate = …)` beside `audio_window = match_device`
        // — so its body is parsed before the `=` every other key expects.
        if key == "audio_window" {
            reject_audio_window_on_output(direction, &name, key_span)?;
            if audio_window.is_some() {
                return Err(syn::Error::new(
                    key_span,
                    format!(
                        "port `{name}` declares `audio_window` more than once — the contract \
                         is whole, so there is nothing a second one could add"
                    ),
                ));
            }
            audio_window = Some((
                key_span,
                parse_audio_window_contract(&content, &name, key_span)?,
            ));
            continue;
        }

        content.parse::<Token![=]>()?;
        match key_name.as_str() {
            "description" => {
                description = Some(DeclaredDescription::parse(&content)?);
            }
            "value" => {
                reject_a_value_input_key_on_an_output(direction, &name, "value", key_span)?;
                value_type = Some(content.parse()?);
            }
            "default" => {
                reject_a_value_input_key_on_an_output(direction, &name, "default", key_span)?;
                value_default = Some(content.parse()?);
            }
            "options" => {
                reject_a_value_input_key_on_an_output(direction, &name, "options", key_span)?;
                options = Some((key_span, parse_string_literals(&content, "options")?));
            }
            "among_the_bundles_its_stream_declares" => {
                reject_a_value_input_key_on_an_output(
                    direction,
                    &name,
                    "among_the_bundles_its_stream_declares",
                    key_span,
                )?;
                let stated: syn::LitBool = content.parse()?;
                if stated.value {
                    among_the_bundles_its_stream_declares = Some(key_span);
                }
            }
            "delivery_profile" => {
                let lit: LitStr = content.parse()?;
                reject_delivery_profile_on_output(direction, &name, key_span)?;
                delivery_profile = Some(lit.value());
            }
            "borrow_quota" => {
                let lit: syn::LitInt = content.parse()?;
                if direction == PortDirection::Output {
                    return Err(syn::Error::new(
                        key_span,
                        format!(
                            "`borrow_quota` is declared by the destination input port, not \
                             `output(\"{name}\", ...)` — each link's reader holds its own"
                        ),
                    ));
                }
                let quota: usize = lit.base10_parse()?;
                if quota > MAX_INPUT_PORT_BORROW_QUOTA {
                    return Err(syn::Error::new(
                        lit.span(),
                        format!(
                            "input port `{name}` declares `borrow_quota = {quota}`, past the \
                             most a link may hold beyond its mailbox, \
                             {MAX_INPUT_PORT_BORROW_QUOTA}"
                        ),
                    ));
                }
                borrow_quota = Some(quota);
            }
            "family" => {
                return Err(syn::Error::new(
                    key_span,
                    format!(
                        "port `{name}` declares `family = …`, which is retired: a port states \
                         `pin_type = …`, one of {}",
                        rendered_rust_declared_pin_type_names()
                    ),
                ));
            }
            "pin_type" if peeks_a_call(&content) => {
                let chosen_by = parse_the_input_choosing_a_pin_type(&content, |function| {
                    format!(
                        "port `{name}` declares `pin_type = {function}(…)` — a port typed by a \
                         value input of a node written as one function states `pin_type = \
                         from_input(<value input>)`, and one typed by a setting `pin_type = \
                         from_setting(\"<setting>\")`"
                    )
                })?;
                pin_type_chosen_by_an_input = Some((key_span, chosen_by));
            }
            "pin_type" => {
                let pin_type_ident: Ident = content.parse()?;
                pin_type = Some((
                    pin_type_ident.span(),
                    parse_pin_type(&pin_type_ident, &name)?,
                ));
            }
            "lifetime" => {
                let lifetime_ident: Ident = content.parse()?;
                lifetime = parse_lifetime(&lifetime_ident, &name, direction)?;
            }
            "flow" => {
                let flow_ident: Ident = content.parse()?;
                flow = Some((flow_ident.span(), parse_pin_flow(&flow_ident, &name)?));
            }
            "former_names" => {
                former_names = Some((
                    key_span,
                    parse_former_names(&content, &name, &cast_name, key_span)?,
                ));
            }
            "may_be_unset" => {
                let stated: syn::LitBool = content.parse()?;
                if direction == PortDirection::Output {
                    return Err(syn::Error::new(
                        key_span,
                        format!(
                            "`may_be_unset` is declared by an input, not `output(\"{name}\", \
                             ...)` — it says the input may hold no value"
                        ),
                    ));
                }
                if stated.value {
                    may_be_unset = Some(key_span);
                }
            }
            "takes_a_value" => {
                let stated: syn::LitBool = content.parse()?;
                if direction == PortDirection::Output {
                    return Err(syn::Error::new(
                        key_span,
                        format!(
                            "`takes_a_value` is declared by an input, not `output(\"{name}\", \
                             ...)` — it says the input takes a value set on it"
                        ),
                    ));
                }
                takes_a_value = stated.value;
            }
            fact_key @ ("rate" | "frame_rate" | "dtype" | "formats" | "residency") => {
                parse_stream_fact_key(&content, fact_key, &name, &mut stream_facts, key_span)?;
            }
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown port key `{other}` — expected `delivery_profile`, \
                         `borrow_quota`, `description`, `audio_window`, `pin_type`, \
                         `lifetime`, `flow`, `former_names`, `may_be_unset`, `takes_a_value`, \
                         `value`, `default`, `options`, `among_the_bundles_its_stream_declares`, \
                         or one of the stream fact keys `rate`, `frame_rate`, \
                         `dtype`, `formats`, `residency`"
                    ),
                ));
            }
        }
    }

    if value_type.is_some() || value_default.is_some() {
        let (value_type, default) = value_input_type_and_default(
            &name,
            name_lit.span(),
            value_type,
            value_default,
            first_port_key,
        )?;
        let chosen_from =
            value_input_chosen_from(&name, options, among_the_bundles_its_stream_declares)?;
        return Ok(DeclaredPort::Value(ParsedValueInput {
            declared_ident: declared_marker_ident,
            cast_name,
            description,
            value_type,
            default,
            chosen_from,
        }));
    }
    if let Some((key_span, _)) = &options {
        return Err(reject_a_choice_on_a_port(&name, "options", *key_span));
    }
    if let Some(key_span) = among_the_bundles_its_stream_declares {
        return Err(reject_a_choice_on_a_port(
            &name,
            "among_the_bundles_its_stream_declares",
            key_span,
        ));
    }

    let stated_pin_type = match (pin_type, pin_type_chosen_by_an_input) {
        (Some(_), Some((chosen_by_span, chosen_by))) => {
            return Err(syn::Error::new(
                chosen_by_span,
                format!(
                    "port `{name}` states `pin_type` twice — once a type, once `{chosen_by}`; \
                     keep one"
                ),
            ));
        }
        (Some((pin_type_span, pin_type)), None) => StatedPinType::Exactly(pin_type_span, pin_type),
        (None, Some((_, chosen_by))) => StatedPinType::ChosenBy(chosen_by),
        (None, None) => {
            return Err(syn::Error::new(
                name_lit.span(),
                format!(
                    "port `{name}` declares no pin type — add `pin_type = …`, one of {}, \
                     `pin_type = from_input(<value input>)` or `pin_type = \
                     from_setting(\"<setting>\")`",
                    rendered_rust_declared_pin_type_names()
                ),
            ));
        }
    };
    if let Some(refusal) = stream_facts.refusal_of_a_fact_the_port_does_not_take(
        &name,
        direction,
        match &stated_pin_type {
            StatedPinType::Exactly(_, pin_type) => Some(pin_type),
            StatedPinType::ChosenBy(_) => None,
        },
    ) {
        return Err(syn::Error::new(name_lit.span(), refusal));
    }
    let (pin_type, pin, pin_type_chosen_by) = match stated_pin_type {
        StatedPinType::Exactly(pin_type_span, pin_type) => {
            let pin_type = match pin_type {
                PinType::Texture { .. } => {
                    texture_of_the_formats_stated(&stream_facts, &name, direction, pin_type_span)?
                }
                other => other,
            };
            let pin = declared_pin(pin_type.clone(), flow, &name)?;
            (Some(pin_type), pin, None)
        }
        StatedPinType::ChosenBy(chosen_by) => {
            let pin = pin_typed_by_the_input_choosing_it(chosen_by.input_name(), flow);
            (None, pin, Some(chosen_by))
        }
    };
    let annotations = stream_facts.annotations;
    // A picture port that states a flow, a former name or that it may be unset states its
    // Texture with it, so what it states is emitted.
    let stated_beside_the_pin = flow.is_some() || former_names.is_some() || may_be_unset.is_some();
    let pin_type = if stated_beside_the_pin {
        pin.clone().into_resolved().map(|pin| pin.pin_type)
    } else {
        pin_type
    };

    if direction == PortDirection::Input {
        if let Some(stated) = &delivery_profile
            && let Some(refusal) = pin.refusal_of_its_stated_delivery_profile(&name, stated)
        {
            return Err(syn::Error::new(name_lit.span(), refusal));
        }
        let delivery_profile_read_by = delivery_profile
            .as_deref()
            .or_else(|| pin.default_input_delivery_profile());
        let Some(delivery_profile_read_by) = delivery_profile_read_by else {
            return Err(syn::Error::new(
                name_lit.span(),
                format!(
                    "input port `{name}` must declare a `delivery_profile` — one of {} — or a \
                     `pin_type = …` whose default it reads by",
                    render_delivery_profile_values(),
                ),
            ));
        };
        // Checked after the loop rather than at the key, so a contract declared
        // before its profile is refused the same as one declared after.
        if let Some((key_span, _)) = &audio_window {
            refuse_audio_window_beside_a_skipping_delivery_profile(Some(delivery_profile_read_by))
                .map_err(|refusal| {
                    syn::Error::new(*key_span, format!("port `{name}`: {refusal}"))
                })?;
        }
    }

    Ok(DeclaredPort::Port(ParsedPort {
        declared_marker_ident,
        cast_name,
        description,
        delivery_profile,
        borrow_quota,
        audio_window: audio_window.map(|(_, contract)| contract),
        annotations,
        pin_type,
        pin_type_chosen_by,
        lifetime,
        flow: flow.map(|(_, flow)| flow),
        former_names: former_names.map(|(_, names)| names).unwrap_or_default(),
        may_be_unset: may_be_unset.is_some(),
        takes_a_value,
    }))
}

/// Parse `former_names = ["…"]` on port `name`, whose cast name is `cast_name`: each name cast as
/// every exposed name is, refusing one that casts to the port's own name or to another of the
/// list.
fn parse_former_names(
    content: ParseStream<'_>,
    name: &str,
    cast_name: &str,
    key_span: proc_macro2::Span,
) -> syn::Result<Vec<String>> {
    let mut cast_former_names: Vec<String> = Vec::new();
    for former_name in parse_string_literal_list(content, "former_names")? {
        let cast = cast_exposed_name_to_url_safe(&former_name)
            .map_err(|casts_to_nothing| syn::Error::new(key_span, casts_to_nothing))?
            .into_owned();
        if cast == cast_name {
            return Err(syn::Error::new(
                key_span,
                format!("port `{name}` lists its own name `{former_name}` as a name it had"),
            ));
        }
        if cast_former_names.contains(&cast) {
            return Err(syn::Error::new(
                key_span,
                format!("port `{name}` lists the name `{former_name}` it had twice"),
            ));
        }
        cast_former_names.push(cast);
    }
    Ok(cast_former_names)
}

/// The lifetime `lifetime_ident` names on port `name`, which only an input declares.
fn parse_lifetime(
    lifetime_ident: &Ident,
    name: &str,
    direction: PortDirection,
) -> syn::Result<NodeValueLifetime> {
    if direction == PortDirection::Output {
        return Err(syn::Error::new(
            lifetime_ident.span(),
            format!(
                "`lifetime` is declared by an input, not `output(\"{name}\", ...)` — it says \
                 how long a value set live on the input lives"
            ),
        ));
    }
    NodeValueLifetime::named(&lifetime_ident.to_string()).ok_or_else(|| {
        syn::Error::new(
            lifetime_ident.span(),
            format!(
                "port `{name}` declares `lifetime = {lifetime_ident}` — a lifetime is one of {}",
                NodeValueLifetime::EVERY_LIFETIME
                    .iter()
                    .map(|lifetime| format!("`{}`", lifetime.wire_name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })
}

/// The pin a port declares: its stated pin type, in the flow it states, else that type's
/// default; a flow the type does not allow is refused at the flow.
fn declared_pin(
    pin_type: PinType,
    flow: Option<(proc_macro2::Span, PinFlow)>,
    port_name: &str,
) -> syn::Result<PinDeclaration> {
    let mut pin = PinDeclaration::exactly(pin_type);
    let Some((flow_span, flow)) = flow else {
        return Ok(pin);
    };
    pin.flow = flow;
    match pin.refusal_of_its_flow(port_name) {
        Some(refusal) => Err(syn::Error::new(flow_span, refusal)),
        None => Ok(pin),
    }
}

/// How a port states its pin type: exactly, at its span, or by the input it names.
enum StatedPinType {
    Exactly(proc_macro2::Span, PinType),
    ChosenBy(PinTypeChosenBy),
}

/// Whether what follows is a call, `<function>(…)`.
fn peeks_a_call(content: ParseStream<'_>) -> bool {
    content.peek(Ident) && content.peek2(syn::token::Paren)
}

/// Parse `flow = <PinFlow>`'s value: `Value`, `Stream` or `Trigger`.
fn parse_pin_flow(flow_ident: &Ident, port_name: &str) -> syn::Result<PinFlow> {
    PinFlow::EVERY_FLOW
        .into_iter()
        .find(|flow| flow_ident == flow.declaration_name())
        .ok_or_else(|| {
            let flow_names = PinFlow::EVERY_FLOW
                .iter()
                .map(|flow| format!("`{}`", flow.declaration_name()))
                .collect::<Vec<_>>()
                .join(", ");
            syn::Error::new(
                flow_ident.span(),
                format!(
                    "port `{port_name}` declares `flow = {flow_ident}` — a flow is one of \
                     {flow_names}"
                ),
            )
        })
}

/// The Texture a port declaring `pin_type = Texture` carries: of the pixel formats its
/// `formats` key states, any when an input states none. An output states the formats it
/// writes, so a link to it is judged by them.
fn texture_of_the_formats_stated(
    stream_facts: &StreamFactsStated,
    port_name: &str,
    direction: PortDirection,
    pin_type_span: proc_macro2::Span,
) -> syn::Result<PinType> {
    if direction == PortDirection::Output && stream_facts.formats.is_empty() {
        return Err(syn::Error::new(
            pin_type_span,
            format!(
                "Texture output `{port_name}` states no pixel format — add `formats = [\"…\"]`, \
                 the formats it writes, so a link to it is judged by them"
            ),
        ));
    }
    let pixel_formats = stream_facts
        .formats
        .iter()
        .map(|format| {
            TexturePixelFormat::from_wire_name(format).ok_or_else(|| {
                syn::Error::new(
                    pin_type_span,
                    format!("port `{port_name}` states format {format:?}, which no texture has"),
                )
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(PinType::Texture { pixel_formats })
}

/// Each pin type a Rust port declares by name alone, taking any size, format or layout.
const RUST_DECLARED_PIN_TYPES: [(&str, PinType); 8] = [
    ("Boolean", PinType::Boolean {}),
    ("Integer", PinType::Integer { exact_size: None }),
    ("Float", PinType::Float { exact_size: None }),
    ("String", PinType::String {}),
    (
        "Texture",
        PinType::Texture {
            pixel_formats: Vec::new(),
        },
    ),
    ("Audio", PinType::Audio {}),
    ("Tensor", PinType::Tensor {}),
    ("Trigger", PinType::Trigger {}),
];

fn rendered_rust_declared_pin_type_names() -> String {
    RUST_DECLARED_PIN_TYPES
        .iter()
        .map(|(name, _)| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse `pin_type = <PinType>`'s value.
fn parse_pin_type(pin_type_ident: &Ident, port_name: &str) -> syn::Result<PinType> {
    RUST_DECLARED_PIN_TYPES
        .iter()
        .find(|(name, _)| pin_type_ident == name)
        .map(|(_, pin_type)| pin_type.clone())
        .ok_or_else(|| {
            syn::Error::new(
                pin_type_ident.span(),
                format!(
                    "port `{port_name}` declares `pin_type = {pin_type_ident}` — a Rust port's \
                     pin type is one of {}",
                    rendered_rust_declared_pin_type_names()
                ),
            )
        })
}

/// Parse `from_setting("name")`, when that is what follows, into the setting the fact is read
/// from.
fn parse_the_setting_a_fact_is_read_from(content: ParseStream<'_>) -> syn::Result<Option<String>> {
    if !peeks_a_call(content) {
        return Ok(None);
    }
    parse_from_setting(content, |function| {
        format!(
            "expected `from_setting(\"<setting>\")`, found `{function}(…)` — a Rust node holds \
             no config, so a fact read at setup is read from one of its settings"
        )
    })
    .map(Some)
}

/// The stream facts a port states beside its pin type: the pixel formats of a Texture, and the
/// annotations of an output's pin.
#[derive(Default)]
struct StreamFactsStated {
    formats: Vec<String>,
    annotations: PinAnnotations,
}

impl StreamFactsStated {
    /// The first fact stated that a port of `pin_type`, at `direction`, does not take, as a
    /// refusal naming the port and the key; a port typed by a setting takes none.
    fn refusal_of_a_fact_the_port_does_not_take(
        &self,
        port_name: &str,
        direction: PortDirection,
        pin_type: Option<&PinType>,
    ) -> Option<String> {
        let is_an_output = direction == PortDirection::Output;
        let is_a = |wanted: fn(&PinType) -> bool| pin_type.is_some_and(wanted);
        let is_a_texture = is_a(|pin_type| matches!(pin_type, PinType::Texture { .. }));
        let is_audio = is_a(|pin_type| matches!(pin_type, PinType::Audio {}));
        let is_a_tensor = is_a(|pin_type| matches!(pin_type, PinType::Tensor {}));
        let stated_keys: [(&str, bool, bool); 5] = [
            ("formats", !self.formats.is_empty(), is_a_texture),
            (
                "frame_rate",
                self.annotations.frame_rate.is_some(),
                is_an_output && is_a_texture,
            ),
            (
                "rate",
                self.annotations.sample_rate.is_some(),
                is_an_output && is_audio,
            ),
            (
                "dtype",
                self.annotations.sample_type.is_some(),
                is_an_output && is_audio,
            ),
            (
                "residency",
                self.annotations.residency.is_some(),
                is_an_output && is_a_tensor,
            ),
        ];
        let direction_word = match direction {
            PortDirection::Input => "input",
            PortDirection::Output => "output",
        };
        let pin_type_words = pin_type.map_or_else(
            || "a type a setting chooses".to_owned(),
            |pin_type| format!("{pin_type}"),
        );
        stated_keys
            .iter()
            .find(|(_, stated, taken)| *stated && !*taken)
            .map(|(key, _, _)| {
                format!(
                    "{direction_word} port {port_name:?} states `{key}=`, which an \
                     {direction_word} of {pin_type_words} does not take"
                )
            })
    }
}

/// Parse one stream fact key's value into `stream_facts`; which pin type and direction take it
/// is checked once the whole port is read.
fn parse_stream_fact_key(
    content: ParseStream<'_>,
    fact_key: &str,
    port_name: &str,
    stream_facts: &mut StreamFactsStated,
    key_span: proc_macro2::Span,
) -> syn::Result<()> {
    let annotations = &mut stream_facts.annotations;
    match fact_key {
        "formats" => stream_facts.formats = parse_string_literal_list(content, "formats")?,
        "rate" => {
            annotations.sample_rate = Some(match parse_the_setting_a_fact_is_read_from(content)? {
                Some(setting_name) => ChannelsOutputPortSampleRate::FromSetting {
                    setting: setting_name,
                },
                None => {
                    let rate: LitInt = content.parse()?;
                    ChannelsOutputPortSampleRate::Stated(rate.base10_parse()?)
                }
            });
        }
        "frame_rate" => {
            annotations.frame_rate = Some(match parse_the_setting_a_fact_is_read_from(content)? {
                Some(setting_name) => ImageOutputPortFrameRate::FromSetting {
                    setting: setting_name,
                },
                None => {
                    let frames_per_second: LitInt = content.parse()?;
                    ImageOutputPortFrameRate::Stated(streamlib_node_schema::ImageStreamFrameRate {
                        numerator: frames_per_second.base10_parse()?,
                        denominator: 1,
                    })
                }
            });
        }
        "dtype" => {
            let dtype: LitStr = content.parse()?;
            annotations.sample_type = Some(match dtype.value().as_str() {
                "f32" => ChannelsOutputPortSampleType::F32,
                "f64" => ChannelsOutputPortSampleType::F64,
                other => {
                    return Err(syn::Error::new(
                        dtype.span(),
                        format!("port `{port_name}`: `dtype` is \"f32\" or \"f64\", not {other:?}"),
                    ));
                }
            });
        }
        "residency" => {
            let residency: LitStr = content.parse()?;
            annotations.residency = Some(match residency.value().as_str() {
                "gpu" => ArrayOutputPortResidency::Gpu,
                "cpu" => ArrayOutputPortResidency::Cpu,
                other => {
                    return Err(syn::Error::new(
                        residency.span(),
                        format!(
                            "port `{port_name}`: `residency` is \"gpu\" or \"cpu\", not {other:?}"
                        ),
                    ));
                }
            });
        }
        other => {
            return Err(syn::Error::new(
                key_span,
                format!("port `{port_name}`: `{other}` is not a stream fact key"),
            ));
        }
    }
    Ok(())
}

/// Parse the `audio_window` body — the call form carrying the values, or
/// `= match_device`, the whole-contract sentinel.
///
/// The values are validated here rather than at the read seam so a contract
/// the stage could not honour never compiles. `channels` may be omitted and
/// then means the source's own count, whatever it is.
fn parse_audio_window_contract(
    content: ParseStream<'_>,
    port_name: &str,
    key_span: proc_macro2::Span,
) -> syn::Result<AudioWindowContract> {
    if content.peek(Token![=]) {
        content.parse::<Token![=]>()?;
        let sentinel: Ident = content.parse()?;
        if sentinel != "match_device" {
            return Err(syn::Error::new(
                sentinel.span(),
                format!(
                    "unknown `audio_window` value `{sentinel}` on port `{port_name}` — the \
                     only sentinel is `match_device`, which resolves the contract at \
                     `setup()` from the device stream the node opened. To state the \
                     values instead, spell the contract as a call: \
                     `audio_window(sample_rate = 16_000, dtype = \"f32\", \
                     window_size = 512, hop = 512)`"
                ),
            ));
        }
        return Ok(AudioWindowContract::MatchDevice {});
    }

    if !content.peek(syn::token::Paren) {
        return Err(syn::Error::new(
            key_span,
            format!(
                "port `{port_name}` declares `audio_window` with no contract — spell it as \
                 `audio_window(sample_rate = …, dtype = …, window_size = …)` \
                 or as `audio_window = match_device`"
            ),
        ));
    }

    let body;
    parenthesized!(body in content);

    let mut sample_rate = None;
    let mut channels = None;
    let mut dtype = None;
    let mut window_size = None;
    let mut hop = None;

    while !body.is_empty() {
        let field: Ident = body.parse()?;
        body.parse::<Token![=]>()?;
        match field.to_string().as_str() {
            "sample_rate" => sample_rate = Some(parse_audio_window_numeric_field(&body)?),
            "channels" => channels = Some(parse_audio_window_numeric_field(&body)?),
            "window_size" => window_size = Some(parse_audio_window_numeric_field(&body)?),
            "hop" => hop = Some(parse_audio_window_numeric_field(&body)?),
            "dtype" => {
                let lit: LitStr = body.parse()?;
                dtype = Some(lit.value());
            }
            other => {
                return Err(syn::Error::new(
                    field.span(),
                    format!(
                        "unknown `audio_window` field `{other}` on port `{port_name}` — \
                         expected `sample_rate`, `channels`, `dtype`, `window_size`, or \
                         `hop`"
                    ),
                ));
            }
        }
        if body.is_empty() {
            break;
        }
        body.parse::<Token![,]>()?;
    }

    let absent_required_fields = [
        ("sample_rate", sample_rate.is_none()),
        ("dtype", dtype.is_none()),
        ("window_size", window_size.is_none()),
    ];
    let (Some(sample_rate), Some(dtype), Some(window_size)) = (sample_rate, dtype, window_size)
    else {
        let missing = absent_required_fields
            .into_iter()
            .filter_map(|(field, absent)| absent.then_some(format!("`{field}`")))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(syn::Error::new(
            key_span,
            format!(
                "port `{port_name}` declares a partial `audio_window` — missing {missing}. \
                 The contract is all-or-nothing but for two fields, because a half-declared \
                 one leaves the engine guessing at exactly the values a model asserts on. \
                 `hop` defaults to `window_size`, and `channels` defaults to the source's \
                 own count"
            ),
        ));
    };

    let values = AudioWindowContractDeclaredValues {
        sample_rate,
        channels,
        dtype,
        window_size,
        hop: hop.unwrap_or(window_size),
    };
    values
        .refuse_if_unhonourable()
        .map_err(|refusal| syn::Error::new(key_span, format!("port `{port_name}`: {refusal}")))?;

    Ok(AudioWindowContract::Declaration(values))
}

/// Parse one `audio_window` numeric field, refusing a negative literal by name.
///
/// The fields are unsigned, so a `-1` would otherwise reach the author as
/// syn's bare `expected integer literal` with nothing to act on.
fn parse_audio_window_numeric_field(body: ParseStream<'_>) -> syn::Result<u32> {
    if body.peek(Token![-]) {
        let minus = body.parse::<Token![-]>()?;
        let lit: LitInt = body.parse()?;
        return Err(syn::Error::new(
            minus.span,
            format!(
                "`audio_window` takes no negative value — got `-{}`. Every numeric field is \
                 strictly positive",
                lit.base10_digits()
            ),
        ));
    }
    body.parse::<LitInt>()?.base10_parse()
}

/// Reject `audio_window` on an `output(...)` with a spanned error — an output
/// port declares no contract. A no-op on an `input(...)`.
fn reject_audio_window_on_output(
    direction: PortDirection,
    port_name: &str,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    if direction == PortDirection::Output {
        return Err(syn::Error::new(
            span,
            format!(
                "`audio_window` is a consumer-side setting and is not valid on \
                 `output(\"{port_name}\", ...)` — a producer publishes what it has, and \
                 only a consuming input port states the window it needs"
            ),
        ));
    }
    Ok(())
}

/// The legal `delivery_profile` values as a quoted, comma-joined list.
fn render_delivery_profile_values() -> String {
    render_declaration_values(&DELIVERY_PROFILE_DECLARATION_VALUES)
}

/// Reject the pre-#1816 positional port schema — `"@org/package/Type"` or the
/// bare `any` — where a key is expected, naming the removal.
///
/// Without this the author sees syn's bare `expected identifier` / `expected
/// =` and has nothing to act on.
fn reject_positional_port_schema(
    content: ParseStream<'_>,
    port_name: &str,
    direction: PortDirection,
) -> syn::Result<()> {
    let span = if content.peek(LitStr) {
        content.fork().parse::<LitStr>()?.span()
    } else if content.peek(Ident::peek_any) && content.fork().parse::<Ident>()? == "any" {
        content.fork().parse::<Ident>()?.span()
    } else {
        return Ok(());
    };
    Err(syn::Error::new(
        span,
        format!(
            "a port declares no type — remove this argument from \
             `{}(\"{port_name}\", ...)`. A port declares a name plus \
             `delivery_profile` and `description`; type information belongs to \
             the authoring language and never reaches the engine",
            direction.keyword()
        ),
    ))
}

/// Reject `delivery_profile` on an `output(...)` with a spanned error — the
/// profile is a consumer-side setting the destination input port declares.
/// A no-op on an `input(...)`.
fn reject_delivery_profile_on_output(
    direction: PortDirection,
    port_name: &str,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    if direction == PortDirection::Output {
        return Err(syn::Error::new(
            span,
            format!(
                "`delivery_profile` is a consumer-side setting and is not valid on \
                 `{}(\"{port_name}\", ...)` — it is declared by the destination \
                 input port, not the producing output port",
                direction.keyword()
            ),
        ));
    }
    Ok(())
}
