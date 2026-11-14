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
//!     built_in_node,                    // flag — type is `tatolab.stream:<struct name>`
//!     paced_by_real_time,               // flag — paces itself on real time, as a live device does
//!     config = crate::CameraConfig,     // typed Config alias; must derive `JsonSchema`
//!     input("video_in", delivery_profile = "newest", family = Image),
//!     output("video", family = Image),
//! )]
//! #[meta(category = "Sources/Cameras", description = "Captures a camera.")]
//! ```
//!
//! What describes the node type, its `category` and `description`, is the
//! `#[meta(...)]` helper attribute written under `#[processor(...)]`. A
//! category is the group an editor files the node type under, or a path into a
//! group inside one; a node type that declares none has none.
//!
//! A port declares a name, its family, a description, and — on an input — a
//! delivery profile, plus the family's keys, an optional `audio_window`
//! contract on an audio input.
//!
//! The attribute declares no identity. A processor is named by the import path
//! of its type, captured by the macro at the expansion site — or, for a
//! declaration flagged `built_in_node`, by its class in `tatolab.stream`.

use streamlib_processor_schema::{
    ArrayOutputPortResidency, AudioWindowContract, AudioWindowContractDeclaredValues,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    DELIVERY_PROFILE_DECLARATION_VALUES, ImageOutputPortFrameRate, MAX_INPUT_PORT_BORROW_QUOTA,
    OutputPortDeclaredNames, PortFamily, PortFamilyDeclaration, PortFamilyDeclarationDirection,
    ProcessorPortSchema, ProcessorScheduling, ProcessorSchema, ProcessorSchemaExecution,
    RuntimeConfig, RuntimeOptions, ThreadPriority, cast_exposed_name_to_url_safe,
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
use syn::ext::IdentExt;
use syn::parse::{ParseStream, Parser};
use syn::spanned::Spanned;
use syn::{Attribute, Ident, LitInt, LitStr, Path, Token, parenthesized};
use unicode_normalization::UnicodeNormalization;

/// Which side of a link a port sits on. `delivery_profile` is a consumer-side
/// setting only valid on an `input(...)`; the grammar rejects it on an
/// `output(...)`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortDirection {
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

    /// The module the generated port markers for this direction live in.
    pub(crate) fn port_marker_module_ident(self) -> Ident {
        let name = match self {
            PortDirection::Input => "InputLink",
            PortDirection::Output => "OutputLink",
        };
        Ident::new(name, proc_macro2::Span::call_site())
    }

    /// The trait the generated port markers for this direction implement.
    pub(crate) fn port_marker_trait_ident(self) -> Ident {
        let name = match self {
            PortDirection::Input => "InputPortMarker",
            PortDirection::Output => "OutputPortMarker",
        };
        Ident::new(name, proc_macro2::Span::call_site())
    }
}

/// A `description = …` value: CommonMark, stored and served exactly as written.
pub enum DeclaredDescription {
    /// `description = "…"`.
    Literal(LitStr),
    /// `description = SOME_CONST`, the path of a `const` `&str`.
    Constant(Path),
}

impl DeclaredDescription {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(LitStr) {
            return Ok(DeclaredDescription::Literal(input.parse()?));
        }
        let fork = input.fork();
        if fork.parse::<Path>().is_ok() && (fork.is_empty() || fork.peek(Token![,])) {
            return Ok(DeclaredDescription::Constant(input.parse()?));
        }
        Err(syn::Error::new(
            input.span(),
            "`description` takes a string literal or the path of a `const` holding one — \
             `description = \"…\"` or `description = MY_NODE_DESCRIPTION`",
        ))
    }

    /// The literal's text; `None` for a `const`, whose value only rustc knows.
    pub fn literal_text(&self) -> Option<String> {
        match self {
            DeclaredDescription::Literal(literal) => Some(literal.value()),
            DeclaredDescription::Constant(_) => None,
        }
    }

    /// The `&'static str` expression the descriptor is built from. A constant
    /// is bound to a `const` first, so a `static` or a non-`&str` is refused
    /// where it is named.
    pub fn to_str_expression(&self) -> proc_macro2::TokenStream {
        match self {
            DeclaredDescription::Literal(literal) => quote::quote! { #literal },
            DeclaredDescription::Constant(path) => quote::quote_spanned! { path.span() =>
                {
                    const DECLARED_DESCRIPTION: &str = #path;
                    DECLARED_DESCRIPTION
                }
            },
        }
    }
}

/// The `&'static str` a declaration's optional description builds into; `""`
/// where none is declared.
pub fn declared_description_str_expression(
    description: Option<&DeclaredDescription>,
) -> proc_macro2::TokenStream {
    match description {
        Some(description) => description.to_str_expression(),
        None => quote::quote! { "" },
    }
}

