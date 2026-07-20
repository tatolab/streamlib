// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grammar for the `#[processor(...)]` attribute — the single source of truth
//! for a processor's identity, execution mode, and ports.
//!
//! Nothing here reads any file. Everything the macro needs to emit the
//! processor module is declared in the attribute tokens:
//!
//! ```ignore
//! #[processor(
//!     "@tatolab/camera/Camera",         // identity, version-free (omit → @app/local/<StructName>)
//!     execution = manual,               // reactive | manual | continuous | continuous(interval_ms = 10)
//!     scheduling = high,                // realtime | high | normal (default: normal)
//!     unsafe_send,                      // flag — emit `unsafe impl Send`
//!     config = crate::CameraConfig,     // Rust type path for the typed Config alias
//!     input("video_in", "@tatolab/core/VideoFrame",
//!           read_mode = "skip_to_latest", buffer_size = 4, overflow = "drop_oldest"),
//!     output("video", "@tatolab/core/VideoFrame"),
//! )]
//! ```
//!
//! Every schema reference — the processor identity and each port schema — is
//! **version-free** (`@org/package/Type`, no `@version`): a schema ref is an
//! identity the runtime binds version-blind to whatever a node provides, and
//! versions are derived at package-build time, never hand-authored (#1409).
//! References are **resolve-free**: the attribute carries the `@org/package/Type`
//! verbatim, so the macro never walks the dependency graph. Deep schema
//! validation (does the referenced schema exist / stay compatible) is out of
//! scope here and handled at the runtime layer.

use streamlib_processor_schema::{
    Org, Package, PortSchemaSpec, ProcessorPortSchema, ProcessorSchema, ProcessorSchemaExecution,
    ProcessorScheduling, RuntimeConfig, RuntimeOptions, SchemaIdent, SemVer, ThreadPriority,
    TypeName,
};
use syn::ext::IdentExt;
use syn::parse::{ParseStream, Parser};
use syn::{Ident, LitInt, LitStr, Path, Token, parenthesized};

/// Which side of a link a port sits on. Producer-side policy keys
/// (`read_mode` / `overflow` / `buffer_size`) are consumer-side settings and
/// are only valid on an `input(...)`; the grammar rejects them on an
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
    pub schema: PortSchemaSpec,
    pub description: Option<String>,
    pub read_mode: Option<String>,
    pub overflow: Option<String>,
    pub buffer_size: Option<usize>,
}

