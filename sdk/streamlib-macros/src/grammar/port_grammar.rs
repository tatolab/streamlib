// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_node_schema::{
    ArrayOutputPortResidency, AudioWindowContract, AudioWindowContractDeclaredValues,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    DELIVERY_PROFILE_DECLARATION_VALUES, ImageOutputPortFrameRate, MAX_INPUT_PORT_BORROW_QUOTA,
    NodeValueLifetime, OutputPortDeclaredNames, PortFamily, PortFamilyDeclaration,
    PortFamilyDeclarationDirection, cast_exposed_name_to_url_safe,
    pin_type::{PinType, TexturePixelFormat},
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
use syn::ext::IdentExt;
use syn::parse::ParseStream;
use syn::{Ident, LitInt, LitStr, Token, parenthesized};
use unicode_normalization::UnicodeNormalization;

use super::{DeclaredDescription, ParsedPort, PortDirection};

/// Parse an `input(...)` / `output(...)` port body.
///
/// `<name-string>[, delivery_profile = "...", borrow_quota = <n>, description = "...",
/// audio_window(...) | audio_window = match_device, family = <Family>, pin_type = <PinType>,
/// lifetime = kept | reset, <family key> = …]`.
///
/// A port states `family = …`, `pin_type = …`, or both when they agree. A port declared by pin
/// type serves the family its messages belong to, and its family keys apply to that family.
///
/// `delivery_profile` is a consumer-side setting: **required** on every
/// `input(...)`, and rejected with a spanned error on an `output(...)` rather
/// than silently dropped. `audio_window` is likewise consumer-side and
/// likewise rejected on an output — a producer publishes what it has, and only
/// a consumer states what it needs. `lifetime` is the input's too: whether a value set live on it
/// is kept across Stop or reset there, kept when it is not stated.
pub(super) fn parse_port(
    input: ParseStream<'_>,
    direction: PortDirection,
) -> syn::Result<ParsedPort> {
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
    let mut family: Option<PortFamily> = None;
    let mut pin_type: Option<(proc_macro2::Span, PinType)> = None;
    let mut lifetime = NodeValueLifetime::Kept;
    let mut family_keys = PortFamilyDeclaration::of_family(PortFamily::Records);
    let mut first_family_key_span: Option<proc_macro2::Span> = None;

    while !content.is_empty() {
        content.parse::<Token![,]>()?;
        if content.is_empty() {
            break;
        }
        reject_positional_port_schema(&content, &name, direction)?;
        let key: Ident = content.parse()?;
        let key_span = key.span();

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
        match key.to_string().as_str() {
            "description" => {
                description = Some(DeclaredDescription::parse(&content)?);
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
                let family_ident: Ident = content.parse()?;
                family = Some(parse_port_family(&family_ident, &name)?);
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
            family_key @ ("names" | "requires" | "codec" | "requires_codec" | "rate"
            | "frame_rate" | "dtype" | "formats" | "residency") => {
                first_family_key_span.get_or_insert(key_span);
                parse_port_family_key(&content, family_key, &name, &mut family_keys)?;
            }
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown port key `{other}` — expected `delivery_profile`, \
                         `borrow_quota`, `description`, `audio_window`, `family`, `pin_type`, \
                         `lifetime`, or one of \
                         the family keys `names`, `requires`, `codec`, `requires_codec`, \
                         `rate`, `frame_rate`, `dtype`, `formats`, `residency`"
                    ),
                ));
            }
        }
    }

    if direction == PortDirection::Input && delivery_profile.is_none() {
        return Err(syn::Error::new(
            name_lit.span(),
            format!(
                "input port `{name}` must declare a `delivery_profile` — one of {}. \
                 There is no default: channel policy is declared port-locally at the \
                 consuming input port",
                render_delivery_profile_values(),
            ),
        ));
    }

    // Checked after the loop rather than at the key, so a contract declared
    // before its profile is refused the same as one declared after.
    if let Some((key_span, _)) = &audio_window {
        refuse_audio_window_beside_a_skipping_delivery_profile(delivery_profile.as_deref())
            .map_err(|refusal| syn::Error::new(*key_span, format!("port `{name}`: {refusal}")))?;
    }

    let family_its_pin_type_carries = pin_type
        .as_ref()
        .map(|(span, pin_type)| (*span, PortFamily::carrying(pin_type)));
    let family = match (family, family_its_pin_type_carries, first_family_key_span) {
        (Some(stated), Some((pin_type_span, Some(carried))), _) if stated != carried => {
            return Err(syn::Error::new(
                pin_type_span,
                format!(
                    "port `{name}` declares `family = {stated}` and a pin type whose messages \
                     are {carried} — drop `family = …`, which the pin type sets"
                ),
            ));
        }
        (Some(family), _, _) | (None, Some((_, Some(family))), _) => family,
        (None, _, Some(family_key_span)) => {
            return Err(syn::Error::new(
                family_key_span,
                format!(
                    "port `{name}` states a family key without `family = …` or \
                     `pin_type = …` — a family is one of {}",
                    rendered_port_family_names()
                ),
            ));
        }
        (None, _, None) => {
            return Err(syn::Error::new(
                name_lit.span(),
                format!(
                    "port `{name}` declares no family — add `family = …`, one of {}, or \
                     `pin_type = …`, one of {}",
                    rendered_port_family_names(),
                    rendered_rust_declared_pin_type_names()
                ),
            ));
        }
    };
    family_keys.family = family;
    let declaration_direction = match direction {
        PortDirection::Input => PortFamilyDeclarationDirection::Input,
        PortDirection::Output => PortFamilyDeclarationDirection::Output,
    };
    if let Some(refusal) = family_keys.refusal_of_its_keys(&name, declaration_direction) {
        return Err(syn::Error::new(name_lit.span(), refusal));
    }
    let pin_type = match pin_type {
        Some((pin_type_span, PinType::Texture { .. })) => Some(texture_of_the_formats_stated(
            &family_keys,
            &name,
            pin_type_span,
        )?),
        other => other.map(|(_, pin_type)| pin_type),
    };
    let family = family_keys;

    Ok(ParsedPort {
        declared_marker_ident,
        cast_name,
        description,
        delivery_profile,
        borrow_quota,
        audio_window: audio_window.map(|(_, contract)| contract),
        family,
        pin_type,
        lifetime,
    })
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