/// The helper attribute a node type's category and description are declared in.
const NODE_META_ATTRIBUTE_NAME: &str = "meta";

/// What a category path's levels are separated by, as `Effects/Stylize`.
const NODE_CATEGORY_PATH_SEPARATOR: char = '/';

/// What the `#[meta(...)]` attributes under `#[processor]` declare; the twin of
/// `tatolab-module-sdk-macros`' `DeclaredNodeMeta`, which reads the same
/// grammar under `#[function_node]`.
#[derive(Default)]
pub struct DeclaredNodeMeta {
    /// The group an editor files the node type under, or a path into a group
    /// inside one, exactly as written.
    pub category: Option<String>,
    /// A string literal or the path of a `const`.
    pub description: Option<DeclaredDescription>,
}

impl DeclaredNodeMeta {
    /// Take every `#[meta(...)]` off `attributes`, since rustc knows no such attribute, and read what they declare.
    pub fn take_from_attributes(attributes: &mut Vec<Attribute>) -> syn::Result<Self> {
        let mut declared = DeclaredNodeMeta::default();
        let mut refusal: Option<syn::Error> = None;
        attributes.retain(|attribute| {
            if !attribute.path().is_ident(NODE_META_ATTRIBUTE_NAME) {
                return true;
            }
            if let Err(refused) =
                attribute.parse_args_with(|input: ParseStream<'_>| declared.read_meta_keys(input))
            {
                match &mut refusal {
                    Some(earlier) => earlier.combine(refused),
                    None => refusal = Some(refused),
                }
            }
            false
        });
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(declared),
        }
    }