/// The fully-parsed `#[processor(...)]` attribute.
pub struct ParsedProcessorAttr {
    pub ident: SchemaIdent,
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
    /// identity's `Type` segment, ports carry the resolve-free `Specific` idents
    /// the attribute declared, and the runtime language defaults to Rust (the
    /// only language a source scan of a Rust crate can produce). `config` stays
    /// `None`: the attribute binds a config *type*, not a resolved manifest
    /// schema; the consuming layer projects the config-schema id to a
    /// release-core catalog entry.
    pub fn to_processor_schema(&self) -> ProcessorSchema {
        let to_port = |p: &ParsedPort| ProcessorPortSchema {
            name: p.name.clone(),
            schema: p.schema.clone(),
            description: p.description.clone(),
            read_mode: p.read_mode.clone(),
            overflow: p.overflow.clone(),
            buffer_size: p.buffer_size,
        };

        ProcessorSchema {
            name: self.ident.r#type.as_str().to_string(),
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
/// [`crate::extract_rust_processors`] calls it with the tokens a `syn`-parsed
/// `#[processor(...)]` attribute carries. There is deliberately no second
/// parser — code is the
/// source of truth, so both readers of that truth share one grammar.
///
/// `struct_ident` provides the `Type` segment for the synthesized `@app/local`
/// identity when no identity string is declared.
pub fn parse2(
    attr: proc_macro2::TokenStream,
    struct_ident: &Ident,
) -> syn::Result<ParsedProcessorAttr> {
    let struct_name = struct_ident.to_string();
    let parser = move |input: ParseStream<'_>| parse_body(input, &struct_name);
    parser.parse2(attr)
}

fn parse_body(input: ParseStream<'_>, struct_name: &str) -> syn::Result<ParsedProcessorAttr> {
    let mut identity: Option<SchemaIdent> = None;
    let mut app_local_type: Option<(String, proc_macro2::Span)> = None;
    let mut description: Option<String> = None;
    let mut execution: Option<ProcessorSchemaExecution> = None;
    let mut scheduling: Option<ThreadPriority> = None;
    let mut unsafe_send = false;
    let mut config_type: Option<Path> = None;
    let mut config_field_name: Option<String> = None;
    let mut config_schema_id: Option<String> = None;
    let mut inputs: Vec<ParsedPort> = Vec::new();
    let mut outputs: Vec<ParsedPort> = Vec::new();

    // Optional leading positional identity string.
    if input.peek(LitStr) {
        let lit: LitStr = input.parse()?;
        identity = Some(parse_schema_ident_str(&lit.value(), lit.span())?);
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
    }

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
                input.parse::<Token![=]>()?;
                let lit: LitStr = input.parse()?;
                app_local_type = Some((lit.value(), lit.span()));
            }
            "input" => inputs.push(parse_port(input, PortDirection::Input)?),
            "output" => outputs.push(parse_port(input, PortDirection::Output)?),
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown `#[processor(...)]` key `{other}` — expected one of \
                         `execution`, `scheduling`, `unsafe_send`, `config`, `config_field`, \
                         `config_schema`, `description`, `type`, `input`, `output`"
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

    // Resolve identity: explicit id string, else synthesize @app/local.
    let ident = match identity {
        Some(id) => id,
        None => {
            let type_str = app_local_type
                .as_ref()
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| struct_name.to_string());
            let type_span = app_local_type
                .as_ref()
                .map(|(_, sp)| *sp)
                .unwrap_or_else(proc_macro2::Span::call_site);
            let type_name = TypeName::new(&type_str).map_err(|e| {
                syn::Error::new(
                    type_span,
                    format!(
                        "cannot synthesize `@app/local` identity: `{type_str}` is not a valid \
                         PascalCase TypeName ({e}). Declare an explicit identity string \
                         (`\"@org/package/Type\"`) or a valid `type = \"...\"`."
                    ),
                )
            })?;
            SchemaIdent::new(
                Org::new("app").expect("`app` is a valid org"),
                Package::new("local").expect("`local` is a valid package"),
                type_name,
                SemVer::new(0, 0, 0),
            )
        }
    };

    // Synthesize the descriptor config-schema id from the config type when the
    // author didn't spell one out: version-free `@<org>/<package>/<ConfigTypeName>`
    // (the runtime schema registry stores and looks up unversioned).
    if config_schema_id.is_none()
        && let Some(path) = &config_type
        && let Some(last) = path.segments.last()
    {
        config_schema_id = Some(format!(
            "@{}/{}/{}",
            ident.org.as_str(),
            ident.package.as_str(),
            last.ident,
        ));
    }

    let config_field_name = config_field_name.unwrap_or_else(|| "config".to_string());

    Ok(ParsedProcessorAttr {
        ident,
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
/// `<name-string>, <schema>, [read_mode = "...", overflow = "...", buffer_size = N,
/// description = "..."]` — where `<schema>` is either the bare identifier `any`
/// or a version-free `"@org/package/Type"` string.
///
/// The producer-side policy keys (`read_mode` / `overflow` / `buffer_size`) are
/// consumer-side settings the destination input port declares; they are
/// rejected with a spanned error on an `output(...)` rather than silently
/// dropped.
fn parse_port(input: ParseStream<'_>, direction: PortDirection) -> syn::Result<ParsedPort> {
    let content;
    parenthesized!(content in input);

    let name_lit: LitStr = content.parse()?;
    let name = name_lit.value();
    if name.is_empty() {
        return Err(syn::Error::new(name_lit.span(), "port name must not be empty"));
    }

    content.parse::<Token![,]>()?;
    let schema = parse_port_schema(&content)?;

    let mut description = None;
    let mut read_mode = None;
    let mut overflow = None;
    let mut buffer_size = None;

    while !content.is_empty() {
        content.parse::<Token![,]>()?;
        if content.is_empty() {
            break;
        }
        let key: Ident = content.parse()?;
        let key_span = key.span();
        content.parse::<Token![=]>()?;
        match key.to_string().as_str() {
            "description" => {
                let lit: LitStr = content.parse()?;
                description = Some(lit.value());
            }
            "read_mode" => {
                let lit: LitStr = content.parse()?;
                reject_producer_key_on_output(direction, "read_mode", &name, key_span)?;
                read_mode = Some(lit.value());
            }
            "overflow" => {
                let lit: LitStr = content.parse()?;
                reject_producer_key_on_output(direction, "overflow", &name, key_span)?;
                overflow = Some(lit.value());
            }
            "buffer_size" => {
                let lit: LitInt = content.parse()?;
                reject_producer_key_on_output(direction, "buffer_size", &name, key_span)?;
                buffer_size = Some(lit.base10_parse()?);
            }
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown port key `{other}` — expected `read_mode`, `overflow`, \
                         `buffer_size`, or `description`"
                    ),
                ));
            }
        }
    }

