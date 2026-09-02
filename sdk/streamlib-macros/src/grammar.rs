// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grammar for the `#[processor(...)]` attribute — the single source of truth
//! for a processor's execution mode and ports.
//!
//! Nothing here reads any file. Everything the macro needs to emit the
//! processor module is declared in the attribute tokens:
//!
//! ```ignore
//! #[processor(
//!     execution = manual,               // reactive | manual | continuous | continuous(interval_ms = 10)
//!     scheduling = high,                // realtime | high | normal (default: normal)
//!     unsafe_send,                      // flag — emit `unsafe impl Send`
//!     config = crate::CameraConfig,     // Rust type path for the typed Config alias
//!     input("video_in", delivery_profile = "newest"),
//!     output("video"),
//! )]
//! ```
//!
//! A port declares a name, a description, and — on an input — a delivery
//! profile, plus an optional `audio_window` contract on an audio input. It
//! carries no type: type information belongs to the authoring language and
//! never reaches the engine.
//!
//! The attribute declares no identity. A processor is named by the import path
//! of its type, captured by the macro at the expansion site.

use streamlib_processor_schema::{
    AudioWindowContract, AudioWindowContractDeclaredValues, DELIVERY_PROFILE_DECLARATION_VALUES,
    ProcessorPortSchema, ProcessorScheduling, ProcessorSchema, ProcessorSchemaExecution,
    RuntimeConfig, RuntimeOptions, ThreadPriority,
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
use syn::ext::IdentExt;
use syn::parse::{ParseStream, Parser};
use syn::{Ident, LitInt, LitStr, Path, Token, parenthesized};

/// Which side of a link a port sits on. `delivery_profile` is a consumer-side
/// setting only valid on an `input(...)`; the grammar rejects it on an
/// `output(...)`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PortDirection {
    Input,
    Output,
}

impl PortDirection {
    fn keyword(self) -> &'static str {
        match self {
            PortDirection::Input => "input",
            PortDirection::Output => "output",
        }
    }
}

/// A parsed input/output port declaration.
pub struct ParsedPort {
    pub name: String,
    pub description: Option<String>,
    /// Always `Some` on an input and always `None` on an output — the grammar
    /// requires it on the one and rejects it on the other.
    pub delivery_profile: Option<String>,
    /// The `audio_window` contract, on the audio inputs that declare one.
    /// `None` everywhere else — this is opt-in, and a port without one is
    /// unchanged in every respect.
    pub audio_window: Option<AudioWindowContract>,
}

/// The fully-parsed `#[processor(...)]` attribute.
pub struct ParsedProcessorAttr {
    /// The author's type name — what an instance's display name defaults to.
    pub processor_class_short_name: String,
    pub description: Option<String>,
    pub execution: ProcessorSchemaExecution,
    pub scheduling: Option<ThreadPriority>,
    pub unsafe_send: bool,
    pub config_type: Option<Path>,
    pub config_field_name: String,
    pub config_schema_id: Option<String>,
    pub inputs: Vec<ParsedPort>,
    pub outputs: Vec<ParsedPort>,
}

impl ParsedProcessorAttr {
    /// Project the parsed attribute into the manifest-shaped [`ProcessorSchema`].
    ///
    /// This is the single projection both readers of the attribute share: the
    /// proc-macro emits its descriptor from this, and the source-scan extractor
    /// builds each manifest entry from it — so an added `ParsedProcessorAttr` or
    /// `ProcessorSchema` field can never silently diverge the two. `name` is the
    /// author's type name, and the runtime language defaults to Rust (the
    /// only language a source scan of a Rust crate can produce). `config` stays
    /// `None`: the attribute binds a config *type*, not a resolved manifest
    /// schema; the consuming layer projects the config-schema id to a
    /// release-core catalog entry.
    pub fn to_processor_schema(&self) -> ProcessorSchema {
        let to_port = |p: &ParsedPort| ProcessorPortSchema {
            name: p.name.clone(),
            description: p.description.clone(),
            delivery_profile: p.delivery_profile.clone(),
            audio_window: p.audio_window.clone(),
        };

        ProcessorSchema {
            name: self.processor_class_short_name.clone(),
            description: self.description.clone(),
            runtime: RuntimeConfig {
                language: Default::default(),
                options: RuntimeOptions {
                    unsafe_send: self.unsafe_send,
                    python_version: None,
                },
                env: Default::default(),
            },
            entrypoint: None,
            execution: self.execution.clone(),
            scheduling: self
                .scheduling
                .map(|priority| ProcessorScheduling { priority }),
            config: None,
            state: Vec::new(),
            inputs: self.inputs.iter().map(to_port).collect(),
            outputs: self.outputs.iter().map(to_port).collect(),
        }
    }
}