    fn read_meta_keys(&mut self, input: ParseStream<'_>) -> syn::Result<()> {
        if input.is_empty() {
            return Err(syn::Error::new(
                input.span(),
                "`#[meta()]` declares nothing — it takes `category`, `description` or both, \
                 `#[meta(category = \"Effects/Stylize\", description = \"…\")]`",
            ));
        }
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "category" => {
                    let category: LitStr = input.parse().map_err(|not_a_literal| {
                        syn::Error::new(
                            not_a_literal.span(),
                            "`category` takes a string literal — `category = \"Effects\"`, or a \
                             path into a group inside one, `category = \"Effects/Stylize\"`",
                        )
                    })?;
                    refuse_a_category_with_a_level_that_has_no_name(&category)?;
                    if self.category.replace(category.value()).is_some() {
                        return Err(declared_more_than_once(&key));
                    }
                }
                "description" => {
                    let description = DeclaredDescription::parse(input)?;
                    if self.description.replace(description).is_some() {
                        return Err(declared_more_than_once(&key));
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown `#[meta(...)]` key `{other}` — expected `category` or \
                             `description`"
                        ),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(())
    }
}

fn declared_more_than_once(key: &Ident) -> syn::Error {
    syn::Error::new(
        key.span(),
        format!("`{key}` is declared more than once in this node's `#[meta(...)]`"),
    )
}

fn refuse_a_category_with_a_level_that_has_no_name(category: &LitStr) -> syn::Result<()> {
    if category
        .value()
        .split(NODE_CATEGORY_PATH_SEPARATOR)
        .any(|level| level.trim().is_empty())
    {
        return Err(syn::Error::new(
            category.span(),
            "this category names a group with no name — a category is `\"Effects\"`, or a path \
             whose every level is named, `\"Effects/Stylize\"`",
        ));
    }
    Ok(())
}

/// A parsed input/output port declaration.
pub struct ParsedPort {
    /// The name as the author wrote it, spanned at the declaration — the
    /// marker type's identifier (`OutputLink::videoOut`).
    pub declared_marker_ident: Ident,
    /// The declared name as every exposed name is cast — what the
    /// descriptor and the marker's `PORT_NAME` carry.
    pub cast_name: String,
    pub description: Option<DeclaredDescription>,
    /// Always `Some` on an input and always `None` on an output — the grammar
    /// requires it on the one and rejects it on the other.
    pub delivery_profile: Option<String>,
    /// The `borrow_quota` an input port declares; `None` takes the engine's
    /// default, and always `None` on an output.
    pub borrow_quota: Option<usize>,
    /// The `audio_window` contract, on the audio inputs that declare one.
    /// `None` everywhere else — this is opt-in, and a port without one is
    /// unchanged in every respect.
    pub audio_window: Option<AudioWindowContract>,
    /// The port family the port declares with `family = …` and its family's
    /// keys.
    pub family: PortFamilyDeclaration,
}

/// The fully-parsed `#[processor(...)]` attribute.
pub struct ParsedProcessorAttr {
    /// The author's type name — what an instance's display name defaults to.
    pub processor_class_short_name: String,
    /// The description the node's `#[meta(...)]` declares.
    pub description: Option<DeclaredDescription>,
    /// The category the node's `#[meta(...)]` declares, exactly as written.
    pub category: Option<String>,
    pub execution: ProcessorSchemaExecution,
    pub scheduling: Option<ThreadPriority>,
    pub unsafe_send: bool,
    /// Whether this is one of the runtime's built-in nodes, typed
    /// `tatolab.stream:<struct name>` rather than by its module path.
    pub built_in_node: bool,
    /// Whether the processor paces itself on real time, as a live device does.
    pub paced_by_real_time: bool,
    pub config_type: Option<Path>,
    pub config_field_name: String,
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
    /// release-core catalog entry. A description declared as a `const` has no
    /// value until rustc evaluates it, so it projects as `None`.
    pub fn to_processor_schema(&self) -> ProcessorSchema {
        let to_port = |p: &ParsedPort| ProcessorPortSchema {
            name: p.cast_name.clone(),
            description: p
                .description
                .as_ref()
                .and_then(DeclaredDescription::literal_text),
            delivery_profile: p.delivery_profile.clone(),
            borrow_quota: p.borrow_quota,
            audio_window: p.audio_window.clone(),
            family: p.family.clone(),
        };

        ProcessorSchema {
            name: self.processor_class_short_name.clone(),
            description: self
                .description
                .as_ref()
                .and_then(DeclaredDescription::literal_text),
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
            paced_by_real_time: self.paced_by_real_time,
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
/// `struct_ident` supplies the class short name the display-name default reads,
/// and `node_meta` what the `#[meta(...)]` under the attribute declares.
pub fn parse2(
    attr: proc_macro2::TokenStream,
    node_meta: DeclaredNodeMeta,
    struct_ident: &Ident,
) -> syn::Result<ParsedProcessorAttr> {
    let struct_name = struct_ident.to_string();
    let parser = move |input: ParseStream<'_>| parse_body(input, node_meta, &struct_name);
    parser.parse2(attr)
}

fn parse_body(
    input: ParseStream<'_>,
    node_meta: DeclaredNodeMeta,
    struct_name: &str,
) -> syn::Result<ParsedProcessorAttr> {
    let mut execution: Option<ProcessorSchemaExecution> = None;
    let mut scheduling: Option<ThreadPriority> = None;
    let mut unsafe_send = false;
    let mut built_in_node = false;
    let mut paced_by_real_time = false;
    let mut config_type: Option<Path> = None;
    let mut config_field_name: Option<String> = None;
    let mut inputs: Vec<ParsedPort> = Vec::new();
    let mut outputs: Vec<ParsedPort> = Vec::new();

    reject_positional_identity(input)?;

    while !input.is_empty() {
        // `parse_any` so keyword-like keys (`type`) are accepted as raw idents.
        let key = Ident::parse_any(input)?;
        match key.to_string().as_str() {
            "unsafe_send" => unsafe_send = true,
            "built_in_node" => built_in_node = true,
            "paced_by_real_time" => paced_by_real_time = true,
            meta_key @ ("description" | "category") => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "`{meta_key}` describes the node type, so it is declared in the \
                         `#[meta(...)]` under `#[processor(...)]`, \
                         `#[meta({meta_key} = \"…\")]`, never here"
                    ),
                ));
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

    refuse_ports_that_cast_alike(inputs.iter().chain(&outputs))?;

    let execution = execution.ok_or_else(|| {
        syn::Error::new(
            input.span(),
            "missing required `execution` — declare `execution = reactive`, \
             `execution = manual`, or `execution = continuous(interval_ms = N)`",
        )
    })?;

    let config_field_name = config_field_name.unwrap_or_else(|| "config".to_string());

    Ok(ParsedProcessorAttr {
        processor_class_short_name: struct_name.to_string(),
        description: node_meta.description,
        category: node_meta.category,
        execution,
        scheduling,
        unsafe_send,
        built_in_node,
        paced_by_real_time,
        config_type,
        config_field_name,
        inputs,
        outputs,
    })
}

/// Every port, input or output, needs a name of its own once cast — the rule
/// `@node.process` holds a Python class to.
fn refuse_ports_that_cast_alike<'port>(
    ports: impl IntoIterator<Item = &'port ParsedPort>,
) -> syn::Result<()> {
    let mut declared_marker_ident_by_cast_name = std::collections::HashMap::new();
    for port in ports {
        let Some(first_declared_marker_ident) = declared_marker_ident_by_cast_name
            .insert(port.cast_name.as_str(), &port.declared_marker_ident)
        else {
            continue;
        };
        let refusal = if *first_declared_marker_ident == port.declared_marker_ident {
            format!(
                "the port name `{}` is declared more than once — every port, input or output, \
                 needs its own name",
                port.declared_marker_ident
            )
        } else {
            format!(
                "the ports `{first_declared_marker_ident}` and `{}` both cast to `{}` — a port name is \
                 lowercased with its accents dropped and anything outside a-z 0-9 - . _ ~ \
                 turned into '-', so two ports need names that stay apart once cast",
                port.declared_marker_ident, port.cast_name
            )
        };
        return Err(syn::Error::new(port.declared_marker_ident.span(), refusal));
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
/// `<name-string>[, delivery_profile = "...", borrow_quota = <n>, description = "...",
/// audio_window(...) | audio_window = match_device, catalog = <Section>Entry { … }]`.
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
                         `borrow_quota`, `description`, `audio_window`, `family`, or one of \
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

    let family = match (family, first_family_key_span) {
        (Some(family), _) => {
            family_keys.family = family;
            let declaration_direction = match direction {
                PortDirection::Input => PortFamilyDeclarationDirection::Input,
                PortDirection::Output => PortFamilyDeclarationDirection::Output,
            };
            if let Some(refusal) = family_keys.refusal_of_its_keys(&name, declaration_direction) {
                return Err(syn::Error::new(name_lit.span(), refusal));
            }
            family_keys
        }
        (None, Some(family_key_span)) => {
            return Err(syn::Error::new(
                family_key_span,
                format!(
                    "port `{name}` states a family key without `family = …` — one of {}",
                    rendered_port_family_names()
                ),
            ));
        }
        (None, None) => {
            return Err(syn::Error::new(
                name_lit.span(),
                format!(
                    "port `{name}` declares no family — add `family = …`, one of {}",
                    rendered_port_family_names()
                ),
            ));
        }
    };

    Ok(ParsedPort {
        declared_marker_ident,
        cast_name,
        description,
        delivery_profile,
        borrow_quota,
        audio_window: audio_window.map(|(_, contract)| contract),
        family,
    })
}

fn rendered_port_family_names() -> String {
    PortFamily::ALL
        .iter()
        .map(|family| format!("`{family}`"))
        .collect::<Vec<_>>()
        .join(", ")
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
                    ImageOutputPortFrameRate::Stated(
                        streamlib_processor_schema::ImageStreamFrameRate {
                            numerator: frames_per_second.base10_parse()?,
                            denominator: 1,
                        },
                    )
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
    "built_in_node",
    "paced_by_real_time",
    "config",
    "config_field",
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
        parse2(tokens, DeclaredNodeMeta::default(), &ident("MyProcessor"))
            .expect("attribute should parse")
    }

    fn parse_err(tokens: proc_macro2::TokenStream) -> String {
        match parse2(tokens, DeclaredNodeMeta::default(), &ident("MyProcessor")) {
            Ok(_) => panic!("attribute should fail to parse"),
            Err(err) => err.to_string(),
        }
    }

    /// The struct a `#[processor]` is handed, with `attributes` written under it.
    fn node_struct_with(attributes: proc_macro2::TokenStream) -> syn::ItemStruct {
        syn::parse_quote! {
            #attributes
            struct MyProcessor;
        }
    }

    fn node_meta_ok(attributes: proc_macro2::TokenStream) -> DeclaredNodeMeta {
        let mut node_struct = node_struct_with(attributes);
        DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs)
            .expect("the node's meta should read")
    }