    Ok(ParsedPort {
        name,
        schema,
        description,
        read_mode,
        overflow,
        buffer_size,
    })
}

/// Reject a producer-side policy key on an `output(...)` with a spanned error.
/// A no-op on an `input(...)`.
fn reject_producer_key_on_output(
    direction: PortDirection,
    key: &str,
    port_name: &str,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    if direction == PortDirection::Output {
        return Err(syn::Error::new(
            span,
            format!(
                "`{key}` is a consumer-side policy key and is not valid on \
                 `{}(\"{port_name}\", ...)` — `read_mode`, `overflow`, and \
                 `buffer_size` are declared by the destination input port, not \
                 the producing output port",
                direction.keyword()
            ),
        ));
    }
    Ok(())
}

/// Parse a port schema reference: `any` (bare ident) or a version-free
/// `"@org/package/Type"` string literal.
fn parse_port_schema(content: ParseStream<'_>) -> syn::Result<PortSchemaSpec> {
    if content.peek(LitStr) {
        let lit: LitStr = content.parse()?;
        let ident = parse_schema_ident_str(&lit.value(), lit.span())?;
        Ok(PortSchemaSpec::Specific(ident))
    } else {
        let kw: Ident = content.parse()?;
        if kw == "any" {
            Ok(PortSchemaSpec::Any)
        } else {
            Err(syn::Error::new(
                kw.span(),
                format!(
                    "port schema must be `any` or a version-free \
                     `\"@org/package/Type\"` string; got `{kw}`"
                ),
            ))
        }
    }
}