/// Parse the `#[processor(...)]` attribute tokens into a [`ParsedProcessorAttr`].
///
/// This is the single, shared grammar entrypoint: the proc-macro calls it with
/// the attribute tokens it receives at expansion (converting its
/// `proc_macro::TokenStream` via `.into()`), and the source-scan
/// The attribute macro calls it with the tokens a `syn`-parsed
/// `#[processor(...)]` attribute carries. There is deliberately no second
/// parser — code is the
/// source of truth, so both readers of that truth share one grammar.
///
/// `struct_ident` supplies the class short name the display-name default reads.
pub fn parse2(
    attr: proc_macro2::TokenStream,
    struct_ident: &Ident,
) -> syn::Result<ParsedProcessorAttr> {
    let struct_name = struct_ident.to_string();
    let parser = move |input: ParseStream<'_>| parse_body(input, &struct_name);
    parser.parse2(attr)
}

fn parse_body(input: ParseStream<'_>, struct_name: &str) -> syn::Result<ParsedProcessorAttr> {
    let mut description: Option<String> = None;
    let mut execution: Option<ProcessorSchemaExecution> = None;
    let mut scheduling: Option<ThreadPriority> = None;
    let mut unsafe_send = false;
    let mut config_type: Option<Path> = None;
    let mut config_field_name: Option<String> = None;
    let mut config_schema_id: Option<String> = None;
    let mut inputs: Vec<ParsedPort> = Vec::new();
    let mut outputs: Vec<ParsedPort> = Vec::new();

    reject_positional_identity(input)?;

    while !input.is_empty() {
        // `parse_any` so keyword-like keys (`type`) are accepted as raw idents.
        let key = Ident::parse_any(input)?;
        match key.to_string().as_str() {
            "unsafe_send" => unsafe_send = true,
            "description" => {
                input.parse::<Token![=]>()?;
                let lit: LitStr = input.parse()?;
                description = Some(lit.value());
            }
            "execution" => {
                input.parse::<Token![=]>()?;
                execution = Some(parse_execution(input)?);
            }
            "scheduling" => {
                input.parse::<Token![=]>()?;
                let mode: Ident = input.parse()?;
                scheduling = Some(match mode.to_string().as_str() {
                    "realtime" => ThreadPriority::RealTime,
                    "high" => ThreadPriority::High,
                    "normal" => ThreadPriority::Normal,
                    other => {
                        return Err(syn::Error::new(
                            mode.span(),
                            format!(
                                "unknown scheduling priority `{other}` — \
                                 expected `realtime`, `high`, or `normal`"
                            ),
                        ));
                    }
                });
            }
            "config" => {
                input.parse::<Token![=]>()?;
                config_type = Some(input.parse()?);
            }
            "config_field" => {
                input.parse::<Token![=]>()?;
                let lit: LitStr = input.parse()?;
                config_field_name = Some(lit.value());
            }
            "config_schema" => {
                input.parse::<Token![=]>()?;
                // Descriptor metadata only — accepts both the new-shape
                // `@org/pkg/Type@version` and legacy reverse-DNS
                // `<segments>.config@<version>` id grammars verbatim.
                let lit: LitStr = input.parse()?;
                config_schema_id = Some(lit.value());
            }
            "type" => {
                return Err(syn::Error::new(key.span(), class_path_rule()));
            }
            "input" => inputs.push(parse_port(input, PortDirection::Input)?),
            "output" => outputs.push(parse_port(input, PortDirection::Output)?),
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown `#[processor(...)]` key `{other}` — expected one of {}",
                        rendered_attribute_keys()
                    ),
                ));
            }
        }

        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
    }

    // Duplicate-port-name guard.
    check_duplicate_ports(&inputs, "input", input.span())?;
    check_duplicate_ports(&outputs, "output", input.span())?;

    let execution = execution.ok_or_else(|| {
        syn::Error::new(
            input.span(),
            "missing required `execution` — declare `execution = reactive`, \
             `execution = manual`, or `execution = continuous(interval_ms = N)`",
        )
    })?;

    // Name the config type when the author didn't spell an id out. Descriptor
    // metadata only — nothing resolves it.
    if config_schema_id.is_none()
        && let Some(path) = &config_type
        && let Some(last) = path.segments.last()
    {
        config_schema_id = Some(last.ident.to_string());
    }

    let config_field_name = config_field_name.unwrap_or_else(|| "config".to_string());

    Ok(ParsedProcessorAttr {
        processor_class_short_name: struct_name.to_string(),
        description,
        execution,
        scheduling,
        unsafe_send,
        config_type,
        config_field_name,
        config_schema_id,
        inputs,
        outputs,
    })
}

