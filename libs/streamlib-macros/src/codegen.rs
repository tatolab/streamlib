// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Code generation for processor attribute macro
//!
//! Generates module wrapper with:
//! - `Processor` struct with public fields
//! - `InputLink` module with port markers
//! - `OutputLink` module with port markers
//! - Processor trait implementation

#[allow(unused_imports)]
use crate::analysis::AnalysisResult;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use streamlib_processor_schema::{PortSchemaSpec, ProcessorSchema, SchemaIdent};
use syn::ItemStruct;

/// Emit a `SchemaIdent` literal expression. Inputs are pre-validated by the
/// manifest parser so the `expect("validated")` calls are infallible.
fn schema_ident_tokens(ident: &SchemaIdent) -> TokenStream {
    let org = ident.org.as_str();
    let pkg = ident.package.as_str();
    let ty = ident.r#type.as_str();
    let major = ident.version.major;
    let minor = ident.version.minor;
    let patch = ident.version.patch;
    quote! {
        ::streamlib::sdk::descriptors::SchemaIdent::new(
            ::streamlib::sdk::descriptors::Org::new(#org).expect("validated by manifest parser"),
            ::streamlib::sdk::descriptors::Package::new(#pkg).expect("validated by manifest parser"),
            ::streamlib::sdk::descriptors::TypeName::new(#ty).expect("validated by manifest parser"),
            ::streamlib::sdk::descriptors::SemVer::new(#major, #minor, #patch),
        )
    }
}

/// Emit a `PortSchemaSpec` literal expression.
///
/// `PortSchemaSpec::Named` should never reach this function — the macro's
/// `load_processor_schema` pre-resolves every `Named` reference against
/// the manifest's `schemas:` map (#767) before handing the schema to
/// codegen. A `Named` here means the resolution pass was skipped or
/// returned an unresolved spec, which is a macro implementation bug.
fn port_schema_spec_tokens(spec: &PortSchemaSpec) -> TokenStream {
    match spec {
        PortSchemaSpec::Any => quote! { ::streamlib::sdk::processors::PortSchemaSpec::Any },
        PortSchemaSpec::Specific(ident) => {
            let inner = schema_ident_tokens(ident);
            quote! { ::streamlib::sdk::processors::PortSchemaSpec::Specific(#inner) }
        }
        PortSchemaSpec::Named(name) => {
            let msg = format!(
                "internal error: PortSchemaSpec::Named(`{}`) reached codegen — \
                 macro should have resolved this against the manifest's `schemas:` map",
                name.as_str()
            );
            quote! { compile_error!(#msg) }
        }
    }
}

// ============================================================================
// YAML-based code generation
// ============================================================================

/// Generate a processor module from a YAML ProcessorSchema and the resolved
/// structured [`SchemaIdent`] (org/package/type/version composed from the
/// enclosing `streamlib.yaml`'s `package:` block + processor short name).
///
/// `config_schema_id` is the canonical id string for the processor's
/// config schema, pre-resolved by the macro entrypoint by walking the
/// manifest's `schemas:` map (#767). `None` when the processor declares
/// no config block. The string is one of two grammars (handled by
/// [`derive_config_type_from_schema`]): new-shape
/// `@<org>/<package>/<TypeName>@<version>` or legacy reverse-DNS
/// `<segments>.config@<version>`.
pub fn generate_from_processor_schema(
    item: &ItemStruct,
    schema: &ProcessorSchema,
    schema_ident: &SchemaIdent,
    config_schema_id: Option<&str>,
    no_inventory: bool,
) -> TokenStream {
    let module_name = &item.ident;

    // Derive config type from schema reference if present
    let config_type = schema
        .config
        .as_ref()
        .map(|_| {
            // The bare-name TypeName from the manifest was resolved at
            // macro entry; always pass the canonical id string here.
            let id = config_schema_id.unwrap_or_else(|| {
                // schema.config.is_some() implies the macro entry
                // resolved a canonical id and supplied it. Reaching here
                // is an internal bug in the macro flow.
                panic!(
                    "internal error: ProcessorSchema declares config but no \
                     resolved canonical id was supplied to codegen"
                )
            });
            derive_config_type_from_schema(id)
        })
        .unwrap_or_else(|| quote! { ::streamlib::sdk::processors::EmptyConfig });

    let config_field_name = schema
        .config
        .as_ref()
        .map(|c| Ident::new(&c.name, Span::call_site()));

    // Extract custom fields from the user's struct
    let custom_fields = extract_custom_fields(item);

    let processor_struct =
        generate_processor_struct_from_schema(schema, &config_field_name, &custom_fields);
    let input_link_module = generate_input_link_module_from_schema(schema);
    let output_link_module = generate_output_link_module_from_schema(schema);
    let processor_impl = generate_processor_impl_from_schema(
        schema,
        schema_ident,
        &config_type,
        &config_field_name,
        &custom_fields,
        config_schema_id,
    );

    let schema_ident_const = quote! {
        /// Structured wire identity for this processor —
        /// `@<org>/<package>/<Type>@<version>` resolved at codegen
        /// from sibling `streamlib.yaml`'s `package:` block plus
        /// the processor's PascalCase short name.
        #[allow(dead_code)]
        pub fn schema_ident() -> ::streamlib::sdk::descriptors::SchemaIdent {
            Processor::schema_ident()
        }
    };

    // Generate unsafe Send impl if required (for !Send types like AVFoundation)
    let unsafe_send_impl = if schema.runtime.options.unsafe_send {
        quote! {
            // SAFETY: This processor contains !Send types (e.g., AVFoundation objects)
            // but is safe to send because these types are only accessed from a single
            // thread after initialization. The processor lifecycle ensures thread safety.
            unsafe impl Send for Processor {}
        }
    } else {
        quote! {}
    };

    // Auto-registration via inventory crate. Suppressed when the
    // processor attribute carries `no_inventory` — used by test-only
    // mocks and any future processor that wants the consumer to
    // register explicitly (via load_project / load_package or a direct
    // PROCESSOR_REGISTRY.register::<P>() call) rather than relying on
    // link-time auto-discovery.
    let inventory_submit = if no_inventory {
        quote! {}
    } else {
        quote! {
            ::streamlib::sdk::inventory::submit! {
                ::streamlib::sdk::processors::macro_codegen::FactoryRegistration {
                    register_fn: |factory| factory.register::<Processor>(),
                }
            }
        }
    };

    quote! {
        #[allow(non_snake_case)]
        pub mod #module_name {
            use super::*;

            /// Configuration type for this processor.
            pub type Config = #config_type;

            #schema_ident_const

            /// Create a [`ProcessorSpec`] for adding this processor to a runtime.
            ///
            /// Convenience wrapper around [`Processor::node`].
            pub fn node(config: Config) -> ::streamlib::sdk::processors::ProcessorSpec {
                Processor::node(config)
            }

            #processor_struct

            #unsafe_send_impl

            #input_link_module

            #output_link_module

            #processor_impl

            #inventory_submit
        }
    }
}

/// Derive the path to a Rust config type from a schema reference.
///
/// Two grammars are supported, and the emitted path differs between them
/// **on purpose**:
///
/// - New-shape `@<org>/<package>/<TypeName>@<version>` (e.g.
///   `@tatolab/audio/AudioMixerConfig@1.0.0`) emits the package-qualified
///   path `crate::_generated_::<org>__<package>::<TypeName>` (e.g.
///   `crate::_generated_::tatolab__audio::AudioMixerConfig`). The qualifier
///   prevents two carve-out packages from colliding when they declare
///   same-named types: `crate::_generated_::tatolab__audio::Strategy` and
///   `crate::_generated_::tatolab__camera::Strategy` are distinct paths,
///   not a Rust E0252 ambiguity.
/// - Legacy reverse-DNS `com.<org>.<processor>.config@<version>` (e.g.
///   `com.tatolab.buffer_rechunker.config@1.0.0`) emits the unqualified
///   path `crate::_generated_::<TypeName>Config`. Legacy schemas land at
///   the `_generated_/` root by codegen convention (the reverse-DNS
///   filename already encodes org/processor — `com_streamlib_h265_*` vs
///   `com_tatolab_screen_capture_*` — so collisions are filename-prevented at the
///   codegen layer).
///
/// Defensive shape, not future-proofing: the qualified path makes
/// cross-package type collisions a compile error rather than a
/// codegen-time `pub use` ambiguity dependent on no two packages happening
/// to choose the same short type name. CLAUDE.md "type-system enforcement
/// beats convention" — this is the engine-grade variant of that rule.
///
/// The actual type must be defined by codegen at the emitted path; the
/// `_generated_/` tree's `pub mod tatolab__<package>;` declaration plus the
/// per-package `pub use <snake_case>::<TypeName>;` inside that submodule
/// resolves the path.
fn derive_config_type_from_schema(schema_ref: &str) -> TokenStream {
    if let Some(rest) = schema_ref.strip_prefix('@') {
        // New-shape grammar: <org>/<package>/<TypeName>[@<version>].
        let ident_part = rest.split('@').next().unwrap_or(rest);
        let segments: Vec<&str> = ident_part.split('/').collect();

        // A well-formed new-shape schema has exactly three `/`-separated
        // segments: org, package, TypeName. Anything shorter is a bug in
        // the manifest parser (which validates the grammar before this
        // macro runs); fall back to the unqualified path so the user sees
        // a clear "type not found" error rather than a confusing path
        // emission failure.
        if segments.len() == 3 {
            let org = segments[0];
            let package = segments[1];
            let type_name = segments[2];
            // Package grammar (`[a-z][a-z0-9-]*`) permits hyphens, but Rust
            // module identifiers don't. The codegen-side `_generated_/`
            // tree maps `api-server` → `api_server` (snake_case mod name);
            // mirror that here so `crate::_generated_::tatolab__api_server`
            // resolves.
            let package_ident_form = package.replace('-', "_");
            let module_ident = Ident::new(
                &format!("{}__{}", org, package_ident_form),
                Span::call_site(),
            );
            let type_ident = Ident::new(type_name, Span::call_site());
            quote! { crate::_generated_::#module_ident::#type_ident }
        } else {
            let fallback = segments.last().copied().unwrap_or("Unknown");
            let ident = Ident::new(fallback, Span::call_site());
            quote! { crate::_generated_::#ident }
        }
    } else {
        // Legacy reverse-DNS grammar: <segments>.config[@<version>].
        // Filename convention encodes org/processor; collisions are
        // prevented at the codegen-output layer. Emit unqualified path
        // for backward compatibility with the legacy `_generated_/mod.rs`
        // top-level re-export shape.
        let name_part = schema_ref.split('@').next().unwrap_or(schema_ref);
        let segments: Vec<&str> = name_part.split('.').collect();

        let processor_segment = if segments.len() >= 2 {
            let last = segments[segments.len() - 1];
            if last == "config" {
                segments[segments.len() - 2]
            } else {
                last
            }
        } else {
            segments.last().copied().unwrap_or("Unknown")
        };

        // e.g. "buffer_rechunker" -> "BufferRechunkerConfig"
        let pascal_name = format!("{}Config", to_pascal_case(processor_segment));
        let ident = Ident::new(&pascal_name, Span::call_site());
        quote! { crate::_generated_::#ident }
    }
}

#[cfg(test)]
mod derive_config_type_tests {
    use super::*;

    fn render(schema_ref: &str) -> String {
        derive_config_type_from_schema(schema_ref).to_string()
    }

    #[test]
    fn new_shape_emits_package_qualified_path() {
        // The defensive shape: package-qualified path means two carve-outs
        // declaring the same short type name compile to distinct types.
        assert_eq!(
            render("@tatolab/audio/AudioMixerConfig@1.0.0"),
            "crate :: _generated_ :: tatolab__audio :: AudioMixerConfig",
        );
        assert_eq!(
            render("@tatolab/camera/CameraConfig@1.0.0"),
            "crate :: _generated_ :: tatolab__camera :: CameraConfig",
        );
    }

    #[test]
    fn new_shape_qualifier_disambiguates_same_named_types() {
        // Hypothetical collision: two packages each ship a `Strategy` enum.
        // Without the package qualifier, the macro would emit
        // `crate::_generated_::Strategy` for both — `_generated_/mod.rs`
        // would `pub use ... ::Strategy;` twice and the codegen output
        // would fail with E0252. With the qualifier each path is distinct.
        let audio = render("@tatolab/audio/Strategy@1.0.0");
        let camera = render("@tatolab/camera/Strategy@1.0.0");
        assert_ne!(audio, camera);
        assert!(audio.ends_with("tatolab__audio :: Strategy"));
        assert!(camera.ends_with("tatolab__camera :: Strategy"));
    }

    #[test]
    fn legacy_reverse_dns_emits_unqualified_path() {
        // Legacy filenames already encode org/processor (com_tatolab_*,
        // com_streamlib_*); the unqualified path is the established shape
        // and the legacy schemas live at the `_generated_/` root.
        assert_eq!(
            render("com.tatolab.buffer_rechunker.config@1.0.0"),
            "crate :: _generated_ :: BufferRechunkerConfig",
        );
        assert_eq!(
            render("com.tatolab.jtd_codegen_fixture_a.config@1.0.0"),
            "crate :: _generated_ :: JtdCodegenFixtureAConfig",
        );
    }

    #[test]
    fn new_shape_sanitizes_hyphenated_package_name() {
        // Package grammar allows hyphens (`api-server`), Rust module
        // identifiers do not. The macro emits the snake_case form so
        // `crate::_generated_::tatolab__api_server` resolves against the
        // codegen-side `pub mod tatolab__api_server;`.
        assert_eq!(
            render("@tatolab/api-server/ApiServerConfig@1.0.0"),
            "crate :: _generated_ :: tatolab__api_server :: ApiServerConfig",
        );
    }

    #[test]
    fn malformed_new_shape_falls_back_without_panicking() {
        // A new-shape string missing org or package doesn't panic; it
        // falls back to the unqualified path so the user gets a clear
        // "type not found" compile error instead of a macro panic.
        let result = render("@tatolab/AudioMixerConfig@1.0.0");
        assert!(result.contains("AudioMixerConfig"));
    }
}

/// Convert a string to PascalCase.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Custom field extracted from the user's struct definition.
struct CustomField {
    name: Ident,
    ty: syn::Type,
}

/// Extract custom fields from the user's struct definition.
fn extract_custom_fields(item: &ItemStruct) -> Vec<CustomField> {
    match &item.fields {
        syn::Fields::Named(fields) => fields
            .named
            .iter()
            .map(|f| CustomField {
                name: f.ident.clone().expect("Named field must have ident"),
                ty: f.ty.clone(),
            })
            .collect(),
        syn::Fields::Unit => Vec::new(),
        syn::Fields::Unnamed(_) => Vec::new(),
    }
}

/// Generate the Processor struct from schema.
fn generate_processor_struct_from_schema(
    schema: &ProcessorSchema,
    config_field_name: &Option<Ident>,
    custom_fields: &[CustomField],
) -> TokenStream {
    let config_field = config_field_name.as_ref().map(|name| {
        quote! { pub #name: Config, }
    });

    // Generate iceoryx2-based IPC fields if ports are defined
    let ipc_input_field = if !schema.inputs.is_empty() {
        quote! { pub inputs: ::streamlib::sdk::iceoryx2::InputMailboxes, }
    } else {
        quote! {}
    };

    let ipc_output_field = if !schema.outputs.is_empty() {
        quote! { pub outputs: ::std::sync::Arc<::streamlib::sdk::iceoryx2::OutputWriter>, }
    } else {
        quote! {}
    };

    // Generate custom fields from the user's struct definition
    let custom_field_defs: Vec<TokenStream> = custom_fields
        .iter()
        .map(|f| {
            let name = &f.name;
            let ty = &f.ty;
            quote! { pub #name: #ty, }
        })
        .collect();

    quote! {
        pub struct Processor {
            #ipc_input_field
            #ipc_output_field
            #config_field
            #(#custom_field_defs)*
        }
    }
}

/// Generate InputLink module from schema.
fn generate_input_link_module_from_schema(schema: &ProcessorSchema) -> TokenStream {
    let port_markers: Vec<TokenStream> = schema
        .inputs
        .iter()
        .map(|port| {
            let port_name = Ident::new(&port.name, proc_macro2::Span::call_site());
            quote! {
                pub struct #port_name;
                impl ::streamlib::sdk::processors::InputPortMarker for #port_name {
                    const PORT_NAME: &'static str = stringify!(#port_name);
                    type Processor = super::Processor;
                }
            }
        })
        .collect();

    quote! {
        pub mod InputLink {
            #(#port_markers)*
        }
    }
}

/// Generate OutputLink module from schema.
fn generate_output_link_module_from_schema(schema: &ProcessorSchema) -> TokenStream {
    let port_markers: Vec<TokenStream> = schema
        .outputs
        .iter()
        .map(|port| {
            let port_name = Ident::new(&port.name, proc_macro2::Span::call_site());
            quote! {
                pub struct #port_name;
                impl ::streamlib::sdk::processors::OutputPortMarker for #port_name {
                    const PORT_NAME: &'static str = stringify!(#port_name);
                    type Processor = super::Processor;
                }
            }
        })
        .collect();

    quote! {
        pub mod OutputLink {
            #(#port_markers)*
        }
    }
}

/// Generate Processor trait implementation from schema.
fn generate_processor_impl_from_schema(
    schema: &ProcessorSchema,
    schema_ident: &SchemaIdent,
    config_type: &TokenStream,
    config_field_name: &Option<Ident>,
    custom_fields: &[CustomField],
    config_schema_id: Option<&str>,
) -> TokenStream {
    use streamlib_processor_schema::ProcessorSchemaExecution;

    let processor_name = &schema.name;
    let description = schema.description.as_deref().unwrap_or("Processor");
    let version = &schema.version;
    let schema_ident_literal = schema_ident_tokens(schema_ident);

    // Derive execution mode from schema
    let (
        execution_variant,
        execution_description,
        processor_trait,
        process_impl,
        start_impl,
        stop_impl,
    ) = match &schema.execution {
        ProcessorSchemaExecution::Reactive => (
            quote! { ::streamlib::sdk::execution::ProcessExecution::Reactive },
            "Reactive",
            quote! { ::streamlib::sdk::processors::ReactiveProcessor },
            quote! {
                <Self as ::streamlib::sdk::processors::ReactiveProcessor>::process(self, ctx)
            },
            quote! {
                Err(::streamlib::sdk::error::Error::Runtime(
                    "start() is only valid for Manual execution mode.".into()
                ))
            },
            quote! {
                Err(::streamlib::sdk::error::Error::Runtime(
                    "stop() is only valid for Manual execution mode.".into()
                ))
            },
        ),
        ProcessorSchemaExecution::Manual => (
            quote! { ::streamlib::sdk::execution::ProcessExecution::Manual },
            "Manual",
            quote! { ::streamlib::sdk::processors::ManualProcessor },
            quote! {
                let _ = ctx;
                Err(::streamlib::sdk::error::Error::Runtime(
                    "process() is only valid for Reactive/Continuous execution modes.".into()
                ))
            },
            quote! {
                <Self as ::streamlib::sdk::processors::ManualProcessor>::start(self, ctx)
            },
            quote! {
                <Self as ::streamlib::sdk::processors::ManualProcessor>::stop(self, ctx)
            },
        ),
        ProcessorSchemaExecution::Continuous { interval_ms } => {
            let interval = *interval_ms;
            (
                quote! { ::streamlib::sdk::execution::ProcessExecution::Continuous { interval_ms: #interval } },
                "Continuous",
                quote! { ::streamlib::sdk::processors::ContinuousProcessor },
                quote! {
                    <Self as ::streamlib::sdk::processors::ContinuousProcessor>::process(self, ctx)
                },
                quote! {
                    Err(::streamlib::sdk::error::Error::Runtime(
                        "start() is only valid for Manual execution mode.".into()
                    ))
                },
                quote! {
                    Err(::streamlib::sdk::error::Error::Runtime(
                        "stop() is only valid for Manual execution mode.".into()
                    ))
                },
            )
        }
    };

    let from_config_body =
        generate_from_config_from_schema(schema, config_field_name, custom_fields);
    let descriptor_impl =
        generate_descriptor_from_schema(schema, description, version, config_schema_id);
    let iceoryx2_accessors = generate_iceoryx2_accessors_from_schema(schema);

    let update_config = config_field_name.as_ref().map(|name| {
        quote! {
            fn update_config(&mut self, config: Self::Config) -> ::streamlib::sdk::error::Result<()> {
                self.#name = config;
                Ok(())
            }
        }
    });

    quote! {
        impl Processor {
            /// Processor PascalCase short name (the `type` segment of the
            /// structured [`SchemaIdent`](::streamlib::sdk::descriptors::SchemaIdent)).
            /// Use [`Processor::schema_ident`] for the full structured identity.
            pub const NAME: &'static str = #processor_name;

            /// Returns the structured wire identity for this processor —
            /// `@<org>/<package>/<Type>@<version>` resolved at codegen
            /// time from the sibling `streamlib.yaml`'s `package:` block
            /// plus the processor's PascalCase short name.
            pub fn schema_ident() -> ::streamlib::sdk::descriptors::SchemaIdent {
                #schema_ident_literal
            }

            /// Create a [`ProcessorSpec`](::streamlib::sdk::processors::ProcessorSpec)
            /// for adding this processor to a runtime.
            pub fn node(config: #config_type) -> ::streamlib::sdk::processors::ProcessorSpec {
                ::streamlib::sdk::processors::ProcessorSpec {
                    name: Self::schema_ident(),
                    config: ::streamlib::sdk::serde_json::to_value(&config)
                        .expect("Config serialization failed"),
                    display_name: None,
                }
            }

            /// Returns the execution mode for this processor.
            pub fn execution_mode(&self) -> ::streamlib::sdk::execution::ProcessExecution {
                #execution_variant
            }

            /// Returns a human-readable description of the execution mode.
            pub fn execution_mode_description(&self) -> &'static str {
                #execution_description
            }
        }

        impl ::streamlib::sdk::processors::__generated_private::GeneratedProcessor for Processor {
            type Config = #config_type;

            fn name(&self) -> &str {
                Self::NAME
            }

            #from_config_body

            fn process(&mut self, ctx: &::streamlib::sdk::context::RuntimeContextLimitedAccess<'_>) -> ::streamlib::sdk::error::Result<()> {
                #process_impl
            }

            fn start(&mut self, ctx: &::streamlib::sdk::context::RuntimeContextFullAccess<'_>) -> ::streamlib::sdk::error::Result<()> {
                let _ = ctx;
                #start_impl
            }

            fn stop(&mut self, ctx: &::streamlib::sdk::context::RuntimeContextFullAccess<'_>) -> ::streamlib::sdk::error::Result<()> {
                let _ = ctx;
                #stop_impl
            }

            #update_config

            fn execution_config(&self) -> ::streamlib::sdk::execution::ExecutionConfig {
                ::streamlib::sdk::execution::ExecutionConfig {
                    execution: #execution_variant,
                }
            }

            #descriptor_impl
            #iceoryx2_accessors

            fn __generated_setup(
                &mut self,
                ctx: &::streamlib::sdk::context::RuntimeContextFullAccess<'_>,
            ) -> impl ::std::future::Future<Output = ::streamlib::sdk::error::Result<()>> + Send {
                <Self as #processor_trait>::setup(self, ctx)
            }

            fn __generated_teardown(
                &mut self,
                ctx: &::streamlib::sdk::context::RuntimeContextFullAccess<'_>,
            ) -> impl ::std::future::Future<Output = ::streamlib::sdk::error::Result<()>> + Send {
                <Self as #processor_trait>::teardown(self, ctx)
            }

            fn __generated_on_pause(
                &mut self,
                ctx: &::streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
            ) -> impl ::std::future::Future<Output = ::streamlib::sdk::error::Result<()>> + Send {
                <Self as #processor_trait>::on_pause(self, ctx)
            }

            fn __generated_on_resume(
                &mut self,
                ctx: &::streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
            ) -> impl ::std::future::Future<Output = ::streamlib::sdk::error::Result<()>> + Send {
                <Self as #processor_trait>::on_resume(self, ctx)
            }
        }
    }
}

/// Generate from_config method from schema.
fn generate_from_config_from_schema(
    schema: &ProcessorSchema,
    config_field_name: &Option<Ident>,
    custom_fields: &[CustomField],
) -> TokenStream {
    // Generate iceoryx2-based IPC field initializers
    let ipc_input_init = if !schema.inputs.is_empty() {
        let add_port_calls: Vec<TokenStream> = schema
            .inputs
            .iter()
            .map(|port| {
                let name = &port.name;
                let buffer_size = port.buffer_size.unwrap_or(1);
                let read_mode_tokens = match port.read_mode.as_deref() {
                    Some("read_next_in_order") => {
                        quote! { ::streamlib::sdk::iceoryx2::ReadMode::ReadNextInOrder }
                    }
                    Some("skip_to_latest") | None => {
                        quote! { ::streamlib::sdk::iceoryx2::ReadMode::SkipToLatest }
                    }
                    Some(unknown) => {
                        let msg = format!(
                            "unknown read_mode '{}' on input port '{}', expected 'skip_to_latest' or 'read_next_in_order'",
                            unknown, name
                        );
                        return quote! { compile_error!(#msg); };
                    }
                };
                quote! { inputs.add_port(#name, #buffer_size, #read_mode_tokens); }
            })
            .collect();
        quote! {
            inputs: {
                let mut inputs = ::streamlib::sdk::iceoryx2::InputMailboxes::new();
                #(#add_port_calls)*
                inputs
            },
        }
    } else {
        quote! {}
    };

    let ipc_output_init = if !schema.outputs.is_empty() {
        quote! { outputs: ::std::sync::Arc::new(::streamlib::sdk::iceoryx2::OutputWriter::new()), }
    } else {
        quote! {}
    };

    let config_init = config_field_name
        .as_ref()
        .map(|name| quote! { #name: config, })
        .unwrap_or_default();

    // Initialize custom fields with Default::default()
    let custom_field_inits: Vec<TokenStream> = custom_fields
        .iter()
        .map(|f| {
            let name = &f.name;
            quote! { #name: ::std::default::Default::default(), }
        })
        .collect();

    quote! {
        fn from_config(config: Self::Config) -> ::streamlib::sdk::error::Result<Self> {
            Ok(Self {
                #ipc_input_init
                #ipc_output_init
                #config_init
                #(#custom_field_inits)*
            })
        }
    }
}

/// Generate descriptor method from schema.
///
/// `config_schema_id` is the canonical id string emitted into
/// `with_config_schema(...)` — the bare-name `TypeName` from the manifest
/// has been resolved by the macro entrypoint via the `schemas:` map
/// (#767). `None` when the processor declares no config.
fn generate_descriptor_from_schema(
    schema: &ProcessorSchema,
    description: &str,
    version: &str,
    config_schema_id: Option<&str>,
) -> TokenStream {
    let _name = &schema.name; // PascalCase short name retained for identifier checks elsewhere
    let repository = "https://github.com/tatolab/streamlib";

    // iceoryx2-based input ports
    let ipc_input_ports: Vec<TokenStream> = schema
        .inputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let port_schema_tokens = port_schema_spec_tokens(&p.schema);
            let port_desc = p.description.as_deref().unwrap_or("");
            quote! {
                .with_input(::streamlib::sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    schema: #port_schema_tokens,
                    required: true,
                    is_iceoryx2: true,
                })
            }
        })
        .collect();

    // iceoryx2-based output ports
    let ipc_output_ports: Vec<TokenStream> = schema
        .outputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let port_schema_tokens = port_schema_spec_tokens(&p.schema);
            let port_desc = p.description.as_deref().unwrap_or("");
            quote! {
                .with_output(::streamlib::sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    schema: #port_schema_tokens,
                    required: true,
                    is_iceoryx2: true,
                })
            }
        })
        .collect();

    // Config schema reference (if present). The bare-name `TypeName`
    // from the manifest was resolved to a canonical id string by the
    // macro entrypoint (#767); we emit that string into
    // `with_config_schema(...)` directly.
    let config_schema = schema.config.as_ref().map(|_c| {
        let schema_ref = config_schema_id.unwrap_or_else(|| {
            panic!(
                "internal error: ProcessorSchema declares config but no \
                 resolved canonical id was supplied to descriptor codegen"
            )
        });
        quote! {
            .with_config_schema(#schema_ref)
        }
    });

    // Declarative scheduling block sourced from the manifest. Absent →
    // `Normal` priority. The OS thread name is derived by the compiler
    // from the processor type + node id at spawn time, not authored.
    let scheduling = schema.scheduling.as_ref().map(|s| {
        let priority_tokens = thread_priority_tokens(s.priority);
        quote! {
            .with_scheduling(::streamlib::sdk::descriptors::ProcessorScheduling {
                priority: #priority_tokens,
            })
        }
    });

    quote! {
        fn descriptor() -> Option<::streamlib::sdk::descriptors::ProcessorDescriptor> {
            Some(
                ::streamlib::sdk::descriptors::ProcessorDescriptor::new(Processor::schema_ident(), #description)
                    .with_version(#version)
                    .with_repository(#repository)
                    #config_schema
                    #scheduling
                    #(#ipc_input_ports)*
                    #(#ipc_output_ports)*
            )
        }
    }
}

fn thread_priority_tokens(priority: streamlib_processor_schema::ThreadPriority) -> TokenStream {
    use streamlib_processor_schema::ThreadPriority;
    match priority {
        ThreadPriority::RealTime => quote! { ::streamlib::sdk::execution::ThreadPriority::RealTime },
        ThreadPriority::High => quote! { ::streamlib::sdk::execution::ThreadPriority::High },
        ThreadPriority::Normal => quote! { ::streamlib::sdk::execution::ThreadPriority::Normal },
    }
}

/// Generate iceoryx2 accessor methods from schema.
fn generate_iceoryx2_accessors_from_schema(schema: &ProcessorSchema) -> TokenStream {
    let has_iceoryx2_outputs = !schema.outputs.is_empty();
    let has_iceoryx2_inputs = !schema.inputs.is_empty();

    if !has_iceoryx2_outputs && !has_iceoryx2_inputs {
        return quote! {};
    }

    let has_outputs_impl = if has_iceoryx2_outputs {
        quote! {
            fn has_iceoryx2_outputs(&self) -> bool {
                true
            }
        }
    } else {
        quote! {}
    };

    let has_inputs_impl = if has_iceoryx2_inputs {
        quote! {
            fn has_iceoryx2_inputs(&self) -> bool {
                true
            }
        }
    } else {
        quote! {}
    };

    let get_output_writer_impl = if has_iceoryx2_outputs {
        quote! {
            fn get_iceoryx2_output_writer(&self) -> Option<::std::sync::Arc<::streamlib::sdk::iceoryx2::OutputWriter>> {
                Some(self.outputs.clone())
            }
        }
    } else {
        quote! {}
    };

    let get_input_mailboxes_impl = if has_iceoryx2_inputs {
        quote! {
            fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut ::streamlib::sdk::iceoryx2::InputMailboxes> {
                Some(&mut self.inputs)
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #has_outputs_impl
        #has_inputs_impl
        #get_output_writer_impl
        #get_input_mailboxes_impl
    }
}

#[cfg(test)]
mod processor_struct_emit_tests {
    use super::*;
    use streamlib_processor_schema::ProcessorSchema;

    fn minimal_schema() -> ProcessorSchema {
        ProcessorSchema {
            name: "MinimalProbe".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            runtime: Default::default(),
            entrypoint: None,
            execution: Default::default(),
            scheduling: None,
            config: None,
            state: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// Locks #734: the macro must NOT emit a `pub audio:
    /// ProcessorAudioConverter` field on the generated `Processor` struct.
    /// Mentally revert the codegen.rs deletion and this assertion flips.
    #[test]
    fn processor_struct_does_not_carry_audio_field() {
        let schema = minimal_schema();
        let rendered = generate_processor_struct_from_schema(&schema, &None, &[]).to_string();
        assert!(
            !rendered.contains("audio"),
            "generated Processor struct must not declare an `audio` field — got: {}",
            rendered
        );
        assert!(
            !rendered.contains("ProcessorAudioConverter"),
            "generated Processor struct must not reference ProcessorAudioConverter — got: {}",
            rendered
        );
    }

    /// Locks #734: the macro's `from_config` initializer must NOT initialize
    /// an `audio` field via `ProcessorAudioConverter::new()`. Mentally revert
    /// the codegen.rs deletion and this assertion flips.
    #[test]
    fn from_config_initializer_does_not_construct_audio_converter() {
        let schema = minimal_schema();
        let rendered =
            generate_from_config_from_schema(&schema, &None, &[]).to_string();
        assert!(
            !rendered.contains("ProcessorAudioConverter"),
            "from_config must not reference ProcessorAudioConverter — got: {}",
            rendered
        );
        assert!(
            !rendered.contains("audio :"),
            "from_config must not initialize an `audio` field — got: {}",
            rendered
        );
    }
}