fn rendered_port_family_names() -> String {
    PortFamily::ALL
        .iter()
        .map(|family| format!("`{family}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The Texture a port declaring `pin_type = Texture` carries: of the pixel formats its
/// `formats` key states, any when it states none.
fn texture_of_the_formats_stated(
    family_keys: &PortFamilyDeclaration,
    port_name: &str,
    pin_type_span: proc_macro2::Span,
) -> syn::Result<PinType> {
    let pixel_formats = family_keys
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

/// Each pin type a Rust port declares by name alone, taking any size, format or layout: the
/// types whose family carries them today as it carries that family's messages.
const RUST_DECLARED_PIN_TYPES: [(&str, PinType); 6] = [
    ("Boolean", PinType::Boolean {}),
    ("Integer", PinType::Integer { exact_size: None }),
    ("Float", PinType::Float { exact_size: None }),
    (
        "Texture",
        PinType::Texture {
            pixel_formats: Vec::new(),
        },
    ),
    ("Audio", PinType::Audio {}),
    ("Tensor", PinType::Tensor {}),
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

/// Parse `family = <Family>`'s value.
fn parse_port_family(family_ident: &Ident, port_name: &str) -> syn::Result<PortFamily> {
    PortFamily::ALL
        .into_iter()
        .find(|family| family_ident == family.declaration_name())
        .ok_or_else(|| {
            syn::Error::new(
                family_ident.span(),
                format!(
                    "port `{port_name}` declares `family = {family_ident}` — a port's family is \
                     one of {}",
                    rendered_port_family_names()
                ),
            )
        })
}

/// Parse `["a", "b"]`, a list of string literals.
fn parse_string_literal_list(content: ParseStream<'_>, key: &str) -> syn::Result<Vec<String>> {
    let list: syn::ExprArray = content.parse().map_err(|_| {
        syn::Error::new(
            content.span(),
            format!("`{key}` takes a list of string literals, `[\"a\", \"b\"]`"),
        )
    })?;
    list.elems
        .iter()
        .map(|element| match element {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(literal),
                ..
            }) => Ok(literal.value()),
            other => Err(syn::Error::new_spanned(
                other,
                format!("`{key}` takes a list of string literals"),
            )),
        })
        .collect()
}

/// Parse `from_config("key")`, when that is what follows, into its key.
fn parse_from_config_key(content: ParseStream<'_>) -> syn::Result<Option<String>> {
    if !(content.peek(Ident) && content.peek2(syn::token::Paren)) {
        return Ok(None);
    }
    let function: Ident = content.parse()?;
    if function != "from_config" {
        return Err(syn::Error::new(
            function.span(),
            format!("expected `from_config(\"<config key>\")`, found `{function}(…)`"),
        ));
    }
    let arguments;
    parenthesized!(arguments in content);
    let config_key: LitStr = arguments.parse()?;
    Ok(Some(config_key.value()))
}

/// Parse one family key's value into `family_keys`; which family and
/// direction take it is checked once the whole port is read.
fn parse_port_family_key(
    content: ParseStream<'_>,
    family_key: &str,
    port_name: &str,
    family_keys: &mut PortFamilyDeclaration,
) -> syn::Result<()> {
    match family_key {
        "names" => {
            family_keys.names = Some(match parse_from_config_key(content)? {
                Some(config_key) => OutputPortDeclaredNames::FromConfig { config_key },
                None => {
                    OutputPortDeclaredNames::Stated(parse_string_literal_list(content, "names")?)
                }
            });
        }
        "requires" => family_keys.requires = parse_string_literal_list(content, "requires")?,
        "requires_codec" => {
            family_keys.requires_codec = parse_string_literal_list(content, "requires_codec")?
        }
        "formats" => family_keys.formats = parse_string_literal_list(content, "formats")?,
        "codec" => family_keys.codec = Some(content.parse::<LitStr>()?.value()),
        "rate" => {
            family_keys.rate = Some(match parse_from_config_key(content)? {
                Some(config_key) => ChannelsOutputPortSampleRate::FromConfig { config_key },
                None => {
                    let rate: LitInt = content.parse()?;
                    ChannelsOutputPortSampleRate::Stated(rate.base10_parse()?)
                }
            });
        }
        "frame_rate" => {
            family_keys.frame_rate = Some(match parse_from_config_key(content)? {
                Some(config_key) => ImageOutputPortFrameRate::FromConfig { config_key },
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
            family_keys.dtype = Some(match dtype.value().as_str() {
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
            family_keys.residency = Some(match residency.value().as_str() {
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
                content.span(),
                format!("port `{port_name}`: `{other}` is not a family key"),
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
