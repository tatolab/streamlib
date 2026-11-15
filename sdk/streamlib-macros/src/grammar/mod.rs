// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
    AudioWindowContract, PortFamilyDeclaration, ProcessorPortSchema, ProcessorScheduling,
    ProcessorSchema, ProcessorSchemaExecution, RuntimeConfig, RuntimeOptions, ThreadPriority,
};
use syn::ext::IdentExt;
use syn::parse::{ParseStream, Parser};
use syn::spanned::Spanned;
use syn::{Attribute, Ident, LitInt, LitStr, Path, Token, parenthesized};

mod port_grammar;

use port_grammar::parse_port;

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
mod tests;