/// Parse a **version-free** `@org/package/Type` into a validated
/// [`SchemaIdent`].
///
/// The attribute grammar is version-free (#1409): a schema ref is an identity
/// the runtime binds version-blind, and versions are derived at package-build
/// time — never hand-authored. A trailing `@<version>` is rejected. The
/// synthesized `SchemaIdent` carries the `0.0.0` version-free sentinel — the
/// same placeholder `ProcessorTypeReference::ResolveToInstalled` renders and
/// the runtime schema registry (which stores/looks up unversioned) ignores.
pub fn parse_schema_ident_str(raw: &str, span: proc_macro2::Span) -> syn::Result<SchemaIdent> {
    let err = |msg: String| syn::Error::new(span, msg);

    let stripped = raw.strip_prefix('@').ok_or_else(|| {
        err(format!(
            "schema identity `{raw}` must start with `@` (e.g. `@tatolab/core/VideoFrame`)"
        ))
    })?;
    if stripped.contains('@') {
        return Err(err(format!(
            "schema identity `{raw}` must be version-free `@<org>/<package>/<Type>` \
             with no `@<version>` — a schema ref is an identity the runtime binds \
             version-blind; versions are derived at package-build time, never \
             hand-authored (#1409)"
        )));
    }
    let segments: Vec<&str> = stripped.split('/').collect();
    if segments.len() != 3 {
        return Err(err(format!(
            "schema identity `{raw}` must be `@<org>/<package>/<Type>` \
             (exactly three `/`-separated segments)"
        )));
    }
    let org = Org::new(segments[0]).map_err(|e| err(format!("invalid org in `{raw}`: {e}")))?;
    let package =
        Package::new(segments[1]).map_err(|e| err(format!("invalid package in `{raw}`: {e}")))?;
    let type_name =
        TypeName::new(segments[2]).map_err(|e| err(format!("invalid type in `{raw}`: {e}")))?;

    Ok(SchemaIdent::new(org, package, type_name, SemVer::new(0, 0, 0)))
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
    fn full_identity_execution_and_ports() {
        let parsed = parse_ok(quote! {
            "@tatolab/camera/Camera",
            execution = manual,
            scheduling = high,
            input("video_in", "@tatolab/core/VideoFrame", read_mode = "skip_to_latest", buffer_size = 4),
            output("video", "@tatolab/core/VideoFrame"),
        });
        assert_eq!(parsed.ident.org.as_str(), "tatolab");
        assert_eq!(parsed.ident.package.as_str(), "camera");
        assert_eq!(parsed.ident.r#type.as_str(), "Camera");
        // Version-free identity synthesizes the 0.0.0 version-free sentinel.
        assert_eq!(parsed.ident.version, SemVer::new(0, 0, 0));
        assert_eq!(parsed.execution, ProcessorSchemaExecution::Manual);
        assert_eq!(parsed.scheduling, Some(ThreadPriority::High));
        assert_eq!(parsed.inputs.len(), 1);
        assert_eq!(parsed.inputs[0].name, "video_in");
        assert_eq!(parsed.inputs[0].read_mode.as_deref(), Some("skip_to_latest"));
        assert_eq!(parsed.inputs[0].buffer_size, Some(4));
        assert!(matches!(
            parsed.inputs[0].schema,
            PortSchemaSpec::Specific(_)
        ));
        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.outputs[0].name, "video");
        // Output ports never carry a producer-side policy.
        assert_eq!(parsed.outputs[0].read_mode, None);
    }

    #[test]
    fn processor_and_port_descriptions_parse() {
        // The descriptor's introspection description surface (#1409): both the
        // processor description and each port description are carried by the
        // attribute and reach the ParsedProcessorAttr.
        let parsed = parse_ok(quote! {
            "@tatolab/camera/Camera",
            description = "Captures video from cameras",
            execution = manual,
            input("video_in", "@tatolab/core/VideoFrame", description = "Frames to convert"),
            output("video", "@tatolab/core/VideoFrame", description = "Live video frames"),
        });
        assert_eq!(parsed.description.as_deref(), Some("Captures video from cameras"));
        assert_eq!(parsed.inputs[0].description.as_deref(), Some("Frames to convert"));
        assert_eq!(parsed.outputs[0].description.as_deref(), Some("Live video frames"));
    }

    #[test]
    fn continuous_with_interval() {
        let parsed = parse_ok(quote! {
            "@tatolab/audio/ChordGenerator",
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
            "@tatolab/audio/ChordGenerator",
            execution = continuous,
        });
        assert_eq!(
            parsed.execution,
            ProcessorSchemaExecution::Continuous { interval_ms: 0 }
        );
    }

    #[test]
    fn any_port_schema() {
        let parsed = parse_ok(quote! {
            "@tatolab/testing/Mock",
            execution = manual,
            input("in1", any),
            output("out1", any),
        });
        assert!(matches!(parsed.inputs[0].schema, PortSchemaSpec::Any));
        assert!(matches!(parsed.outputs[0].schema, PortSchemaSpec::Any));
    }

    #[test]
    fn config_type_and_synthesized_schema_id() {
        let parsed = parse_ok(quote! {
            "@tatolab/camera/Camera",
            execution = manual,
            config = crate::_generated_::tatolab__camera::CameraConfig,
        });
        assert!(parsed.config_type.is_some());
        assert_eq!(parsed.config_field_name, "config");
        // The synthesized config-schema id is version-free.
        assert_eq!(
            parsed.config_schema_id.as_deref(),
            Some("@tatolab/camera/CameraConfig")
        );
    }

    #[test]
    fn explicit_config_schema_overrides_synthesis() {
        let parsed = parse_ok(quote! {
            "@tatolab/audio/BufferRechunker",
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
            "@tatolab/testing/Mock",
            execution = manual,
        });
        assert!(parsed.config_type.is_none());
        assert!(parsed.config_schema_id.is_none());
    }

    #[test]
    fn app_local_synthesis_from_struct_name() {
        // No identity string, no `type` — synthesize @app/local/<StructName>.
        let parsed = parse2(
            quote! { execution = reactive },
            &ident("MyLocalProcessor"),
        )
        .expect("bare app-local processor should parse");
        assert_eq!(parsed.ident.org.as_str(), "app");
        assert_eq!(parsed.ident.package.as_str(), "local");
        assert_eq!(parsed.ident.r#type.as_str(), "MyLocalProcessor");
        assert_eq!(parsed.ident.version, SemVer::new(0, 0, 0));
    }

    #[test]
    fn app_local_type_override() {
        let parsed = parse2(
            quote! { execution = reactive, type = "CustomName" },
            &ident("StructIdent"),
        )
        .expect("app-local with type override should parse");
        assert_eq!(parsed.ident.r#type.as_str(), "CustomName");
        assert_eq!(parsed.ident.org.as_str(), "app");
    }

    #[test]
    fn unsafe_send_flag() {
        let parsed = parse_ok(quote! {
            "@tatolab/camera/Camera",
            execution = manual,
            unsafe_send,
        });
        assert!(parsed.unsafe_send);
    }

    // ---- error cases ----

    #[test]
    fn missing_execution_is_an_error() {
        let msg = parse_err(quote! { "@tatolab/camera/Camera" });
        assert!(msg.contains("missing required `execution`"), "got: {msg}");
    }

    #[test]
    fn duplicate_input_port_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/testing/Mock",
            execution = manual,
            input("dup", any),
            input("dup", any),
        });
        assert!(msg.contains("duplicate input port name `dup`"), "got: {msg}");
    }

    #[test]
    fn duplicate_output_port_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/testing/Mock",
            execution = manual,
            output("dup", any),
            output("dup", any),
        });
        assert!(msg.contains("duplicate output port name `dup`"), "got: {msg}");
    }

    #[test]
    fn output_producer_side_policy_keys_are_rejected() {
        // Regression: producer-side policy keys on an `output(...)` were
        // silently parsed-then-nulled. They must now be a spanned error.
        // Mentally revert `reject_producer_key_on_output` and each of these
        // parses cleanly (bug) instead of erroring.
        for key in ["overflow", "read_mode", "buffer_size"] {
            let value = if key == "buffer_size" { "4" } else { "\"drop_oldest\"" };
            let tokens: proc_macro2::TokenStream = format!(
                "\"@tatolab/camera/Camera\", execution = manual, \
                 output(\"video\", \"@tatolab/core/VideoFrame\", {key} = {value})"
            )
            .parse()
            .expect("token stream parses");
            let msg = parse_err(tokens);
            assert!(
                msg.contains(&format!("`{key}` is a consumer-side policy key")),
                "key `{key}` got: {msg}"
            );
        }
    }

    #[test]
    fn input_producer_side_policy_keys_are_accepted() {
        // The mirror of the rejection test: the same keys stay valid on an
        // `input(...)` and reach the parsed port.
        let parsed = parse_ok(quote! {
            "@tatolab/camera/Camera",
            execution = manual,
            input("video_in", "@tatolab/core/VideoFrame",
                  read_mode = "skip_to_latest", overflow = "drop_oldest", buffer_size = 4),
        });
        assert_eq!(parsed.inputs[0].read_mode.as_deref(), Some("skip_to_latest"));
        assert_eq!(parsed.inputs[0].overflow.as_deref(), Some("drop_oldest"));
        assert_eq!(parsed.inputs[0].buffer_size, Some(4));
    }

    #[test]
    fn unknown_key_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/testing/Mock",
            execution = manual,
            frobnicate = "yes",
        });
        assert!(msg.contains("unknown `#[processor(...)]` key `frobnicate`"), "got: {msg}");
    }

    #[test]
    fn unknown_execution_mode_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/testing/Mock",
            execution = sideways,
        });
        assert!(msg.contains("unknown execution mode `sideways`"), "got: {msg}");
    }

    #[test]
    fn malformed_identity_is_an_error() {
        let msg = parse_err(quote! {
            "tatolab/camera/Camera",
            execution = manual,
        });
        assert!(msg.contains("must start with `@`"), "got: {msg}");
    }

    #[test]
    fn versioned_identity_is_rejected() {
        // The grammar is version-free (#1409): a hand-authored `@<version>` on
        // the identity is rejected. Mentally revert the version-free
        // `parse_schema_ident_str` and this passes when it must fail.
        let msg = parse_err(quote! {
            "@tatolab/camera/Camera@1.0.0",
            execution = manual,
        });
        assert!(msg.contains("must be version-free"), "got: {msg}");
    }

    #[test]
    fn versioned_port_schema_is_rejected() {
        // A hand-authored `@<version>` on a port schema ref is rejected too.
        let msg = parse_err(quote! {
            "@tatolab/camera/Camera",
            execution = manual,
            output("video", "@tatolab/core/VideoFrame@1.0.0"),
        });
        assert!(msg.contains("must be version-free"), "got: {msg}");
    }

    #[test]
    fn identity_wrong_segment_count_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/Camera",
            execution = manual,
        });
        assert!(msg.contains("three `/`-separated segments"), "got: {msg}");
    }

    #[test]
    fn port_schema_bad_ident_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/testing/Mock",
            execution = manual,
            input("in1", something_else),
        });
        assert!(msg.contains("port schema must be `any`"), "got: {msg}");
    }

    #[test]
    fn continuous_unknown_key_is_an_error() {
        let msg = parse_err(quote! {
            "@tatolab/camera/Camera",
            execution = continuous(period = 5),
        });
        assert!(msg.contains("expected `interval_ms`"), "got: {msg}");
    }

    #[test]
    fn schema_from_parsed_uses_named_free_specific_ports() {
        // Regression: a resolve-free grammar must still produce Specific idents
        // for the runtime (Named panics at iceoryx2 service open).
        let parsed = parse_ok(quote! {
            "@tatolab/camera/Camera",
            execution = manual,
            output("video", "@tatolab/core/VideoFrame"),
        });
        let PortSchemaSpec::Specific(id) = &parsed.outputs[0].schema else {
            panic!("expected Specific port schema");
        };
        assert_eq!(id.org.as_str(), "tatolab");
        assert_eq!(id.package.as_str(), "core");
        assert_eq!(id.r#type.as_str(), "VideoFrame");
    }

    #[test]
    fn quote_placeholder_keeps_quote_in_scope() {
        // Guards the test-only `quote` import stays wired.
        let _ = quote! {};
    }
}