    fn node_meta_err(attributes: proc_macro2::TokenStream) -> String {
        let mut node_struct = node_struct_with(attributes);
        match DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs) {
            Ok(_) => panic!("the node's meta should be refused"),
            Err(refusal) => refusal.to_string(),
        }
    }

    fn parse_ok_with_meta(
        attributes: proc_macro2::TokenStream,
        tokens: proc_macro2::TokenStream,
    ) -> ParsedProcessorAttr {
        parse2(tokens, node_meta_ok(attributes), &ident("MyProcessor"))
            .expect("attribute should parse")
    }

    #[test]
    fn full_execution_and_ports() {
        let parsed = parse_ok(quote! {
            execution = manual,
            scheduling = high,
            input("video_in", family = Image, delivery_profile = "newest"),
            output("video", family = Image),
        });
        assert_eq!(parsed.execution, ProcessorSchemaExecution::Manual);
        assert_eq!(parsed.scheduling, Some(ThreadPriority::High));
        assert_eq!(parsed.inputs.len(), 1);
        assert_eq!(parsed.inputs[0].cast_name, "video_in");
        assert_eq!(parsed.inputs[0].delivery_profile.as_deref(), Some("newest"));
        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.outputs[0].cast_name, "video");
        // Output ports never carry a delivery profile.
        assert_eq!(parsed.outputs[0].delivery_profile, None);
    }

    #[test]
    fn processor_and_port_descriptions_parse() {
        let parsed = parse_ok_with_meta(
            quote! { #[meta(description = "Captures video from cameras")] },
            quote! {
                execution = manual,
                input(
                    "video_in",
                    family = Image,
                    delivery_profile = "newest",
                    description = "Frames to convert"
                ),
                output("video", family = Image, description = "Live video frames"),
            },
        );
        let literal_text_of = |description: &Option<DeclaredDescription>| {
            description
                .as_ref()
                .and_then(DeclaredDescription::literal_text)
        };
        assert_eq!(
            literal_text_of(&parsed.description).as_deref(),
            Some("Captures video from cameras")
        );
        assert_eq!(
            literal_text_of(&parsed.inputs[0].description).as_deref(),
            Some("Frames to convert")
        );
        assert_eq!(
            literal_text_of(&parsed.outputs[0].description).as_deref(),
            Some("Live video frames")
        );
    }

    #[test]
    fn a_description_may_name_a_const_on_the_node_and_on_a_port() {
        let parsed = parse_ok_with_meta(
            quote! { #[meta(description = crate::descriptions::CAMERA_NODE_DESCRIPTION)] },
            quote! {
                execution = manual,
                output("video", family = Image, description = VIDEO_PORT_DESCRIPTION),
            },
        );
        assert!(matches!(
            &parsed.description,
            Some(DeclaredDescription::Constant(path))
                if quote!(#path).to_string() == "crate :: descriptions :: CAMERA_NODE_DESCRIPTION"
        ));
        assert!(matches!(
            &parsed.outputs[0].description,
            Some(DeclaredDescription::Constant(path)) if path.is_ident("VIDEO_PORT_DESCRIPTION")
        ));
        assert_eq!(parsed.to_processor_schema().description, None);
    }

    #[test]
    fn a_description_that_is_neither_a_literal_nor_a_const_path_is_refused() {
        let msg = node_meta_err(quote! { #[meta(description = include_str!("README.md"))] });
        assert!(
            msg.contains("string literal or the path of a `const`"),
            "got: {msg}"
        );
    }

    #[test]
    fn a_node_meta_declares_a_category_and_a_description_in_one_attribute() {
        let parsed = parse_ok_with_meta(
            quote! { #[meta(category = "Sources/Cameras", description = "Captures a camera.")] },
            quote! { execution = manual },
        );
        assert_eq!(parsed.category.as_deref(), Some("Sources/Cameras"));
        assert_eq!(
            parsed
                .description
                .as_ref()
                .and_then(DeclaredDescription::literal_text)
                .as_deref(),
            Some("Captures a camera.")
        );
    }

    #[test]
    fn a_node_with_no_meta_has_no_category_and_no_description() {
        let parsed = parse_ok(quote! { execution = manual });
        assert_eq!(parsed.category, None);
        assert!(parsed.description.is_none());
    }

    #[test]
    fn a_node_meta_may_be_written_as_one_attribute_per_key() {
        let declared = node_meta_ok(quote! {
            #[meta(category = "Effects")]
            #[meta(description = "Tints a frame.")]
        });
        assert_eq!(declared.category.as_deref(), Some("Effects"));
        assert!(declared.description.is_some());
    }

    #[test]
    fn reading_a_node_meta_takes_it_off_the_struct_and_leaves_every_other_attribute() {
        let mut node_struct = node_struct_with(quote! {
            #[derive(Debug)]
            #[meta(category = "Effects")]
            #[allow(dead_code)]
        });
        DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs)
            .expect("the node's meta should read");
        let left_on_the_struct: Vec<String> = node_struct
            .attrs
            .iter()
            .map(|attribute| {
                attribute
                    .path()
                    .get_ident()
                    .map(Ident::to_string)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(left_on_the_struct, ["derive", "allow"]);
    }

    #[test]
    fn a_node_meta_key_declared_twice_is_refused_naming_the_key() {
        let msg = node_meta_err(quote! {
            #[meta(category = "Effects")]
            #[meta(category = "Sources")]
        });
        assert!(
            msg.contains("`category` is declared more than once"),
            "got: {msg}"
        );
    }

    #[test]
    fn every_node_meta_that_is_refused_is_reported_and_none_is_left_on_the_struct() {
        let mut node_struct = node_struct_with(quote! {
            #[meta(tags = "video")]
            #[allow(dead_code)]
            #[meta(category = "")]
        });
        let refusals: Vec<String> = DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs)
            .err()
            .expect("both metas are refused")
            .into_iter()
            .map(|refusal| refusal.to_string())
            .collect();
        assert_eq!(refusals.len(), 2, "got: {refusals:?}");
        assert_eq!(node_struct.attrs.len(), 1);
    }

    #[test]
    fn a_node_meta_that_declares_nothing_is_refused() {
        let msg = node_meta_err(quote! { #[meta()] });
        assert!(msg.contains("declares nothing"), "got: {msg}");
    }

    #[test]
    fn an_unknown_node_meta_key_is_refused_naming_it_and_the_keys_there_are() {
        let msg = node_meta_err(quote! { #[meta(tags = "video")] });
        assert!(
            msg.contains("unknown `#[meta(...)]` key `tags`"),
            "got: {msg}"
        );
        assert!(msg.contains("`category` or `description`"), "got: {msg}");
    }

    #[test]
    fn a_category_with_a_level_that_has_no_name_is_refused() {
        for category in [
            "",
            "  ",
            "/Effects",
            "Effects/",
            "Effects//Stylize",
            "Effects/ /Blur",
        ] {
            let msg = node_meta_err(quote! { #[meta(category = #category)] });
            assert!(
                msg.contains("names a group with no name"),
                "category {category:?} got: {msg}"
            );
        }
    }

    #[test]
    fn a_category_that_is_not_a_string_literal_is_refused() {
        let msg = node_meta_err(quote! { #[meta(category = EFFECTS_CATEGORY)] });
        assert!(
            msg.contains("`category` takes a string literal"),
            "got: {msg}"
        );
    }

    #[test]
    fn a_meta_key_written_in_the_processor_attribute_is_refused_naming_where_it_goes() {
        for meta_key in ["description", "category"] {
            let key = ident(meta_key);
            let msg = parse_err(quote! { #key = "Captures a camera.", execution = manual });
            assert!(
                msg.contains(&format!("#[meta({meta_key} = \"…\")]")),
                "key {meta_key} got: {msg}"
            );
        }
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
        // `input("name", family = Image, <schema>, ...)` form must fail to parse, not be
        // silently accepted and dropped.
        let msg = parse_err(quote! {
            execution = manual,
            input("in1", family = Image, "@tatolab/core/VideoFrame", delivery_profile = "newest"),
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
            output("out1", family = Image, any),
        });
        assert!(
            msg.contains("a port declares no type") && msg.contains("output(\"out1\", ...)"),
            "the error must name the removal and the offending port: {msg}"
        );
    }

    #[test]
    fn a_config_key_binds_the_type_and_the_default_field_name() {
        let parsed = parse_ok(quote! {
            execution = manual,
            config = crate::camera_config::CameraConfig,
        });
        assert!(parsed.config_type.is_some());
        assert_eq!(parsed.config_field_name, "config");
    }

    #[test]
    fn a_processor_declaring_no_config_binds_no_config_type() {
        let parsed = parse_ok(quote! {
            execution = manual,
        });
        assert!(parsed.config_type.is_none());
    }

    #[test]
    fn the_retired_config_schema_key_is_refused_by_name_with_the_keys_that_remain() {
        let msg = parse_err(quote! {
            execution = reactive,
            config = crate::BufferRechunkerConfig,
            config_schema = "com.tatolab.buffer_rechunker.config@1.0.0",
        });
        assert!(
            msg.contains("`config_schema`") && msg.contains("`config`"),
            "the error must name the retired key and list the ones that remain: {msg}"
        );
    }

    #[test]
    fn the_class_short_name_is_the_authored_struct_ident() {
        // The display-name default's only carrier. It is read off the item the
        // attribute is attached to — never authored, and never recovered by
        // splitting the import path.
        let parsed = parse2(
            quote! { execution = reactive },
            DeclaredNodeMeta::default(),
            &ident("MyLocalProcessor"),
        )
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

    #[test]
    fn paced_by_real_time_flag_reaches_the_schema() {
        let parsed = parse_ok(quote! {
            execution = manual,
            paced_by_real_time,
        });
        assert!(parsed.paced_by_real_time);
        assert!(parsed.to_processor_schema().paced_by_real_time);
        assert!(!parse_ok(quote! { execution = manual }).paced_by_real_time);
    }

    #[test]
    fn built_in_node_flag() {
        assert!(
            parse_ok(quote! {
                execution = manual,
                built_in_node,
            })
            .built_in_node
        );
        assert!(!parse_ok(quote! { execution = manual }).built_in_node);
    }

    // ---- error cases ----

    #[test]
    fn missing_execution_is_an_error() {
        let msg = parse_err(quote! { scheduling = high });
        assert!(msg.contains("missing required `execution`"), "got: {msg}");
    }

    #[test]
    fn a_port_name_declared_twice_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            input("dup", family = Image, delivery_profile = "newest"),
            input("dup", family = Image, delivery_profile = "newest"),
        });
        assert!(
            msg.contains("the port name `dup` is declared more than once"),
            "got: {msg}"
        );
    }

    #[test]
    fn an_input_and_an_output_sharing_a_name_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            input("video", family = Image, delivery_profile = "newest"),
            output("video", family = Image),
        });
        assert!(
            msg.contains("the port name `video` is declared more than once"),
            "got: {msg}"
        );
    }

    #[test]
    fn two_ports_that_cast_alike_are_an_error_naming_both() {
        let msg = parse_err(quote! {
            execution = manual,
            output("videoOut", family = Image),
            output("video_out", family = Image),
            output("VideoOut", family = Image),
        });
        assert!(
            msg.contains("the ports `videoOut` and `VideoOut` both cast to `videoout`"),
            "got: {msg}"
        );
    }

    #[test]
    fn a_port_is_declared_under_its_cast_and_keeps_its_spelling_for_the_marker() {
        let parsed = parse_ok(quote! {
            execution = manual,
            input("VidéoIn", family = Image, delivery_profile = "newest"),
            output("videoOut", family = Image),
        });
        assert_eq!(parsed.inputs[0].declared_marker_ident, "VidéoIn");
        assert_eq!(parsed.inputs[0].cast_name, "videoin");
        assert_eq!(parsed.outputs[0].declared_marker_ident, "videoOut");
        assert_eq!(parsed.outputs[0].cast_name, "videoout");

        let schema = parsed.to_processor_schema();
        assert_eq!(schema.inputs[0].name, "videoin");
        assert_eq!(schema.outputs[0].name, "videoout");
    }

    #[test]
    fn a_port_name_that_is_not_an_identifier_is_an_error() {
        for not_an_identifier in [
            "",
            "..",
            "video in",
            "video-out",
            "type",
            "r#type",
            " videoOut ",
            "videoOut /* c */",
        ] {
            let msg = parse_err(quote! {
                execution = manual,
                output(#not_an_identifier),
            });
            assert!(
                msg.contains(&format!(
                    "port {not_an_identifier:?} is not a plain Rust identifier"
                )),
                "{not_an_identifier:?} got: {msg}"
            );
        }
    }

    /// A decomposed accent names the same identifier rustc's lexer composes it
    /// into, so it is accepted as that identifier.
    #[test]
    fn a_decomposed_spelling_is_declared_as_its_composed_identifier() {
        let parsed = parse_ok(quote! {
            execution = manual,
            output("Vide\u{301}oOut", family = Image),
        });
        assert_eq!(parsed.outputs[0].declared_marker_ident, "Vid\u{e9}oOut");
        assert_eq!(parsed.outputs[0].cast_name, "videoout");
    }

    #[test]
    fn a_port_name_casting_to_nothing_is_an_error() {
        let msg = parse_err(quote! {
            execution = manual,
            output("日本", family = Image),
        });
        assert!(msg.contains("cannot name anything"), "got: {msg}");
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
            input("video_in", family = Image, delivery_profile = "ordered"),
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
            input("video_in", family = Image),
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
            output("video", family = Image),
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
            input("audio", family = Channels, delivery_profile = "ordered", frobnicate = "yes"),
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
                family = Channels,
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
                family = Channels,
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
            input("audio", family = Channels, delivery_profile = "ordered", audio_window = match_device),
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
            input("video_in", family = Image, delivery_profile = "newest"),
            output("video_out", family = Image),
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
                family = Channels,
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
                family = Channels,
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
                family = Channels,
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
                family = Channels,
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
            input("audio", family = Channels, delivery_profile = "newest", audio_window = match_device),
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
                family = Channels,
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
                family = Channels,
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
                family = Channels,
                delivery_profile = "ordered",
                audio_window(sample_rate = 16_000, channels = 1, dtype = "i16", window_size = 512)
            ),
        });

        assert!(
            msg.contains("i16") && msg.contains("f32") && msg.contains("f64"),
            "the refusal must name the value and the legal ones; got {msg}"
        );
    }

    #[test]
    fn an_unknown_audio_window_field_is_refused_listing_the_five() {
        let msg = parse_err(quote! {
            execution = reactive,
            input(
                "audio",
                family = Channels,
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
            input("audio", family = Channels, delivery_profile = "ordered", audio_window = match_whatever),
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
            output("audio_out", family = Channels, audio_window = match_device),
        });

        assert!(
            msg.contains("consumer-side") && msg.contains("audio_out"),
            "the refusal must name the port and say whose setting it is; got {msg}"
        );
    }

    #[test]
    fn a_ports_family_and_its_keys_reach_the_schema() {
        let parsed = parse_ok(quote! {
            execution = manual,
            input("signals", delivery_profile = "ordered", family = Channels, requires = ["rpm"]),
            input("video", delivery_profile = "newest", family = Image, formats = ["rgba8_unorm"]),
            output(
                "telemetry",
                family = Channels,
                names = from_config("channel_names"),
                rate = 48000,
                dtype = "f32"
            ),
            output("packets", family = Packets, codec = "h264"),
            output("tensor", family = Array, names = ["boxes"], residency = "gpu"),
        });
        let schema = parsed.to_processor_schema();

        let signals = &schema.inputs[0].family;
        assert_eq!(signals.family, PortFamily::Channels);
        assert_eq!(signals.requires, vec!["rpm".to_string()]);
        assert_eq!(
            schema.inputs[1].family.formats,
            vec!["rgba8_unorm".to_string()]
        );
        let telemetry = &schema.outputs[0].family;
        assert_eq!(
            telemetry.names,
            Some(OutputPortDeclaredNames::FromConfig {
                config_key: "channel_names".into()
            })
        );
        assert_eq!(
            telemetry.rate,
            Some(ChannelsOutputPortSampleRate::Stated(48_000))
        );
        assert_eq!(telemetry.dtype, Some(ChannelsOutputPortSampleType::F32));
        assert_eq!(schema.outputs[1].family.codec.as_deref(), Some("h264"));
        assert_eq!(
            schema.outputs[2].family.residency,
            Some(ArrayOutputPortResidency::Gpu)
        );
    }

    #[test]
    fn a_family_key_the_family_does_not_take_is_refused_naming_the_port() {
        let msg = parse_err(quote! {
            execution = manual,
            output("video", family = Image, names = ["x"]),
        });
        assert!(
            msg.contains("\"video\"") && msg.contains("`names=`"),
            "got {msg}"
        );
    }

    #[test]
    fn a_port_without_a_family_is_refused_naming_the_families() {
        let msg = parse_err(quote! {
            execution = manual,
            output("video"),
        });
        assert!(
            msg.contains("port `video` declares no family") && msg.contains("`Records`"),
            "got {msg}"
        );
    }

    #[test]
    fn a_family_key_without_a_family_and_an_unknown_family_are_refused() {
        let msg = parse_err(quote! {
            execution = manual,
            output("video", formats = ["rgba8_unorm"]),
        });
        assert!(msg.contains("without `family = …`"), "got {msg}");
        let msg = parse_err(quote! {
            execution = manual,
            output("video", family = Picture),
        });
        assert!(
            msg.contains("Picture") && msg.contains("`Image`"),
            "got {msg}"
        );
    }

    #[test]
    fn an_image_outputs_frame_rate_reaches_the_schema_stated_or_from_config() {
        let parsed = parse_ok(quote! {
            execution = manual,
            output("video", family = Image, frame_rate = 30),
            output("preview", family = Image, frame_rate = from_config("frames_per_second")),
            output("bare", family = Image),
        });
        let schema = parsed.to_processor_schema();

        assert_eq!(
            schema.outputs[0].family.frame_rate,
            Some(ImageOutputPortFrameRate::Stated(
                streamlib_processor_schema::ImageStreamFrameRate {
                    numerator: 30,
                    denominator: 1
                }
            ))
        );
        assert_eq!(
            schema.outputs[1].family.frame_rate,
            Some(ImageOutputPortFrameRate::FromConfig {
                config_key: "frames_per_second".into()
            })
        );
        assert_eq!(schema.outputs[2].family.frame_rate, None);
    }

    #[test]
    fn a_catalog_key_is_refused_as_unknown() {
        let msg = parse_err(quote! {
            execution = manual,
            output("text", family = Image, catalog = TextEntry { format: "vtt" }),
        });

        assert!(msg.contains("unknown port key `catalog`"), "got {msg}");
    }

    #[test]
    fn a_second_audio_window_on_one_port_is_refused() {
        let msg = parse_err(quote! {
            execution = manual,
            input(
                "audio",
                family = Channels,
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
                family = Channels,
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
                family = Channels,
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
                input("audio", family = Channels, delivery_profile = "ordered", #declaration),
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
                family = Channels,
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
            input("video_in", family = Image, delivery_profile = "newest"),
        });

        let schema = parsed.to_processor_schema();
        let json = serde_json::to_value(&schema.inputs[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "video_in",
                "description": null,
                "delivery_profile": "newest",
                "family": {"family": "image"},
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