fn check_duplicate_ports(
    ports: &[ParsedPort],
    kind: &str,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for port in ports {
        if !seen.insert(port.name.as_str()) {
            return Err(syn::Error::new(
                span,
                format!("duplicate {kind} port name `{}`", port.name),
            ));
        }
    }
    Ok(())
}

/// Parse an `execution = ...` right-hand side.
fn parse_execution(input: ParseStream<'_>) -> syn::Result<ProcessorSchemaExecution> {
    let mode: Ident = input.parse()?;
    match mode.to_string().as_str() {
        "reactive" => Ok(ProcessorSchemaExecution::Reactive),
        "manual" => Ok(ProcessorSchemaExecution::Manual),
        "continuous" => {
            let mut interval_ms = 0u32;
            if input.peek(syn::token::Paren) {
                let content;
                parenthesized!(content in input);
                if !content.is_empty() {
                    let key: Ident = content.parse()?;
                    if key != "interval_ms" {
                        return Err(syn::Error::new(
                            key.span(),
                            format!(
                                "unknown `continuous(...)` key `{key}` — expected `interval_ms`"
                            ),
                        ));
                    }
                    content.parse::<Token![=]>()?;
                    let lit: LitInt = content.parse()?;
                    interval_ms = lit.base10_parse()?;
                }
            }
            Ok(ProcessorSchemaExecution::Continuous { interval_ms })
        }
        other => Err(syn::Error::new(
            mode.span(),
            format!(
                "unknown execution mode `{other}` — expected `reactive`, `manual`, or `continuous`"
            ),
        )),
    }
}

/// Parse an `input(...)` / `output(...)` port body.
///
/// `<name-string>[, delivery_profile = "...", description = "...",
/// audio_window(...) | audio_window = match_device]`.
///
/// `delivery_profile` is a consumer-side setting: **required** on every
/// `input(...)`, and rejected with a spanned error on an `output(...)` rather
/// than silently dropped. `audio_window` is likewise consumer-side and
/// likewise rejected on an output — a producer publishes what it has, and only
/// a consumer states what it needs.
fn parse_port(input: ParseStream<'_>, direction: PortDirection) -> syn::Result<ParsedPort> {
    let content;
    parenthesized!(content in input);

    let name_lit: LitStr = content.parse()?;
    let name = name_lit.value();
    if name.is_empty() {
        return Err(syn::Error::new(
            name_lit.span(),
            "port name must not be empty",
        ));
    }

    let mut description = None;
    let mut delivery_profile = None;
    let mut audio_window: Option<(proc_macro2::Span, AudioWindowContract)> = None;

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
                let lit: LitStr = content.parse()?;
                description = Some(lit.value());
            }
            "delivery_profile" => {
                let lit: LitStr = content.parse()?;
                reject_delivery_profile_on_output(direction, &name, key_span)?;
                delivery_profile = Some(lit.value());
            }
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown port key `{other}` — expected `delivery_profile`, \
                         `description`, or `audio_window`"
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

    Ok(ParsedPort {
        name,
        description,
        delivery_profile,
        audio_window: audio_window.map(|(_, contract)| contract),
    })
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
                     `setup()` from the device stream the processor opened. To state the \
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

