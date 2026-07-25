// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Code generation for the `#[processor(...)]` attribute macro.
//!
//! Generates the module wrapper with:
//! - `Processor` struct with public fields
//! - `InputLink` module with port markers
//! - `OutputLink` module with port markers
//! - Processor trait implementation

use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use streamlib_processor_schema::{PortSchemaSpec, ProcessorSchema, SchemaIdent};
use syn::{ItemStruct, Path};

/// Emit a `SchemaIdent` literal expression. Inputs are pre-validated by the
/// attribute parser so the `expect("validated")` calls are infallible.
fn schema_ident_tokens(ident: &SchemaIdent) -> TokenStream {
    let org = ident.org.as_str();
    let pkg = ident.package.as_str();
    let ty = ident.r#type.as_str();
    let major = ident.version.major;
    let minor = ident.version.minor;
    let patch = ident.version.patch;
    quote! {
        __streamlib_sdk::descriptors::SchemaIdent::new(
            __streamlib_sdk::descriptors::Org::new(#org).expect("validated by manifest parser"),
            __streamlib_sdk::descriptors::Package::new(#pkg).expect("validated by manifest parser"),
            __streamlib_sdk::descriptors::TypeName::new(#ty).expect("validated by manifest parser"),
            __streamlib_sdk::descriptors::SemVer::new(#major, #minor, #patch),
        )
    }
}

/// Emit a `PortSchemaSpec` literal expression.
///
/// `PortSchemaSpec::Named` should never reach this function — the attribute
/// grammar only ever produces `Any` or a fully-qualified `Specific`. A `Named`
/// here means the spec arrived unresolved, which is a macro implementation bug.
fn port_schema_spec_tokens(spec: &PortSchemaSpec) -> TokenStream {
    match spec {
        PortSchemaSpec::Any => quote! { __streamlib_sdk::processors::PortSchemaSpec::Any },
        PortSchemaSpec::Specific(ident) => {
            let inner = schema_ident_tokens(ident);
            quote! { __streamlib_sdk::processors::PortSchemaSpec::Specific(#inner) }
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

/// Generate a processor module from the attribute-declared [`ProcessorSchema`]
/// and its fully-qualified [`SchemaIdent`]. Identity, execution, and ports are
/// authored in the `#[processor(...)]` attribute — nothing here reads a file.
///
/// `config_type_path` is the Rust type path for the processor's typed `Config`
/// alias, taken verbatim from the attribute's `config = <Path>`; `None` binds
/// the tolerant [`EmptyConfig`]. `config_field_name` is the generated struct
/// field (present iff `config_type_path` is `Some`). `config_schema_id` is the
/// descriptor-metadata id string emitted into `with_config_schema(...)`.
pub fn generate_from_processor_schema(
    item: &ItemStruct,
    schema: &ProcessorSchema,
    schema_ident: &SchemaIdent,
    config_type_path: Option<&Path>,
    config_field_name: Option<&str>,
    config_schema_id: Option<&str>,
    sdk_root: TokenStream,
) -> TokenStream {
    let module_name = &item.ident;

    let config_type = match config_type_path {
        Some(path) => quote! { #path },
        None => quote! { __streamlib_sdk::processors::EmptyConfig },
    };

    let config_field_name = config_field_name.map(|name| Ident::new(name, Span::call_site()));

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
        /// `@<org>/<package>/<Type>@<version>` declared in the
        /// `#[processor(...)]` attribute.
        #[allow(dead_code)]
        pub fn schema_ident() -> __streamlib_sdk::descriptors::SchemaIdent {
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

    quote! {
        #[allow(non_snake_case)]
        pub mod #module_name {
            use super::*;

            // Alias the consumer's real SDK crate (streamlib-plugin-sdk for
            // plugins, the streamlib facade for hosts) so the emitted paths
            // below resolve without any `streamlib` aliasing in the consumer.
            #[allow(unused_imports)]
            use #sdk_root as __streamlib_sdk;

            /// Configuration type for this processor.
            pub type Config = #config_type;

            #schema_ident_const

            /// Create a [`ProcessorSpec`] for adding this processor to a runtime.
            ///
            /// Convenience wrapper around [`Processor::node`].
            pub fn node(config: Config) -> __streamlib_sdk::processors::ProcessorSpec {
                Processor::node(config)
            }

            #processor_struct

            #unsafe_send_impl

            #input_link_module

            #output_link_module

            #processor_impl
        }
    }
}

/// Custom field extracted from the user's struct definition.
struct CustomField {
    name: Ident,
    ty: syn::Type,
    /// Authored attributes re-emitted onto the generated `Processor` struct
    /// field, filtered by [`is_forwarded_onto_generated_field_definition`].
    attributes_for_generated_field_definition: Vec<syn::Attribute>,
    /// Authored attributes re-emitted onto the `from_config` struct-literal
    /// initializer, filtered by
    /// [`is_forwarded_onto_from_config_initializer`]. Narrower than the field
    /// definition's set — the two must stay in lockstep on presence.
    attributes_for_from_config_initializer: Vec<syn::Attribute>,
}

/// Authoring contract for `#[processor]` struct fields: `cfg` is the
/// load-bearing forward — dropping it makes a platform-conditional field
/// unconditional and its platform-specific type fails to resolve off that
/// platform. `doc` and the lint controls ride along so an author's `///` and
/// `#[allow(dead_code)]` survive expansion.
fn is_forwarded_onto_generated_field_definition(attribute: &syn::Attribute) -> bool {
    let path = attribute.path();
    path.is_ident("cfg")
        || path.is_ident("doc")
        || path.is_ident("allow")
        || path.is_ident("warn")
        || path.is_ident("deny")
        || path.is_ident("forbid")
        || path.is_ident("expect")
}

/// `cfg` is the only attribute a struct-expression field takes cleanly — a
/// `doc` there is an `unused_doc_comments` warning and the gates deny warnings.
/// `cfg_attr` is forwarded to neither site: an expansion that changes presence
/// would desync the field definition from this initializer.
fn is_forwarded_onto_from_config_initializer(attribute: &syn::Attribute) -> bool {
    attribute.path().is_ident("cfg")
}

/// Extract custom fields from the user's struct definition.
fn extract_custom_fields(item: &ItemStruct) -> Vec<CustomField> {
    match &item.fields {
        syn::Fields::Named(fields) => fields
            .named
            .iter()
            .map(|authored_field| CustomField {
                name: authored_field
                    .ident
                    .clone()
                    .expect("Named field must have ident"),
                ty: authored_field.ty.clone(),
                attributes_for_generated_field_definition: authored_field
                    .attrs
                    .iter()
                    .filter(|attribute| is_forwarded_onto_generated_field_definition(attribute))
                    .cloned()
                    .collect(),
                attributes_for_from_config_initializer: authored_field
                    .attrs
                    .iter()
                    .filter(|attribute| is_forwarded_onto_from_config_initializer(attribute))
                    .cloned()
                    .collect(),
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

    // Generate iceoryx2 PluginAbiObject fields if ports are defined.
    // Issue #894 retires the shared-Rust-type crossings — the
    // processor's `outputs` / `inputs` fields are now layout-stable
    // `#[repr(C)] { handle, vtable }` PluginAbiObjects; the host patches
    // them up via `ProcessorVTable::set_iceoryx2_resources` after
    // `from_config` returns.
    let ipc_input_field = if !schema.inputs.is_empty() {
        quote! { pub inputs: __streamlib_sdk::iceoryx2::InputMailboxes, }
    } else {
        quote! {}
    };

    let ipc_output_field = if !schema.outputs.is_empty() {
        quote! { pub outputs: __streamlib_sdk::iceoryx2::OutputWriter, }
    } else {
        quote! {}
    };

    // Generate custom fields from the user's struct definition
    let custom_field_defs: Vec<TokenStream> = custom_fields
        .iter()
        .map(|custom_field| {
            let attributes = &custom_field.attributes_for_generated_field_definition;
            let name = &custom_field.name;
            let ty = &custom_field.ty;
            quote! { #(#attributes)* pub #name: #ty, }
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
                impl super::__streamlib_sdk::processors::InputPortMarker for #port_name {
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
                impl super::__streamlib_sdk::processors::OutputPortMarker for #port_name {
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
    // The descriptor version mirrors the processor's own identity version — the
    // version-free `0.0.0` sentinel in the attribute grammar (#1409); the load
    // path re-registers the descriptor under the package version.
    let version = schema_ident.version.to_string();
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
            quote! { __streamlib_sdk::execution::ProcessExecution::Reactive },
            "Reactive",
            quote! { __streamlib_sdk::processors::ReactiveProcessor },
            quote! {
                <Self as __streamlib_sdk::processors::ReactiveProcessor>::process(self, ctx)
            },
            quote! {
                Err(__streamlib_sdk::error::Error::Runtime(
                    "start() is only valid for Manual execution mode.".into()
                ))
            },
            quote! {
                Err(__streamlib_sdk::error::Error::Runtime(
                    "stop() is only valid for Manual execution mode.".into()
                ))
            },
        ),
        ProcessorSchemaExecution::Manual => (
            quote! { __streamlib_sdk::execution::ProcessExecution::Manual },
            "Manual",
            quote! { __streamlib_sdk::processors::ManualProcessor },
            quote! {
                let _ = ctx;
                Err(__streamlib_sdk::error::Error::Runtime(
                    "process() is only valid for Reactive/Continuous execution modes.".into()
                ))
            },
            quote! {
                <Self as __streamlib_sdk::processors::ManualProcessor>::start(self, ctx)
            },
            quote! {
                <Self as __streamlib_sdk::processors::ManualProcessor>::stop(self, ctx)
            },
        ),
        ProcessorSchemaExecution::Continuous { interval_ms } => {
            let interval = *interval_ms;
            (
                quote! { __streamlib_sdk::execution::ProcessExecution::Continuous { interval_ms: #interval } },
                "Continuous",
                quote! { __streamlib_sdk::processors::ContinuousProcessor },
                quote! {
                    <Self as __streamlib_sdk::processors::ContinuousProcessor>::process(self, ctx)
                },
                quote! {
                    Err(__streamlib_sdk::error::Error::Runtime(
                        "start() is only valid for Manual execution mode.".into()
                    ))
                },
                quote! {
                    Err(__streamlib_sdk::error::Error::Runtime(
                        "stop() is only valid for Manual execution mode.".into()
                    ))
                },
            )
        }
    };

    let from_config_body =
        generate_from_config_from_schema(schema, config_field_name, custom_fields);
    let descriptor_impl =
        generate_descriptor_from_schema(schema, description, &version, config_schema_id);
    let iceoryx2_accessors = generate_iceoryx2_accessors_from_schema(schema);

    let update_config = config_field_name.as_ref().map(|name| {
        quote! {
            fn update_config(&mut self, config: Self::Config) -> __streamlib_sdk::error::Result<()> {
                self.#name = config;
                Ok(())
            }
        }
    });

    quote! {
        impl Processor {
            /// Processor PascalCase short name (the `type` segment of the
            /// structured [`SchemaIdent`](__streamlib_sdk::descriptors::SchemaIdent)).
            /// Use [`Processor::schema_ident`] for the full structured identity.
            pub const NAME: &'static str = #processor_name;

            /// Build-fingerprint handshake constants read by
            /// `export_plugin!` when it emits the `STREAMLIB_PLUGIN`
            /// declaration. Resolved against the detected SDK crate —
            /// the facade `streamlib` or the engine-free
            /// `streamlib-plugin-sdk`.
            #[doc(hidden)]
            pub const __STREAMLIB_ABI_LAYOUT_FINGERPRINT: u64 =
                __streamlib_sdk::plugin::PLUGIN_ABI_LAYOUT_FINGERPRINT;
            #[doc(hidden)]
            pub const __STREAMLIB_BUILD_IDENTITY: &'static str =
                __streamlib_sdk::plugin::BUILD_IDENTITY;

            /// Install the host's services into this plugin and return the
            /// registration helper. Called by `export_plugin!` so the plugin
            /// ABI entry point never names an SDK path — all SDK-crate
            /// resolution is centralized in the `#[processor]` macro's
            /// auto-detected `__streamlib_sdk` alias.
            ///
            /// # Safety
            /// `host_services` must point at a layout-compatible
            /// `HostServices` payload, per the plugin ABI register contract.
            #[doc(hidden)]
            pub unsafe fn __streamlib_install_host_services(
                host_services: *const ::core::ffi::c_void,
            ) -> ::core::option::Option<__streamlib_sdk::plugin::RegisterHelper> {
                unsafe { __streamlib_sdk::plugin::install_host_services(host_services) }
            }

            /// Register this processor with the host via a helper obtained from
            /// [`Processor::__streamlib_install_host_services`]. Called by
            /// `export_plugin!`.
            #[doc(hidden)]
            pub fn __streamlib_register(helper: &__streamlib_sdk::plugin::RegisterHelper) {
                helper.register::<Processor>();
            }

            /// Returns the structured wire identity for this processor —
            /// the version-free `@<org>/<package>/<Type>` declared in the
            /// `#[processor(...)]` attribute (carrying the `0.0.0`
            /// version-free sentinel).
            pub fn schema_ident() -> __streamlib_sdk::descriptors::SchemaIdent {
                #schema_ident_literal
            }

            /// Create a [`ProcessorSpec`](__streamlib_sdk::processors::ProcessorSpec)
            /// for adding this processor to a runtime.
            pub fn node(config: #config_type) -> __streamlib_sdk::processors::ProcessorSpec {
                // `ProcessorSpec::new` takes the SchemaIdent directly on the
                // engine-free SDK and via `From<SchemaIdent>` on the engine SDK
                // (where `name` is a `ProcessorTypeReference`).
                __streamlib_sdk::processors::ProcessorSpec::new(
                    Self::schema_ident(),
                    __streamlib_sdk::serde_json::to_value(&config)
                        .expect("Config serialization failed"),
                )
            }

            /// Returns the execution mode for this processor.
            pub fn execution_mode(&self) -> __streamlib_sdk::execution::ProcessExecution {
                #execution_variant
            }

            /// Returns a human-readable description of the execution mode.
            pub fn execution_mode_description(&self) -> &'static str {
                #execution_description
            }
        }

        impl __streamlib_sdk::processors::__generated_private::GeneratedProcessor for Processor {
            type Config = #config_type;

            fn name(&self) -> &str {
                Self::NAME
            }

            #from_config_body

            fn process(&mut self, ctx: &__streamlib_sdk::context::RuntimeContextLimitedAccess<'_>) -> __streamlib_sdk::error::Result<()> {
                #process_impl
            }

            fn start(&mut self, ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>) -> __streamlib_sdk::error::Result<()> {
                let _ = ctx;
                #start_impl
            }

            fn stop(&mut self, ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>) -> __streamlib_sdk::error::Result<()> {
                let _ = ctx;
                #stop_impl
            }

            #update_config

            fn execution_config(&self) -> __streamlib_sdk::execution::ExecutionConfig {
                __streamlib_sdk::execution::ExecutionConfig {
                    execution: #execution_variant,
                }
            }

            #descriptor_impl
            #iceoryx2_accessors

            fn __generated_setup(
                &mut self,
                ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>,
            ) -> __streamlib_sdk::error::Result<()> {
                <Self as #processor_trait>::setup(self, ctx)
            }

            fn __generated_teardown(
                &mut self,
                ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>,
            ) -> __streamlib_sdk::error::Result<()> {
                <Self as #processor_trait>::teardown(self, ctx)
            }

            fn __generated_on_pause(
                &mut self,
                ctx: &__streamlib_sdk::context::RuntimeContextLimitedAccess<'_>,
            ) -> __streamlib_sdk::error::Result<()> {
                <Self as #processor_trait>::on_pause(self, ctx)
            }

            fn __generated_on_resume(
                &mut self,
                ctx: &__streamlib_sdk::context::RuntimeContextLimitedAccess<'_>,
            ) -> __streamlib_sdk::error::Result<()> {
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
    // Issue #894: host-allocates iceoryx2 inner Arcs. The macro
    // emits empty PluginAbiObjects; the host's
    // `ProcessorInstance::install_iceoryx2_resources` patches in
    // real handles via `ProcessorVTable::set_iceoryx2_resources`
    // immediately after `from_config` returns. Per-port delivery
    // resolution (drain order + ring depth) is owned entirely by the
    // host wire path, which reads the wire type's `flow_class` and any
    // port-site `delivery_profile` override at wire time — the macro
    // registers no ports here.
    let ipc_input_init = if !schema.inputs.is_empty() {
        quote! { inputs: __streamlib_sdk::iceoryx2::InputMailboxes::empty(), }
    } else {
        quote! {}
    };

    let ipc_output_init = if !schema.outputs.is_empty() {
        quote! { outputs: __streamlib_sdk::iceoryx2::OutputWriter::empty(), }
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
        .map(|custom_field| {
            let attributes = &custom_field.attributes_for_from_config_initializer;
            let name = &custom_field.name;
            quote! { #(#attributes)* #name: ::std::default::Default::default(), }
        })
        .collect();

    quote! {
        fn from_config(config: Self::Config) -> __streamlib_sdk::error::Result<Self> {
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
/// `config_schema_id` is the descriptor-metadata id string emitted into
/// `with_config_schema(...)`, declared (or synthesized from the config type)
/// by the `#[processor(...)]` attribute. `None` when the processor declares
/// no config.
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
            let delivery_profile_tokens = match p.delivery_profile.as_deref() {
                Some(value) => quote! { ::std::option::Option::Some(#value.to_string()) },
                None => quote! { ::std::option::Option::None },
            };
            quote! {
                .with_input(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    schema: #port_schema_tokens,
                    required: true,
                    is_iceoryx2: true,
                    delivery_profile: #delivery_profile_tokens,
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
                .with_output(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    schema: #port_schema_tokens,
                    required: true,
                    is_iceoryx2: true,
                    delivery_profile: ::std::option::Option::None,
                })
            }
        })
        .collect();

    // Config schema reference (descriptor metadata), declared or synthesized
    // by the attribute. Emitted verbatim into `with_config_schema(...)`.
    let config_schema = config_schema_id.map(|schema_ref| {
        quote! {
            .with_config_schema(#schema_ref)
        }
    });

    // Declarative scheduling intent. Absent → `Normal` priority. The OS
    // thread name is derived by the compiler from the processor type + node
    // id at spawn time, not authored.
    let scheduling = schema.scheduling.as_ref().map(|s| {
        let priority_tokens = thread_priority_tokens(s.priority);
        quote! {
            .with_scheduling(__streamlib_sdk::descriptors::ProcessorScheduling {
                priority: #priority_tokens,
            })
        }
    });

    quote! {
        fn descriptor() -> Option<__streamlib_sdk::descriptors::ProcessorDescriptor> {
            Some(
                __streamlib_sdk::descriptors::ProcessorDescriptor::new(Processor::schema_ident(), #description)
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
        ThreadPriority::RealTime => quote! { __streamlib_sdk::execution::ThreadPriority::RealTime },
        ThreadPriority::High => quote! { __streamlib_sdk::execution::ThreadPriority::High },
        ThreadPriority::Normal => quote! { __streamlib_sdk::execution::ThreadPriority::Normal },
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

    // Issue #894: emit `set_iceoryx2_resources` to receive host-
    // allocated PluginAbiObjects + the `iceoryx2_output_writer_inner` /
    // `iceoryx2_input_mailboxes_inner` accessors so the host's
    // wiring path can mutate the inner Arc directly. The host owns
    // per-port registration (`InputMailboxesInner::add_port`) at wire
    // time, where it resolves the delivery profile from the wire type's
    // `flow_class` + any port-site override — the macro registers none.
    let assign_outputs = if has_iceoryx2_outputs {
        quote! {
            if let ::std::option::Option::Some(ow) = output_writer {
                self.outputs = ow;
            }
        }
    } else {
        quote! {}
    };

    let assign_inputs = if has_iceoryx2_inputs {
        quote! {
            if let ::std::option::Option::Some(im) = input_mailboxes {
                self.inputs = im;
            }
        }
    } else {
        quote! {
            let _ = input_mailboxes;
        }
    };

    let outputs_inner_impl = if has_iceoryx2_outputs {
        quote! {
            fn iceoryx2_output_writer_inner(
                &self,
            ) -> ::std::option::Option<::std::sync::Arc<__streamlib_sdk::iceoryx2::OutputWriterInner>> {
                self.outputs.inner_arc()
            }
        }
    } else {
        quote! {}
    };

    let inputs_inner_impl = if has_iceoryx2_inputs {
        quote! {
            fn iceoryx2_input_mailboxes_inner(
                &self,
            ) -> ::std::option::Option<::std::sync::Arc<__streamlib_sdk::iceoryx2::InputMailboxesInner>> {
                self.inputs.inner_arc()
            }
        }
    } else {
        quote! {}
    };

    let set_resources_impl = quote! {
        fn set_iceoryx2_resources(
            &mut self,
            output_writer: ::std::option::Option<__streamlib_sdk::iceoryx2::OutputWriter>,
            input_mailboxes: ::std::option::Option<__streamlib_sdk::iceoryx2::InputMailboxes>,
        ) -> __streamlib_sdk::error::Result<()> {
            #assign_outputs
            #assign_inputs
            ::std::result::Result::Ok(())
        }
    };

    quote! {
        #has_outputs_impl
        #has_inputs_impl
        #set_resources_impl
        #outputs_inner_impl
        #inputs_inner_impl
    }
}

#[cfg(test)]
mod processor_struct_emit_tests {
    use super::*;
    use streamlib_processor_schema::{PortSchemaSpec, ProcessorPortSchema, ProcessorSchema};

    fn minimal_schema() -> ProcessorSchema {
        ProcessorSchema {
            name: "MinimalProbe".to_string(),
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
        let rendered = generate_from_config_from_schema(&schema, &None, &[]).to_string();
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

    /// `TokenStream::to_string` spacing is not part of any contract, so
    /// assertions compare against a whitespace-free rendering.
    fn render_without_whitespace(tokens: TokenStream) -> String {
        tokens
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    fn platform_conditional_field_struct() -> ItemStruct {
        syn::parse_quote! {
            struct PlatformConditionalFieldProbe {
                #[cfg(target_os = "linux")]
                linux_only_backend_state: Option<u32>,
                platform_agnostic_state: u64,
            }
        }
    }

    fn schema_declaring_iceoryx2_input_and_output_ports() -> ProcessorSchema {
        let port = |name: &str| ProcessorPortSchema {
            name: name.to_string(),
            schema: PortSchemaSpec::Any,
            description: None,
            delivery_profile: None,
        };
        ProcessorSchema {
            inputs: vec![port("video_in")],
            outputs: vec![port("video_out")],
            ..minimal_schema()
        }
    }

    fn struct_with_every_custom_field_compiled_out() -> ItemStruct {
        syn::parse_quote! {
            struct EveryCustomFieldCompiledOutProbe {
                #[cfg(any())]
                never_compiled_backend_state: Option<u32>,
                #[cfg(any())]
                never_compiled_frame_counter: u64,
            }
        }
    }

    /// The `inputs` / `outputs` fields are the `#[repr(C)]` `(handle, vtable)`
    /// PluginAbiObjects the host patches through
    /// `ProcessorVTable::set_iceoryx2_resources`, so their presence is a plugin
    /// ABI contract, not an authoring convenience: they are emitted from the
    /// port schema ahead of the custom fields and no authored field attribute
    /// may ever reach them. Compiling out every authored field is the
    /// adversarial case — if the #1588 attribute filter ever widened to the
    /// whole field list, these assertions are what catches it.
    #[test]
    fn plugin_abi_object_port_fields_stay_unconditional_when_every_custom_field_is_compiled_out() {
        let schema = schema_declaring_iceoryx2_input_and_output_ports();
        let custom_fields = extract_custom_fields(&struct_with_every_custom_field_compiled_out());

        let struct_rendered = render_without_whitespace(generate_processor_struct_from_schema(
            &schema,
            &None,
            &custom_fields,
        ));
        assert!(
            struct_rendered.contains(
                "pubstructProcessor{pubinputs:__streamlib_sdk::iceoryx2::InputMailboxes,\
                 puboutputs:__streamlib_sdk::iceoryx2::OutputWriter,"
            ),
            "the PluginAbiObject port fields must open the generated struct with \
             nothing interposed — got: {}",
            struct_rendered
        );
        assert!(
            struct_rendered.contains("#[cfg(any())]pubnever_compiled_backend_state"),
            "the probe must actually exercise the attribute filter — got: {}",
            struct_rendered
        );

        let from_config_rendered = render_without_whitespace(generate_from_config_from_schema(
            &schema,
            &None,
            &custom_fields,
        ));
        assert!(
            from_config_rendered.contains(
                "Ok(Self{inputs:__streamlib_sdk::iceoryx2::InputMailboxes::empty(),\
                 outputs:__streamlib_sdk::iceoryx2::OutputWriter::empty(),"
            ),
            "the PluginAbiObject port initializers must stay unconditional — got: {}",
            from_config_rendered
        );
    }

    /// Locks #1588: a `#[cfg]` authored on a processor struct field must be
    /// re-emitted on the generated `Processor` field, or the field becomes
    /// unconditional and its platform-specific type fails to resolve off that
    /// platform.
    #[test]
    fn processor_struct_preserves_cfg_attribute_on_custom_field() {
        let custom_fields = extract_custom_fields(&platform_conditional_field_struct());
        let rendered = render_without_whitespace(generate_processor_struct_from_schema(
            &minimal_schema(),
            &None,
            &custom_fields,
        ));
        assert!(
            rendered.contains(r#"#[cfg(target_os="linux")]publinux_only_backend_state"#),
            "generated Processor struct must carry the authored `#[cfg]` on \
             `linux_only_backend_state` — got: {}",
            rendered
        );
        assert!(
            !rendered.contains(r#"#[cfg(target_os="linux")]pubplatform_agnostic_state"#),
            "the `#[cfg]` must not leak onto the unconditional field — got: {}",
            rendered
        );
    }

    /// Locks #1588: the `from_config` struct-literal initializer must carry the
    /// same `#[cfg]` as the field it initializes — an unconditional initializer
    /// for a conditional field does not compile.
    #[test]
    fn from_config_initializer_preserves_cfg_attribute_on_custom_field() {
        let custom_fields = extract_custom_fields(&platform_conditional_field_struct());
        let rendered = render_without_whitespace(generate_from_config_from_schema(
            &minimal_schema(),
            &None,
            &custom_fields,
        ));
        assert!(
            rendered.contains(r#"#[cfg(target_os="linux")]linux_only_backend_state:"#),
            "from_config must carry the authored `#[cfg]` on the \
             `linux_only_backend_state` initializer — got: {}",
            rendered
        );
    }

    fn annotated_field_struct() -> ItemStruct {
        syn::parse_quote! {
            struct AnnotatedFieldProbe {
                /// Authored doc on a processor field.
                #[allow(dead_code)]
                #[cfg_attr(target_os = "linux", allow(unused))]
                #[serde(skip)]
                annotated_backend_state: Option<u32>,
            }
        }
    }

    /// The field-definition site forwards `doc` and the lint controls so an
    /// author's `///` and `#[allow(dead_code)]` survive expansion; `cfg_attr`
    /// and unknown attributes stay dropped.
    #[test]
    fn processor_struct_forwards_doc_and_lint_attributes_but_not_cfg_attr() {
        let custom_fields = extract_custom_fields(&annotated_field_struct());
        let rendered = render_without_whitespace(generate_processor_struct_from_schema(
            &minimal_schema(),
            &None,
            &custom_fields,
        ));
        assert!(
            rendered.contains("Authoreddoconaprocessorfield."),
            "generated Processor struct must carry the authored doc — got: {}",
            rendered
        );
        assert!(
            rendered.contains("#[allow(dead_code)]"),
            "generated Processor struct must carry the authored lint control — got: {}",
            rendered
        );
        assert!(
            !rendered.contains("cfg_attr"),
            "`cfg_attr` must not be forwarded — its expansion could change \
             field presence and desync the two emission sites — got: {}",
            rendered
        );
        assert!(
            !rendered.contains("serde"),
            "unknown attributes stay dropped — got: {}",
            rendered
        );
    }

    /// The `from_config` struct-literal site takes `cfg` only — a `doc` on a
    /// struct-expression field is an `unused_doc_comments` warning and the
    /// gates deny warnings.
    #[test]
    fn from_config_initializer_forwards_cfg_only() {
        let custom_fields = extract_custom_fields(&annotated_field_struct());
        let rendered = render_without_whitespace(generate_from_config_from_schema(
            &minimal_schema(),
            &None,
            &custom_fields,
        ));
        assert!(
            rendered.contains("annotated_backend_state:"),
            "from_config must still initialize the field — got: {}",
            rendered
        );
        for dropped in ["doc", "allow", "cfg_attr", "serde"] {
            assert!(
                !rendered.contains(dropped),
                "from_config initializer must forward `cfg` only, found `{}` — got: {}",
                dropped,
                rendered
            );
        }
    }
}
