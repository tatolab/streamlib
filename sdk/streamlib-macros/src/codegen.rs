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
use quote::{quote, quote_spanned};
use streamlib_processor_schema::ProcessorSchema;
use syn::spanned::Spanned;
use syn::{ItemStruct, Path};

/// Generate a processor module from the attribute-declared [`ProcessorSchema`].
/// Execution and ports are authored in the `#[processor(...)]` attribute —
/// nothing here reads a file, and nothing authors identity.
///
/// `config_type_path` is the Rust type path for the processor's typed `Config`
/// alias, taken verbatim from the attribute's `config = <Path>`; `None` binds
/// the tolerant [`EmptyConfig`]. `config_field_name` is the generated struct
/// field (present iff `config_type_path` is `Some`). `config_schema_id` is the
/// descriptor-metadata id string emitted into `with_config_schema(...)`.
pub fn generate_from_processor_schema(
    item: &ItemStruct,
    schema: &ProcessorSchema,
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
    let refused_field_attribute_diagnostics = refused_field_attribute_diagnostics(item);

    let processor_struct =
        generate_processor_struct_from_schema(schema, &config_field_name, &custom_fields);
    let input_link_module = generate_input_link_module_from_schema(schema);
    let output_link_module = generate_output_link_module_from_schema(schema);
    let processor_impl = generate_processor_impl_from_schema(
        schema,
        &config_type,
        &config_field_name,
        &custom_fields,
        config_schema_id,
    );

    let processor_class_import_path_accessor = quote! {
        /// This processor's identity: the path its type is reached by.
        #[allow(dead_code)]
        pub fn processor_class_import_path()
            -> __streamlib_sdk::descriptors::ProcessorClassImportPath
        {
            Processor::processor_class_import_path()
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

            #refused_field_attribute_diagnostics

            /// Configuration type for this processor.
            pub type Config = #config_type;

            #processor_class_import_path_accessor

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
    /// The field's authored attributes verbatim; each emission site filters.
    attributes_authored_on_processor_struct_field: Vec<syn::Attribute>,
}

/// Authoring contract for `#[processor]` struct fields. Everything
/// [`is_forwarded_onto_from_config_initializer`] takes is admitted here first,
/// because an initializer gating a field the definition did not gate the same
/// way initializes a field that isn't there. `doc` and the `allow` / `warn` /
/// `deny` / `forbid` controls are the extras layered on top of that set, so an
/// author's `///` and `#[allow(dead_code)]` survive expansion. `expect` is
/// excluded from both: the generated field is `pub` where the authored one was
/// private, so a `dead_code` expectation the author wrote to silence a warning
/// becomes an `unfulfilled_lint_expectation` warning instead.
fn is_forwarded_onto_generated_field_definition(attribute: &syn::Attribute) -> bool {
    if is_forwarded_onto_from_config_initializer(attribute) {
        return true;
    }

    let path = attribute.path();
    path.is_ident("doc")
        || path.is_ident("allow")
        || path.is_ident("warn")
        || path.is_ident("deny")
        || path.is_ident("forbid")
}

/// `cfg` is the load-bearing forward — dropping it makes a platform-conditional
/// field unconditional and its platform-specific type fails to resolve off that
/// platform — and it is the only attribute a struct-expression field takes
/// cleanly: a `doc` there is an `unused_doc_comments` warning and the gates deny
/// warnings.
fn is_forwarded_onto_from_config_initializer(attribute: &syn::Attribute) -> bool {
    attribute.path().is_ident("cfg")
}

/// `cfg_attr` on a processor struct field is refused rather than dropped: it can
/// expand to a presence-changing `cfg`, and the field definition accepts a strict
/// superset of what the `from_config` initializer accepts, so an expansion
/// admitted at the definition site alone gates the field without gating its
/// initializer — an error far from its cause. Silently discarding it is the same
/// failure class as #1588.
fn refused_field_attribute_diagnostics(item: &ItemStruct) -> TokenStream {
    let syn::Fields::Named(fields) = &item.fields else {
        return quote! {};
    };

    let diagnostics: Vec<TokenStream> = fields
        .named
        .iter()
        .filter_map(|authored_field| {
            let field_name = authored_field.ident.as_ref()?;
            let refused = authored_field
                .attrs
                .iter()
                .find(|attribute| attribute.path().is_ident("cfg_attr"))?;
            let message = format!(
                "`#[cfg_attr(...)]` is not supported on `#[processor]` struct fields (field \
                 `{field_name}`): a `cfg_attr` that expands to a `cfg` would change the field's \
                 presence on the generated `Processor` struct without changing it on the \
                 `from_config` initializer, and the two would no longer agree. Author the \
                 `#[cfg(...)]` directly on the field instead."
            );
            Some(quote_spanned! { refused.span() => compile_error!(#message); })
        })
        .collect();

    quote! { #(#diagnostics)* }
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
                attributes_authored_on_processor_struct_field: authored_field.attrs.clone(),
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

    // Generate iceoryx2 port-handle fields if ports are defined. The
    // processor's `outputs` / `inputs` fields are the layout-stable
    // `OutputWriter` / `InputMailboxes` handles; the engine patches
    // them up via `GeneratedProcessor::set_iceoryx2_resources` after
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
            let attributes = custom_field
                .attributes_authored_on_processor_struct_field
                .iter()
                .filter(|attribute| is_forwarded_onto_generated_field_definition(attribute));
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
    config_type: &TokenStream,
    config_field_name: &Option<Ident>,
    custom_fields: &[CustomField],
    config_schema_id: Option<&str>,
) -> TokenStream {
    use streamlib_processor_schema::ProcessorSchemaExecution;

    let processor_class_short_name = &schema.name;
    let description = schema.description.as_deref().unwrap_or("Processor");

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
    let descriptor_impl = generate_descriptor_from_schema(schema, description, config_schema_id);
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
            /// The authored type's name — what an instance's display name
            /// defaults to. Not the processor's identity: use
            /// [`Processor::processor_class_import_path`] for that.
            pub const NAME: &'static str = #processor_class_short_name;

            /// This processor's identity: the path its type is reached by,
            /// captured where the macro expanded.
            pub fn processor_class_import_path()
                -> __streamlib_sdk::descriptors::ProcessorClassImportPath
            {
                __streamlib_sdk::descriptors::ProcessorClassImportPath::new(
                    ::core::module_path!(),
                )
                .expect("module_path! always names the enclosing module")
            }

            /// Create a [`ProcessorSpec`](__streamlib_sdk::processors::ProcessorSpec)
            /// for adding this processor to a runtime.
            pub fn node(config: #config_type) -> __streamlib_sdk::processors::ProcessorSpec {
                __streamlib_sdk::processors::ProcessorSpec::new(
                    Self::processor_class_import_path(),
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
    // The macro emits empty handles; the host's
    // `ProcessorInstance::install_iceoryx2_resources` patches in real
    // handles via `GeneratedProcessor::set_iceoryx2_resources`
    // immediately after `from_config` returns. Per-port delivery
    // resolution (drain order + ring depth) is owned entirely by the
    // host wire path, which reads the destination input port's declared
    // `delivery_profile` at wire time — the macro registers no ports here.
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
            let attributes = custom_field
                .attributes_authored_on_processor_struct_field
                .iter()
                .filter(|attribute| is_forwarded_onto_from_config_initializer(attribute));
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
    config_schema_id: Option<&str>,
) -> TokenStream {
    let repository = "https://github.com/tatolab/streamlib";

    // iceoryx2-based input ports
    let ipc_input_ports: Vec<TokenStream> = schema
        .inputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let port_desc = p.description.as_deref().unwrap_or("");
            let delivery_profile_tokens = match p.delivery_profile.as_deref() {
                Some(value) => quote! { ::std::option::Option::Some(#value.to_string()) },
                None => quote! { ::std::option::Option::None },
            };
            let audio_window_tokens = audio_window_contract_tokens(p.audio_window.as_ref());
            quote! {
                .with_input(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    required: true,
                    is_iceoryx2: true,
                    delivery_profile: #delivery_profile_tokens,
                    audio_window: #audio_window_tokens,
                })
            }
        })
        .collect();

    // iceoryx2-based output ports
    let no_audio_window_tokens = audio_window_contract_tokens(None);
    let ipc_output_ports: Vec<TokenStream> = schema
        .outputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let port_desc = p.description.as_deref().unwrap_or("");
            quote! {
                .with_output(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    required: true,
                    is_iceoryx2: true,
                    delivery_profile: ::std::option::Option::None,
                    audio_window: #no_audio_window_tokens,
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

    // `module_path!()` resolves here to `<author's module>::<authored type>`,
    // because this expansion lands inside the `pub mod` the macro names after
    // the author's struct — the generated module's own path *is* the type
    // path, with no `stringify!` needed to append the name.
    quote! {
        fn descriptor() -> Option<__streamlib_sdk::descriptors::ProcessorDescriptor> {
            Some(
                __streamlib_sdk::descriptors::ProcessorDescriptor::new(
                    __streamlib_sdk::descriptors::ProcessorClassShortName::new(Processor::NAME)
                        .expect("a struct's identifier is never blank"),
                    Processor::processor_class_import_path(),
                    #description,
                )
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

    // Emit `set_iceoryx2_resources` to receive host-allocated handles +
    // the `iceoryx2_output_writer_inner` / `iceoryx2_input_mailboxes_inner`
    // accessors so the host's wiring path can mutate the inner Arc
    // directly. The host owns per-port registration
    // (`InputMailboxesInner::add_port`) at wire time, where it reads the
    // destination input port's declared delivery profile — the macro
    // registers none.
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

/// Emit the `audio_window` a port declared, reconstructing the contract in the
/// generated descriptor.
///
/// The values are already validated by the grammar, so this only has to render
/// them — a contract that gets this far is one the read-side stage can honour.
fn audio_window_contract_tokens(
    audio_window: Option<&streamlib_processor_schema::AudioWindowContract>,
) -> TokenStream {
    use streamlib_processor_schema::AudioWindowContract;

    match audio_window {
        None => quote! { ::std::option::Option::None },
        // Rendering-only: `graph` uses it to say five values came from a device
        // rather than from an author, and the grammar this codegen reads has no
        // spelling that produces one. A `compile_error!` rather than a panic
        // because the only way to reach it is a future grammar change, and that
        // change should fail the build that introduced it.
        Some(AudioWindowContract::Device(_)) => quote! {
            compile_error!(
                "`audio_window` resolved from a device is a rendering, not a declaration \
                 — a port states its five values or `match_device`"
            )
        },
        Some(AudioWindowContract::MatchDevice {}) => quote! {
            ::std::option::Option::Some(
                __streamlib_sdk::descriptors::AudioWindowContract::MatchDevice {},
            )
        },
        Some(AudioWindowContract::Declaration(values)) => {
            let sample_rate = values.sample_rate;
            let channels = match values.channels {
                Some(channels) => quote! { ::std::option::Option::Some(#channels) },
                None => quote! { ::std::option::Option::None },
            };
            let dtype = &values.dtype;
            let window_size = values.window_size;
            let hop = values.hop;
            quote! {
                ::std::option::Option::Some(
                    __streamlib_sdk::descriptors::AudioWindowContract::Declaration(
                        __streamlib_sdk::descriptors::AudioWindowContractDeclaredValues {
                            sample_rate: #sample_rate,
                            channels: #channels,
                            dtype: #dtype.to_string(),
                            window_size: #window_size,
                            hop: #hop,
                        },
                    ),
                )
            }
        }
    }
}

#[cfg(test)]
mod processor_struct_emit_tests {
    use super::*;
    use streamlib_processor_schema::{ProcessorPortSchema, ProcessorSchema};

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
    fn render_token_stream_without_whitespace(tokens: TokenStream) -> String {
        tokens
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    fn attribute_path_ident(attribute: &syn::Attribute) -> String {
        let path = attribute.path();
        path.get_ident()
            .map(Ident::to_string)
            .unwrap_or_else(|| render_token_stream_without_whitespace(quote! { #path }))
    }

    /// The path ident of each attribute, in authored order.
    fn attribute_path_idents(attributes: &[syn::Attribute]) -> Vec<String> {
        attributes.iter().map(attribute_path_ident).collect()
    }

    fn attributes_authored_across_probe_custom_fields(
        custom_fields: &[CustomField],
    ) -> Vec<&syn::Attribute> {
        custom_fields
            .iter()
            .flat_map(|custom_field| {
                custom_field
                    .attributes_authored_on_processor_struct_field
                    .iter()
            })
            .collect()
    }

    /// The attribute path idents one emission site's filter keeps — the
    /// decision the filter actually makes, read without going through a
    /// rendering.
    fn forwarded_attribute_path_idents(
        custom_fields: &[CustomField],
        is_forwarded_onto_emission_site: impl Fn(&syn::Attribute) -> bool,
    ) -> Vec<String> {
        attributes_authored_across_probe_custom_fields(custom_fields)
            .into_iter()
            .filter(|attribute| is_forwarded_onto_emission_site(attribute))
            .map(attribute_path_ident)
            .collect()
    }

    fn parse_generated_processor_struct(tokens: TokenStream) -> ItemStruct {
        syn::parse2(tokens).expect("the struct emitter must emit a parseable `struct Processor`")
    }

    fn declared_field_names(generated_struct: &ItemStruct) -> Vec<String> {
        match &generated_struct.fields {
            syn::Fields::Named(fields) => fields
                .named
                .iter()
                .filter_map(|field| field.ident.as_ref().map(Ident::to_string))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn declared_field<'a>(
        generated_struct: &'a ItemStruct,
        field_name: &str,
    ) -> Option<&'a syn::Field> {
        match &generated_struct.fields {
            syn::Fields::Named(fields) => fields.named.iter().find(|field| {
                field
                    .ident
                    .as_ref()
                    .is_some_and(|ident| ident == field_name)
            }),
            _ => None,
        }
    }

    fn type_path_last_segment(ty: &syn::Type) -> Option<String> {
        match ty {
            syn::Type::Path(type_path) => type_path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            _ => None,
        }
    }

    /// The `Self { .. }` literal the emitted `from_config` returns, so field
    /// assertions read the struct expression rather than its rendering.
    fn parse_generated_from_config_struct_expression(tokens: TokenStream) -> syn::ExprStruct {
        let from_config: syn::ImplItemFn =
            syn::parse2(tokens).expect("the from_config emitter must emit a parseable method");
        let tail = from_config
            .block
            .stmts
            .last()
            .expect("from_config must have a body");
        let syn::Stmt::Expr(syn::Expr::Call(ok_call), _) = tail else {
            panic!("from_config must end in an `Ok(..)` tail expression — got: {tail:?}");
        };
        match ok_call.args.first() {
            Some(syn::Expr::Struct(struct_expression)) => struct_expression.clone(),
            other => panic!("from_config must wrap a `Self {{ .. }}` literal — got: {other:?}"),
        }
    }

    fn initialized_field_names(struct_expression: &syn::ExprStruct) -> Vec<String> {
        struct_expression
            .fields
            .iter()
            .filter_map(|field_value| match &field_value.member {
                syn::Member::Named(ident) => Some(ident.to_string()),
                syn::Member::Unnamed(_) => None,
            })
            .collect()
    }

    fn initialized_field<'a>(
        struct_expression: &'a syn::ExprStruct,
        field_name: &str,
    ) -> Option<&'a syn::FieldValue> {
        struct_expression
            .fields
            .iter()
            .find(|field_value| match &field_value.member {
                syn::Member::Named(ident) => ident == field_name,
                syn::Member::Unnamed(_) => false,
            })
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
            description: None,
            delivery_profile: None,
            audio_window: None,
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

    /// The `inputs` / `outputs` fields are the `OutputWriter` /
    /// `InputMailboxes` handles the engine patches through
    /// `GeneratedProcessor::set_iceoryx2_resources`, so their presence is
    /// a wiring contract, not an authoring convenience: no authored field
    /// attribute may ever reach them, at either emission site. Compiling out every
    /// authored field is the adversarial case — if the #1588 attribute filter
    /// ever widened to the whole field list, these assertions are what catches
    /// it. Declaration order is deliberately not asserted: `Processor` is not
    /// `#[repr(C)]`, so its field order is not a contract.
    #[test]
    fn port_fields_stay_unconditional_when_every_custom_field_is_compiled_out() {
        let schema = schema_declaring_iceoryx2_input_and_output_ports();
        let custom_fields = extract_custom_fields(&struct_with_every_custom_field_compiled_out());

        let generated_struct = parse_generated_processor_struct(
            generate_processor_struct_from_schema(&schema, &None, &custom_fields),
        );
        for (port_field_name, port_field_type) in
            [("inputs", "InputMailboxes"), ("outputs", "OutputWriter")]
        {
            let port_field =
                declared_field(&generated_struct, port_field_name).unwrap_or_else(|| {
                    panic!(
                        "the generated Processor struct must declare the `{}` port \
                     field — it declares {:?}",
                        port_field_name,
                        declared_field_names(&generated_struct)
                    )
                });
            assert!(
                port_field.attrs.is_empty(),
                "the `{}` port field must be unconditional — no authored field attribute may \
                 reach it, but it carries {:?}",
                port_field_name,
                attribute_path_idents(&port_field.attrs)
            );
            assert!(
                matches!(port_field.vis, syn::Visibility::Public(_)),
                "the `{}` port field must stay `pub` — the host patches it after `from_config` \
                 returns",
                port_field_name
            );
            assert_eq!(
                type_path_last_segment(&port_field.ty).as_deref(),
                Some(port_field_type),
                "the `{}` port field must keep its handle type",
                port_field_name
            );
        }

        let compiled_out_field = declared_field(&generated_struct, "never_compiled_backend_state")
            .unwrap_or_else(|| {
                panic!(
                    "the probe's compiled-out field must still be emitted so this test exercises \
                     the attribute filter — the struct declares {:?}",
                    declared_field_names(&generated_struct)
                )
            });
        assert_eq!(
            attribute_path_idents(&compiled_out_field.attrs),
            vec!["cfg".to_string()],
            "the probe must actually exercise the attribute filter — its compiled-out field has \
             to carry the authored `#[cfg]`"
        );

        let from_config_struct_expression = parse_generated_from_config_struct_expression(
            generate_from_config_from_schema(&schema, &None, &custom_fields),
        );
        for port_field_name in ["inputs", "outputs"] {
            let port_initializer =
                initialized_field(&from_config_struct_expression, port_field_name).unwrap_or_else(
                    || {
                        panic!(
                            "`from_config` must initialize the `{}` port field — \
                             it initializes {:?}",
                            port_field_name,
                            initialized_field_names(&from_config_struct_expression)
                        )
                    },
                );
            assert!(
                port_initializer.attrs.is_empty(),
                "the `{}` port initializer must stay unconditional — no authored field attribute \
                 may reach it, but it carries {:?}",
                port_field_name,
                attribute_path_idents(&port_initializer.attrs)
            );
        }
    }

    /// Locks #1588: a `#[cfg]` authored on a processor struct field must be
    /// re-emitted on the generated `Processor` field, or the field becomes
    /// unconditional and its platform-specific type fails to resolve off that
    /// platform.
    #[test]
    fn processor_struct_preserves_cfg_attribute_on_custom_field() {
        let custom_fields = extract_custom_fields(&platform_conditional_field_struct());
        let rendered = render_token_stream_without_whitespace(
            generate_processor_struct_from_schema(&minimal_schema(), &None, &custom_fields),
        );
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
        let rendered = render_token_stream_without_whitespace(generate_from_config_from_schema(
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
                #[expect(unused_parens)]
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
        let rendered = render_token_stream_without_whitespace(
            generate_processor_struct_from_schema(&minimal_schema(), &None, &custom_fields),
        );
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

    /// `expect` is the one lint control that must NOT ride along. The macro
    /// re-emits the authored field as `pub`, which changes which lints can fire
    /// on it at all, so a forwarded `#[expect(...)]` the author wrote to silence
    /// a warning lands unfulfilled and warns `unfulfilled_lint_expectation`
    /// instead — the inverse of the silencing they asked for.
    #[test]
    fn processor_struct_does_not_forward_expect_onto_the_generated_pub_field() {
        let custom_fields = extract_custom_fields(&annotated_field_struct());
        let forwarded_attribute_paths = forwarded_attribute_path_idents(
            &custom_fields,
            is_forwarded_onto_generated_field_definition,
        );
        assert!(
            !forwarded_attribute_paths.contains(&"expect".to_string()),
            "`expect` must not reach the generated `pub` field — it would land \
             unfulfilled and warn; forwarded attributes were: {:?}",
            forwarded_attribute_paths
        );
        assert!(
            forwarded_attribute_paths.contains(&"allow".to_string()),
            "the probe must still exercise a forwarded lint control, or this \
             test passes vacuously; forwarded attributes were: {:?}",
            forwarded_attribute_paths
        );
    }

    /// Every `compile_error!` message in a generated token stream, so a
    /// diagnostic test asserts on the text the author will actually read.
    fn compile_error_messages(tokens: TokenStream) -> Vec<String> {
        use proc_macro2::TokenTree;

        let mut messages = Vec::new();
        let mut previous_ident_was_compile_error = false;
        for tree in tokens {
            match tree {
                TokenTree::Ident(ident) => {
                    previous_ident_was_compile_error = ident == "compile_error";
                }
                TokenTree::Group(group) => {
                    if previous_ident_was_compile_error
                        && let Ok(message) = syn::parse2::<syn::LitStr>(group.stream())
                    {
                        messages.push(message.value());
                    }
                    previous_ident_was_compile_error = false;
                    messages.extend(compile_error_messages(group.stream()));
                }
                TokenTree::Punct(_) => {}
                TokenTree::Literal(_) => previous_ident_was_compile_error = false,
            }
        }
        messages
    }

    fn expand_probe_processor(item: &ItemStruct) -> TokenStream {
        generate_from_processor_schema(
            item,
            &minimal_schema(),
            None,
            None,
            None,
            quote! { streamlib },
        )
    }

    fn struct_with_cfg_attr_on_a_field() -> ItemStruct {
        syn::parse_quote! {
            struct CfgAttrFieldProbe {
                #[cfg_attr(target_os = "linux", allow(unused))]
                cfg_attr_annotated_backend_state: Option<u32>,
                unannotated_frame_counter: u64,
            }
        }
    }

    fn struct_with_dropped_derive_helper_attributes_on_a_field() -> ItemStruct {
        syn::parse_quote! {
            struct DeriveHelperAnnotatedFieldProbe {
                /// Authored doc on a processor field.
                #[allow(dead_code)]
                #[serde(skip)]
                #[schemars(skip)]
                derive_helper_annotated_backend_state: Option<u32>,
            }
        }
    }

    /// A `cfg_attr` on a processor field is refused loudly instead of dropped
    /// silently — silently dropping a presence-changing attribute is the same
    /// failure class as #1588 itself.
    #[test]
    fn cfg_attr_on_a_processor_field_emits_a_compile_error_naming_the_field() {
        let messages =
            compile_error_messages(expand_probe_processor(&struct_with_cfg_attr_on_a_field()));
        assert_eq!(
            messages.len(),
            1,
            "exactly one field carries `cfg_attr`, so exactly one diagnostic is \
             expected — got: {:?}",
            messages
        );
        assert!(
            messages[0].contains("cfg_attr_annotated_backend_state"),
            "the diagnostic must name the offending field so the author can find \
             it — got: {}",
            messages[0]
        );
        assert!(
            messages[0].contains("cfg_attr"),
            "the diagnostic must name the unsupported attribute — got: {}",
            messages[0]
        );
    }

    /// The refusal is scoped to `cfg_attr`. Derive-helper attributes are dropped
    /// by design — erroring on those would break every processor carrying one.
    #[test]
    fn a_dropped_derive_helper_attribute_on_a_processor_field_stays_silent() {
        let messages = compile_error_messages(expand_probe_processor(
            &struct_with_dropped_derive_helper_attributes_on_a_field(),
        ));
        assert!(
            messages.is_empty(),
            "only `cfg_attr` is refused; a dropped derive-helper attribute must \
             not raise a diagnostic — got: {:?}",
            messages
        );
    }

    fn struct_with_a_cfg_alongside_every_other_forwardable_attribute() -> ItemStruct {
        syn::parse_quote! {
            struct CfgAlongsideEveryOtherAttributeProbe {
                /// Authored doc on a processor field.
                #[allow(dead_code)]
                #[expect(unused_parens)]
                #[cfg(target_os = "linux")]
                #[cfg_attr(target_os = "linux", allow(unused))]
                #[serde(skip)]
                cfg_and_lint_annotated_backend_state: Option<u32>,
            }
        }
    }

    /// The `from_config` struct-literal site takes `cfg` and nothing else — a
    /// `doc` on a struct-expression field is an `unused_doc_comments` warning
    /// and the gates deny warnings.
    #[test]
    fn from_config_initializer_forwards_cfg_only() {
        let custom_fields =
            extract_custom_fields(&struct_with_a_cfg_alongside_every_other_forwardable_attribute());
        let from_config_struct_expression = parse_generated_from_config_struct_expression(
            generate_from_config_from_schema(&minimal_schema(), &None, &custom_fields),
        );

        let initializer = initialized_field(
            &from_config_struct_expression,
            "cfg_and_lint_annotated_backend_state",
        )
        .unwrap_or_else(|| {
            panic!(
                "`from_config` must still initialize the authored field — it initializes {:?}",
                initialized_field_names(&from_config_struct_expression)
            )
        });
        assert_eq!(
            attribute_path_idents(&initializer.attrs),
            vec!["cfg".to_string()],
            "the `from_config` initializer must carry the authored `#[cfg]` and nothing else — \
             the probe also authors `doc`, `allow`, `expect`, `cfg_attr`, and a derive helper"
        );
    }

    /// The initializer's accepted set must stay a subset of the field
    /// definition's: a presence-changing attribute on the initializer for a
    /// field the definition did not gate the same way initializes a field that
    /// isn't there. [`is_forwarded_onto_generated_field_definition`] holds the
    /// nesting by construction, so this is the regression pin that catches an
    /// edit splitting the two sites back into independent lists.
    #[test]
    fn every_attribute_the_from_config_initializer_takes_also_reaches_the_field_definition() {
        let custom_fields =
            extract_custom_fields(&struct_with_a_cfg_alongside_every_other_forwardable_attribute());
        let authored = attributes_authored_across_probe_custom_fields(&custom_fields);
        assert!(
            authored
                .iter()
                .any(|attribute| is_forwarded_onto_from_config_initializer(attribute)),
            "the probe must author at least one initializer-forwarded attribute, or this \
             test passes vacuously — it authors: {:?}",
            authored
                .iter()
                .copied()
                .map(attribute_path_ident)
                .collect::<Vec<String>>()
        );
        for attribute in authored {
            if is_forwarded_onto_from_config_initializer(attribute) {
                assert!(
                    is_forwarded_onto_generated_field_definition(attribute),
                    "`{}` reaches the `from_config` initializer but not the generated \
                     field definition — the initializer would gate a field the definition \
                     does not declare the same way",
                    attribute_path_ident(attribute)
                );
            }
        }
    }

    fn rendered_descriptor() -> String {
        render_token_stream_without_whitespace(generate_descriptor_from_schema(
            &minimal_schema(),
            "a probe",
            None,
        ))
    }

    /// The whole `impl Processor` block, which is where the identity accessor
    /// the descriptor delegates to is emitted.
    fn rendered_processor_impl() -> String {
        render_token_stream_without_whitespace(generate_processor_impl_from_schema(
            &minimal_schema(),
            &quote! { __streamlib_sdk::processors::EmptyConfig },
            &None,
            &[],
            None,
        ))
    }

    /// The mechanism, not the result — what the string comes out as is
    /// asserted where a real `#[processor]` can be expanded and read back
    /// (`streamlib-engine/tests/processor_class_import_path_test.rs`).
    #[test]
    fn the_descriptor_captures_its_identity_with_module_path() {
        let rendered = rendered_processor_impl();
        assert!(
            rendered.contains("module_path!()"),
            "identity must be captured at the expansion site — got: {rendered}"
        );
        // Captured once. The descriptor names the accessor rather than
        // re-expanding `module_path!()`, so the two can never disagree.
        assert_eq!(
            rendered.matches("module_path!()").count(),
            1,
            "identity must be captured in exactly one place — got: {rendered}"
        );
        assert!(
            rendered_descriptor().contains("Processor::processor_class_import_path()"),
            "the descriptor must take its identity from that one capture"
        );
    }

    /// `std::any::type_name`'s output format is documented as unspecified and
    /// free to change between compiler versions. Keying a registry on it means
    /// a toolchain bump silently renames every processor, with nothing failing
    /// to say so — which is why this is a test and not a comment.
    #[test]
    fn the_descriptor_never_reaches_for_type_name() {
        for rendered in [rendered_descriptor(), rendered_processor_impl()] {
            assert!(
                !rendered.contains("type_name"),
                "identity must never be reflected at runtime — got: {rendered}"
            );
        }
    }
}