/// Every key the attribute takes — the one list both error messages render, so
/// adding or retiring a key cannot leave one of them lying.
const PROCESSOR_ATTRIBUTE_KEYS: &[&str] = &[
    "execution",
    "scheduling",
    "unsafe_send",
    "config",
    "config_field",
    "config_schema",
    "description",
    "input",
    "output",
];

/// [`PROCESSOR_ATTRIBUTE_KEYS`] as a quoted, comma-joined list.
fn rendered_attribute_keys() -> String {
    PROCESSOR_ATTRIBUTE_KEYS
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The refusal every authored-identity spelling lands on.
fn class_path_rule() -> String {
    format!(
        "`#[processor(...)]` declares no identity. A processor is named by the import path of \
         the type it is — `my_app::filters::BlurProcessor` — captured by the macro at the \
         expansion site and never authored. Remove it; {} are the keys the attribute takes.",
        rendered_attribute_keys()
    )
}

/// Reject the leading positional `"@org/package/Type"` the attribute used to
/// take, naming the class-path rule.
///
/// Without this the author sees syn's bare `expected identifier` and has
/// nothing to act on.
fn reject_positional_identity(input: ParseStream<'_>) -> syn::Result<()> {
    if input.peek(LitStr) {
        return Err(syn::Error::new(
            input.fork().parse::<LitStr>()?.span(),
            class_path_rule(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;
    use streamlib_processor_schema::ProcessorSchemaExecution;

    fn ident(name: &str) -> Ident {
        Ident::new(name, proc_macro2::Span::call_site())
    }

    fn parse_ok(tokens: proc_macro2::TokenStream) -> ParsedProcessorAttr {
        parse2(tokens, &ident("MyProcessor")).expect("attribute should parse")
    }

    fn parse_err(tokens: proc_macro2::TokenStream) -> String {
        match parse2(tokens, &ident("MyProcessor")) {
            Ok(_) => panic!("attribute should fail to parse"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn full_execution_and_ports() {
        let parsed = parse_ok(quote! {
            execution = manual,
            scheduling = high,
            input("video_in", delivery_profile = "newest"),
            output("video"),
        });
        assert_eq!(parsed.execution, ProcessorSchemaExecution::Manual);
        assert_eq!(parsed.scheduling, Some(ThreadPriority::High));
        assert_eq!(parsed.inputs.len(), 1);
        assert_eq!(parsed.inputs[0].name, "video_in");
        assert_eq!(parsed.inputs[0].delivery_profile.as_deref(), Some("newest"));
        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.outputs[0].name, "video");
        // Output ports never carry a delivery profile.
        assert_eq!(parsed.outputs[0].delivery_profile, None);
    }

    #[test]
    fn processor_and_port_descriptions_parse() {
        // The descriptor's introspection description surface (#1409): both the
        // processor description and each port description are carried by the
        // attribute and reach the ParsedProcessorAttr.
        let parsed = parse_ok(quote! {
            description = "Captures video from cameras",
            execution = manual,
            input(
                "video_in",
                delivery_profile = "newest",
                description = "Frames to convert"
            ),
            output("video", description = "Live video frames"),
        });
        assert_eq!(
            parsed.description.as_deref(),
            Some("Captures video from cameras")
        );
        assert_eq!(
            parsed.inputs[0].description.as_deref(),
            Some("Frames to convert")
        );
        assert_eq!(
            parsed.outputs[0].description.as_deref(),
            Some("Live video frames")
        );
    }

    #[test]
    fn continuous_with_interval() {
        let parsed = parse_ok(quote! {
            execution = continuous(interval_ms = 10),
        });
        assert_eq!(
            parsed.execution,
            ProcessorSchemaExecution::Continuous { interval_ms: 10 }
        );
    }

    #[test]
    fn continuous_without_interval_defaults_to_zero() {
        let parsed = parse_ok(quote! {
            execution = continuous,
        });
        assert_eq!(
            parsed.execution,
            ProcessorSchemaExecution::Continuous { interval_ms: 0 }
        );
    }

    #[test]
    fn a_positional_port_schema_is_an_error() {
        // A port declares a name and keyed options only. The old
        // `input("name", <schema>, ...)` form must fail to parse, not be
        // silently accepted and dropped.
        let msg = parse_err(quote! {
            execution = manual,
            input("in1", "@tatolab/core/VideoFrame", delivery_profile = "newest"),
        });
        assert!(
            msg.contains("a port declares no type") && msg.contains("input(\"in1\", ...)"),
            "the error must name the removal and the offending port: {msg}"
        );
    }

    #[test]
    fn a_bare_any_port_schema_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            output("out1", any),
        });
        assert!(
            msg.contains("a port declares no type") && msg.contains("output(\"out1\", ...)"),
            "the error must name the removal and the offending port: {msg}"
        );
    }

    #[test]
    fn config_type_and_synthesized_schema_id() {
        let parsed = parse_ok(quote! {
            execution = manual,
            config = crate::camera_config::CameraConfig,
        });
        assert!(parsed.config_type.is_some());
        assert_eq!(parsed.config_field_name, "config");
        // The synthesized config-schema id is version-free.
        assert_eq!(parsed.config_schema_id.as_deref(), Some("CameraConfig"));
    }

    #[test]
    fn explicit_config_schema_overrides_synthesis() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            config = crate::BufferRechunkerConfig,
            config_schema = "com.tatolab.buffer_rechunker.config@1.0.0",
        });
        assert_eq!(
            parsed.config_schema_id.as_deref(),
            Some("com.tatolab.buffer_rechunker.config@1.0.0")
        );
    }

    #[test]
    fn no_config_has_no_schema_id() {
        let parsed = parse_ok(quote! {
            execution = manual,
        });
        assert!(parsed.config_type.is_none());
        assert!(parsed.config_schema_id.is_none());
    }

    #[test]
    fn the_class_short_name_is_the_authored_struct_ident() {
        // The display-name default's only carrier. It is read off the item the
        // attribute is attached to — never authored, and never recovered by
        // splitting the import path.
        let parsed = parse2(quote! { execution = reactive }, &ident("MyLocalProcessor"))
            .expect("a bare processor should parse");
        assert_eq!(parsed.processor_class_short_name, "MyLocalProcessor");
        assert_eq!(parsed.to_processor_schema().name, "MyLocalProcessor");
    }

    #[test]
    fn unsafe_send_flag() {
        let parsed = parse_ok(quote! {
            execution = manual,
            unsafe_send,
        });
        assert!(parsed.unsafe_send);
    }

    // ---- error cases ----

    #[test]
    fn missing_execution_is_an_error() {
        let msg = parse_err(quote! { description = "no execution" });
        assert!(msg.contains("missing required `execution`"), "got: {msg}");
    }

    #[test]
    fn duplicate_input_port_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            input("dup", delivery_profile = "newest"),
            input("dup", delivery_profile = "newest"),
        });
        assert!(
            msg.contains("duplicate input port name `dup`"),
            "got: {msg}"
        );
    }

    #[test]
    fn duplicate_output_port_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            output("dup"),
            output("dup"),
        });
        assert!(
            msg.contains("duplicate output port name `dup`"),
            "got: {msg}"
        );
    }

    #[test]
    fn output_delivery_profile_is_rejected() {
        // Regression: `delivery_profile` is a consumer-side setting on an
        // `output(...)`. It must be a spanned error, not silently nulled.
        // Mentally revert `reject_delivery_profile_on_output` and this parses
        // cleanly (bug) instead of erroring.
        let tokens: proc_macro2::TokenStream =
            "execution = manual, output(\"video\", delivery_profile = \"newest\")"
                .parse()
                .expect("token stream parses");
        let msg = parse_err(tokens);
        assert!(
            msg.contains("`delivery_profile` is a consumer-side setting"),
            "got: {msg}"
        );
    }

    #[test]
    fn input_delivery_profile_is_accepted() {
        // The mirror of the rejection test: `delivery_profile` stays valid on
        // an `input(...)` and reaches the parsed port.
        let parsed = parse_ok(quote! {
            execution = manual,
            input("video_in", delivery_profile = "ordered"),
        });
        assert_eq!(
            parsed.inputs[0].delivery_profile.as_deref(),
            Some("ordered")
        );
    }

    #[test]
    fn input_without_a_delivery_profile_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            input("video_in"),
        });
        assert!(
            msg.contains("input port `video_in` must declare a `delivery_profile`"),
            "got: {msg}"
        );
        assert!(
            msg.contains("newest") && msg.contains("ordered"),
            "the error must list the valid profiles: {msg}"
        );
    }

    #[test]
    fn output_without_a_delivery_profile_stays_valid() {
        // The requirement is consumer-side only: an `output(...)` declaring no
        // profile is the correct shape, not a missing declaration.
        let parsed = parse_ok(quote! {
            execution = manual,
            output("video"),
        });
        assert_eq!(parsed.outputs[0].delivery_profile, None);
    }

    // ---- `audio_window`: the declaration, and every way it is refused ----
    /// The unknown-port-key list is the author's map of the port grammar, so a
    /// key the grammar accepts and the list omits is a key nobody finds.
    #[test]
    fn the_unknown_port_key_error_offers_every_key_a_port_accepts() {
        let msg = parse_err(quote! {
            execution = reactive,
            input("audio", delivery_profile = "ordered", frobnicate = "yes"),
        });

        assert!(msg.contains("unknown port key `frobnicate`"), "got: {msg}");
        for key in ["delivery_profile", "description", "audio_window"] {
            assert!(msg.contains(key), "the list must offer `{key}`; got: {msg}");
        }
    }

    fn declared_contract(parsed: &ParsedProcessorAttr) -> &AudioWindowContractDeclaredValues {
        match parsed.inputs[0]
            .audio_window
            .as_ref()
            .expect("the port declared a contract")
        {
            AudioWindowContract::Declaration(values) => values,
            other => panic!("expected declared values, got {other:?}"),
        }
    }

    #[test]
    fn an_audio_window_declares_its_five_values() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(
                    sample_rate = 16_000,
                    channels = 1,
                    dtype = "f32",
                    window_size = 512,
                    hop = 512
                )
            ),
        });

        assert_eq!(
            declared_contract(&parsed),
            &AudioWindowContractDeclaredValues {
                sample_rate: 16_000,
                channels: Some(1),
                dtype: "f32".to_string(),
                window_size: 512,
                hop: 512,
            }
        );
    }

    /// `hop` is the one omittable field: leaving it out is how an author asks
    /// for contiguous, non-overlapping windows.
    #[test]
    fn an_omitted_hop_defaults_to_the_window_size() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 400)
            ),
        });

        assert_eq!(declared_contract(&parsed).hop, 400);
    }

    #[test]
    fn the_sentinel_is_a_whole_contract() {
        let parsed = parse_ok(quote! {
            execution = manual,
            input("audio", delivery_profile = "ordered", audio_window = match_device),
        });

        assert_eq!(
            parsed.inputs[0].audio_window,
            Some(AudioWindowContract::MatchDevice {})
        );
    }

    #[test]
    fn a_port_declaring_no_contract_carries_none() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input("video_in", delivery_profile = "newest"),
            output("video_out"),
        });

        assert_eq!(parsed.inputs[0].audio_window, None);
        assert_eq!(parsed.outputs[0].audio_window, None);
    }

    #[test]
    fn a_partial_contract_is_refused_naming_the_missing_fields() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(sample_rate = 16_000, window_size = 512)
            ),
        });

        assert!(
            msg.contains("partial") && msg.contains("`dtype`"),
            "the refusal must name the missing fields; got {msg}"
        );
    }

    #[test]
    fn a_hop_above_the_window_size_is_refused_naming_both_numbers() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(
                    sample_rate = 16_000,
                    channels = 1,
                    dtype = "f32",
                    window_size = 512,
                    hop = 1024
                )
            ),
        });

        assert!(
            msg.contains("1024") && msg.contains("512"),
            "the refusal must name both numbers; got {msg}"
        );
    }

    #[test]
    fn a_contract_beside_a_skipping_delivery_profile_is_refused_naming_both_knobs() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "newest",
                audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 512)
            ),
        });

        assert!(
            msg.contains("audio_window") && msg.contains("newest") && msg.contains("ordered"),
            "the refusal must name both knobs; got {msg}"
        );
    }

    /// The profile refusal is checked after the whole port body, so it cannot
    /// depend on which key the author happened to write first.
    #[test]
    fn a_contract_declared_before_its_profile_is_refused_the_same() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 512),
                delivery_profile = "newest"
            ),
        });

        assert!(
            msg.contains("audio_window") && msg.contains("ordered"),
            "key order must not change the refusal; got {msg}"
        );
    }

    /// The sentinel is a contract too, so it takes the same profile refusal.
    #[test]
    fn the_sentinel_beside_a_skipping_delivery_profile_is_refused_too() {
        let msg = parse_err(quote! {
            execution = manual,
            input("audio", delivery_profile = "newest", audio_window = match_device),
        });

        assert!(
            msg.contains("ordered"),
            "the refusal must name the profile a contract requires; got {msg}"
        );
    }

    #[test]
    fn a_zero_numeric_field_is_refused_naming_the_field_and_the_value() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 0)
            ),
        });

        assert!(
            msg.contains("window_size") && msg.contains("is 0"),
            "the refusal must name the field and the value; got {msg}"
        );
    }

    /// The fields are unsigned, so a negative literal would otherwise reach the
    /// author as syn's bare `expected integer literal`.
    #[test]
    fn a_negative_numeric_field_is_refused_by_name() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(sample_rate = -1, channels = 1, dtype = "f32", window_size = 512)
            ),
        });

        assert!(
            msg.contains("negative") && msg.contains("-1"),
            "the refusal must name the value; got {msg}"
        );
    }

    #[test]
    fn an_unknown_dtype_is_refused_listing_the_legal_values() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(sample_rate = 16_000, channels = 1, dtype = "f64", window_size = 512)
            ),
        });

        assert!(
            msg.contains("f64") && msg.contains("f32") && msg.contains("i16"),
            "the refusal must name the value and the legal ones; got {msg}"
        );
    }

    #[test]
    fn an_unknown_audio_window_field_is_refused_listing_the_five() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(
                    sample_rate = 16_000,
                    channels = 1,
                    dtype = "f32",
                    window_size = 512,
                    overlap = 128
                )
            ),
        });

        assert!(
            msg.contains("`overlap`") && msg.contains("window_size"),
            "the refusal must name the stray field and the legal ones; got {msg}"
        );
    }

    #[test]
    fn an_unknown_sentinel_is_refused_naming_the_only_one() {
        let msg = parse_err(quote! {
            execution = manual,
            input("audio", delivery_profile = "ordered", audio_window = match_whatever),
        });

        assert!(
            msg.contains("match_whatever") && msg.contains("match_device"),
            "the refusal must name what was written and the only sentinel; got {msg}"
        );
    }

    #[test]
    fn an_audio_window_on_an_output_is_refused() {
        let msg = parse_err(quote! {
            execution = manual,
            output("audio_out", audio_window = match_device),
        });

        assert!(
            msg.contains("consumer-side") && msg.contains("audio_out"),
            "the refusal must name the port and say whose setting it is; got {msg}"
        );
    }

    #[test]
    fn a_second_audio_window_on_one_port_is_refused() {
        let msg = parse_err(quote! {
            execution = manual,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window = match_device,
                audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 512)
            ),
        });

        assert!(
            msg.contains("more than once"),
            "a second contract is refused; got {msg}"
        );
    }

    #[test]
    fn the_projected_schema_carries_the_contract_the_port_declared() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(
                    sample_rate = 16_000,
                    channels = 1,
                    dtype = "f32",
                    window_size = 512,
                    hop = 160
                )
            ),
        });

        let schema = parsed.to_processor_schema();
        assert_eq!(
            serde_json::to_value(&schema.inputs[0].audio_window).unwrap(),
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 160,
            })
        );
    }


    /// The count is the one value a port may leave to whatever is upstream of
    /// it, so a graph can grow a source without every consumer being edited.
    #[test]
    fn a_contract_omitting_its_channel_count_parses_as_following_the_source() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(
                    sample_rate = 48_000,
                    dtype = "f32",
                    window_size = 960,
                    hop = 960
                )
            ),
        });

        assert_eq!(
            declared_contract(&parsed),
            &AudioWindowContractDeclaredValues {
                sample_rate: 48_000,
                channels: None,
                dtype: "f32".to_string(),
                window_size: 960,
                hop: 960,
            }
        );
    }

    /// Relaxing the count relaxed nothing else: the refusal still names each
    /// value the stage cannot run without.
    #[test]
    fn a_contract_missing_a_value_other_than_the_count_is_still_refused_by_name() {
        for (declaration, expected) in [
            (
                quote! { audio_window(dtype = "f32", window_size = 512) },
                "`sample_rate`",
            ),
            (
                quote! { audio_window(sample_rate = 16_000, window_size = 512) },
                "`dtype`",
            ),
            (
                quote! { audio_window(sample_rate = 16_000, dtype = "f32") },
                "`window_size`",
            ),
        ] {
            let msg = parse_err(quote! {
                execution = reactive,
                input("audio", delivery_profile = "ordered", #declaration),
            });

            assert!(
                msg.contains("partial") && msg.contains(expected),
                "the refusal must name {expected}; got {msg}"
            );
        }
    }

    #[test]
    fn a_port_following_the_source_projects_its_count_as_the_source() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input(
                "audio",
                delivery_profile = "ordered",
                audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960)
            ),
        });

        let schema = parsed.to_processor_schema();
        assert_eq!(
            serde_json::to_value(&schema.inputs[0].audio_window).unwrap(),
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 48_000,
                "channels": "source",
                "dtype": "f32",
                "window_size": 960,
                "hop": 960,
            })
        );
    }

    /// A contract-less port must project exactly what it always did — the
    /// field is absent from the rendering, not present as a null.
    #[test]
    fn a_contract_less_ports_projection_carries_no_audio_window_key() {
        let parsed = parse_ok(quote! {
            execution = reactive,
            input("video_in", delivery_profile = "newest"),
        });

        let schema = parsed.to_processor_schema();
        let json = serde_json::to_value(&schema.inputs[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "video_in",
                "description": null,
                "delivery_profile": "newest",
            })
        );
    }

    #[test]
    fn unknown_key_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            frobnicate = "yes",
        });
        assert!(
            msg.contains("unknown `#[processor(...)]` key `frobnicate`"),
            "got: {msg}"
        );
    }

    #[test]
    fn unknown_execution_mode_is_an_error() {
        let msg = parse_err(quote! {
            execution = sideways,
        });
        assert!(
            msg.contains("unknown execution mode `sideways`"),
            "got: {msg}"
        );
    }

    #[test]
    fn a_positional_identity_is_refused_naming_the_class_path_rule() {
        // Every spelling the deleted grammar accepted lands on one refusal —
        // including the ones it used to reject for its own reasons, which must
        // not leak a message about a grammar that no longer exists. Mental-
        // revert guard: restore the positional `LitStr` parse and these parse
        // clean instead of erroring.
        for identity in [
            "\"@tatolab/camera/Camera\", execution = manual",
            "\"@tatolab/camera/Camera@1.0.0\", execution = manual",
            "\"tatolab/camera/Camera\", execution = manual",
            "\"@tatolab/camera\", execution = manual",
        ] {
            let tokens: proc_macro2::TokenStream = identity.parse().expect("token stream parses");
            let msg = parse_err(tokens);
            assert!(
                msg.contains("declares no identity") && msg.contains("import path"),
                "got: {msg}"
            );
        }
    }

    #[test]
    fn a_type_override_is_refused_naming_the_class_path_rule() {
        let msg = parse_err(quote! { execution = reactive, type = "CustomName" });
        assert!(msg.contains("declares no identity"), "got: {msg}");
    }

    #[test]
    fn type_is_not_offered_as_a_valid_key() {
        // The unknown-key list is the author's map of the grammar; leaving
        // `type` on it would point them at a key that now errors.
        let msg = parse_err(quote! { execution = reactive, bogus = "x" });
        assert!(
            msg.contains("unknown `#[processor(...)]` key"),
            "got: {msg}"
        );
        assert!(!msg.contains("`type`"), "got: {msg}");
    }

    #[test]
    fn continuous_unknown_key_is_an_error() {
        let msg = parse_err(quote! {
            execution = continuous(period = 5),
        });
        assert!(msg.contains("expected `interval_ms`"), "got: {msg}");
    }

    #[test]
    fn quote_placeholder_keeps_quote_in_scope() {
        // Guards the test-only `quote` import stays wired.
        let _ = quote! {};
    }
}
