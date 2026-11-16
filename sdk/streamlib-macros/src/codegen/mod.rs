// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

use crate::grammar::{
    DeclaredDescription, ParsedPort, ParsedProcessorAttr, PortDirection,
    declared_description_str_expression,
};
use syn::ItemStruct;
use syn::spanned::Spanned;

mod author_visibility;
mod port_marker_module;

use author_visibility::{
    GENERATED_MODULE_MEMBER_DEPTH, GENERATED_PORT_MARKER_DEPTH,
    author_visibility_written_from_a_nested_module,
};
use port_marker_module::generate_port_marker_module;

/// Generate a processor module from the parsed `#[processor(...)]` attribute.
/// Execution and ports are authored in the attribute — nothing here reads a
/// file, and nothing authors identity.
///
/// The descriptor is emitted from the attribute's [`ProcessorSchema`], which
/// carries each port's cast name; the port markers keep the author's spelling.
/// A config declared as `config = <Path>` binds that typed `Config` alias and a
/// generated struct field; none binds `EmptyConfig`, whose schema is the empty
/// object a processor declaring no config publishes.
pub(crate) fn generate_from_parsed_processor_attr(
    item: &ItemStruct,
    parsed: &ParsedProcessorAttr,
    sdk_root: TokenStream,
) -> TokenStream {
    let module_name = &item.ident;
    let module_visibility = &item.vis;
    let member_visibility =
        author_visibility_written_from_a_nested_module(&item.vis, GENERATED_MODULE_MEMBER_DEPTH);
    let port_marker_visibility =
        author_visibility_written_from_a_nested_module(&item.vis, GENERATED_PORT_MARKER_DEPTH);
    let schema = &parsed.to_processor_schema();

    let config_type = match &parsed.config_type {
        Some(path) => quote! { #path },
        None => quote! { __streamlib_sdk::processors::EmptyConfig },
    };

    let config_field_name = parsed
        .config_type
        .as_ref()
        .map(|_| Ident::new(&parsed.config_field_name, Span::call_site()));

    // Extract custom fields from the user's struct
    let custom_fields = extract_custom_fields(item);
    let refused_field_attribute_diagnostics = refused_field_attribute_diagnostics(item);

    let processor_struct = generate_processor_struct_from_schema(
        schema,
        &config_field_name,
        &custom_fields,
        &member_visibility,
    );
    let input_link_module = generate_port_marker_module(
        PortDirection::Input,
        &parsed.inputs,
        &member_visibility,
        &port_marker_visibility,
    );
    let output_link_module = generate_port_marker_module(
        PortDirection::Output,
        &parsed.outputs,
        &member_visibility,
        &port_marker_visibility,
    );
    let processor_impl = generate_processor_impl_from_schema(
        schema,
        &member_visibility,
        &ProcessorDeclaredMetaAndPortDescriptions::of(parsed),
        &node_type_minted(parsed.built_in_node, &schema.name),
        &config_type,
        &config_field_name,
        &custom_fields,
    );

    let processor_class_import_path_accessor = quote! {
        /// This node type's identity: its class in `tatolab.stream` for a
        /// built-in, else the module path captured where the macro expanded.
        #[allow(dead_code, reason = "the identity function is emitted for every node, and not every crate calls it")]
        #member_visibility fn processor_class_import_path()
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
        #[allow(non_snake_case, reason = "the generated module is named after the author's processor type")]
        #module_visibility mod #module_name {
            use super::*;

            // Alias the consumer's real SDK crate (streamlib-plugin-sdk for
            // plugins, the streamlib facade for hosts) so the emitted paths
            // below resolve without any `streamlib` aliasing in the consumer.
            #[allow(unused_imports, reason = "the generated module may name nothing from the SDK root")]
            use #sdk_root as __streamlib_sdk;

            #refused_field_attribute_diagnostics

            /// Configuration type for this processor.
            #member_visibility type Config = #config_type;

            #processor_class_import_path_accessor

            /// Create a [`ProcessorSpec`] for adding this processor to a runtime.
            ///
            /// Convenience wrapper around [`Processor::node`].
            #member_visibility fn node(config: Config) -> __streamlib_sdk::processors::ProcessorSpec {
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
/// excluded from both: the generated field takes the struct's visibility, wider
/// than the private field the author wrote, so a `dead_code` expectation the
/// author wrote to silence a warning
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
    member_visibility: &TokenStream,
) -> TokenStream {
    let config_field = config_field_name.as_ref().map(|name| {
        quote! { #member_visibility #name: Config, }
    });

    // Generate iceoryx2 port-handle fields if ports are defined. The
    // processor's `outputs` / `inputs` fields are the layout-stable
    // `OutputWriter` / `InputMailboxes` handles; the engine patches
    // them up via `GeneratedProcessor::set_iceoryx2_resources` after
    // `from_config` returns.
    let ipc_input_field = if !schema.inputs.is_empty() {
        quote! { #member_visibility inputs: __streamlib_sdk::iceoryx2::InputMailboxes, }
    } else {
        quote! {}
    };

    let ipc_output_field = if !schema.outputs.is_empty() {
        quote! { #member_visibility outputs: __streamlib_sdk::iceoryx2::OutputWriter, }
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
            quote! { #(#attributes)* #member_visibility #name: #ty, }
        })
        .collect();

    quote! {
        #member_visibility struct Processor {
            #ipc_input_field
            #ipc_output_field
            #config_field
            #(#custom_field_defs)*
        }
    }
}

/// The expression minting a node type's identity: a built-in's class in
/// `tatolab.stream`, else the module path captured where the macro expands.
fn node_type_minted(built_in_node: bool, processor_class_short_name: &str) -> TokenStream {
    if built_in_node {
        quote! {
            __streamlib_sdk::descriptors::ProcessorClassImportPath::of_built_in_node(
                #processor_class_short_name,
            )
        }
    } else {
        quote! {
            __streamlib_sdk::descriptors::ProcessorClassImportPath::new(::core::module_path!())
                .expect("module_path! always names the enclosing module")
        }
    }
}

/// Generate Processor trait implementation from schema.
fn generate_processor_impl_from_schema(
    schema: &ProcessorSchema,
    member_visibility: &TokenStream,
    declared_meta_and_port_descriptions: &ProcessorDeclaredMetaAndPortDescriptions<'_>,
    node_type_minted: &TokenStream,
    config_type: &TokenStream,
    config_field_name: &Option<Ident>,
    custom_fields: &[CustomField],
) -> TokenStream {
    use streamlib_processor_schema::ProcessorSchemaExecution;

    let processor_class_short_name = &schema.name;

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
        generate_descriptor_from_schema(schema, declared_meta_and_port_descriptions, config_type);
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
            #member_visibility const NAME: &'static str = #processor_class_short_name;

            /// This node type's identity: its class in `tatolab.stream` for a
            /// built-in, else the module path captured where the macro expanded.
            #member_visibility fn processor_class_import_path()
                -> __streamlib_sdk::descriptors::ProcessorClassImportPath
            {
                #node_type_minted
            }

            /// Create a [`ProcessorSpec`](__streamlib_sdk::processors::ProcessorSpec)
            /// for adding this processor to a runtime.
            #member_visibility fn node(config: #config_type) -> __streamlib_sdk::processors::ProcessorSpec {
                __streamlib_sdk::processors::ProcessorSpec::new(
                    Self::processor_class_import_path(),
                    __streamlib_sdk::serde_json::to_value(&config)
                        .expect("Config serialization failed"),
                )
            }

            /// Returns the execution mode for this processor.
            #member_visibility fn execution_mode(&self) -> __streamlib_sdk::execution::ProcessExecution {
                #execution_variant
            }

            /// Returns a human-readable description of the execution mode.
            #member_visibility fn execution_mode_description(&self) -> &'static str {
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

/// The node's declared category and description and each port's description,
/// beside the schema the descriptor is emitted from: the schema carries no
/// category, and a `const` description has no text it could carry.
struct ProcessorDeclaredMetaAndPortDescriptions<'parsed> {
    node_description: Option<&'parsed DeclaredDescription>,
    category: Option<&'parsed str>,
    inputs: Vec<Option<&'parsed DeclaredDescription>>,
    outputs: Vec<Option<&'parsed DeclaredDescription>>,
}

impl<'parsed> ProcessorDeclaredMetaAndPortDescriptions<'parsed> {
    fn of(parsed: &'parsed ParsedProcessorAttr) -> Self {
        let of_ports = |ports: &'parsed [ParsedPort]| {
            ports.iter().map(|port| port.description.as_ref()).collect()
        };
        Self {
            node_description: parsed.description.as_ref(),
            category: parsed.category.as_deref(),
            inputs: of_ports(&parsed.inputs),
            outputs: of_ports(&parsed.outputs),
        }
    }
}

/// Generate descriptor method from schema.
///
/// `config_type` is the processor's `Config` type — the declared `config =`
/// path, or `EmptyConfig` where none was declared. Its JSON Schema is what the
/// descriptor carries.
fn generate_descriptor_from_schema(
    schema: &ProcessorSchema,
    declared_meta_and_port_descriptions: &ProcessorDeclaredMetaAndPortDescriptions<'_>,
    config_type: &TokenStream,
) -> TokenStream {
    let description =
        declared_description_str_expression(declared_meta_and_port_descriptions.node_description);
    let repository = "https://github.com/tatolab/streamlib";

    // iceoryx2-based input ports
    let ipc_input_ports: Vec<TokenStream> = schema
        .inputs
        .iter()
        .zip(&declared_meta_and_port_descriptions.inputs)
        .map(|(p, declared_description)| {
            let port_name = &p.name;
            let port_desc = declared_description_str_expression(*declared_description);
            let delivery_profile_tokens = match p.delivery_profile.as_deref() {
                Some(value) => quote! { ::std::option::Option::Some(#value.to_string()) },
                None => quote! { ::std::option::Option::None },
            };
            let audio_window_tokens = audio_window_contract_tokens(p.audio_window.as_ref());
            let family_tokens = port_family_declaration_tokens(&p.family);
            let borrow_quota_tokens = match p.borrow_quota {
                Some(quota) => quote! { ::std::option::Option::Some(#quota) },
                None => quote! { ::std::option::Option::None },
            };
            quote! {
                .with_input(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    required: true,
                    delivery_profile: #delivery_profile_tokens,
                    borrow_quota: #borrow_quota_tokens,
                    audio_window: #audio_window_tokens,
                    family: #family_tokens,
                    pin_default: ::std::option::Option::None,
                })
            }
        })
        .collect();

    // iceoryx2-based output ports
    let no_audio_window_tokens = audio_window_contract_tokens(None);
    let ipc_output_ports: Vec<TokenStream> = schema
        .outputs
        .iter()
        .zip(&declared_meta_and_port_descriptions.outputs)
        .map(|(p, declared_description)| {
            let port_name = &p.name;
            let port_desc = declared_description_str_expression(*declared_description);
            let family_tokens = port_family_declaration_tokens(&p.family);
            quote! {
                .with_output(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    required: true,
                    delivery_profile: ::std::option::Option::None,
                    borrow_quota: ::std::option::Option::None,
                    audio_window: #no_audio_window_tokens,
                    family: #family_tokens,
                    pin_default: ::std::option::Option::None,
                })
            }
        })
        .collect();

    // The config type's JSON Schema. Reached through the bound-carrying
    // trait rather than `schemars` directly, so a config type missing the
    // derive fails on a diagnostic that names the fix.
    let config_schema = quote! {
        .with_config_schema(
            <#config_type as __streamlib_sdk::descriptors::ProcessorConfigJsonSchema>
                ::processor_config_schema_document()
        )
    };

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

    let paced_by_real_time = schema
        .paced_by_real_time
        .then(|| quote! { .paced_by_real_time() });

    let category = declared_meta_and_port_descriptions
        .category
        .map(|category| quote! { .with_category(#category) });

    // `module_path!()` resolves here to `<author's module>::<authored type>`,
    // because this expansion lands inside the module the macro names after
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
                    #paced_by_real_time
                    #category
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

/// Emit an expression that rebuilds `family` as a `PortFamilyDeclaration`.
fn port_family_declaration_tokens(
    family: &streamlib_processor_schema::PortFamilyDeclaration,
) -> TokenStream {
    use streamlib_processor_schema::{
        ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
        ImageOutputPortFrameRate, OutputPortDeclaredNames,
    };

    let descriptors = quote! { __streamlib_sdk::descriptors };
    let family_ident = proc_macro2::Ident::new(
        family.family.declaration_name(),
        proc_macro2::Span::call_site(),
    );
    let strings = |values: &[String]| {
        quote! { ::std::vec![#(::std::string::String::from(#values)),*] }
    };
    let optional = |tokens: Option<TokenStream>| match tokens {
        Some(tokens) => quote! { ::std::option::Option::Some(#tokens) },
        None => quote! { ::std::option::Option::None },
    };
    let names = optional(family.names.as_ref().map(|names| match names {
        OutputPortDeclaredNames::Stated(names) => {
            let names = strings(names);
            quote! { #descriptors::OutputPortDeclaredNames::Stated(#names) }
        }
        OutputPortDeclaredNames::FromConfig { config_key } => quote! {
            #descriptors::OutputPortDeclaredNames::FromConfig {
                config_key: ::std::string::String::from(#config_key),
            }
        },
    }));
    let requires = strings(&family.requires);
    let codec = optional(
        family
            .codec
            .as_ref()
            .map(|codec| quote! { ::std::string::String::from(#codec) }),
    );
    let requires_codec = strings(&family.requires_codec);
    let rate = optional(family.rate.as_ref().map(|rate| match rate {
        ChannelsOutputPortSampleRate::Stated(rate) => {
            quote! { #descriptors::ChannelsOutputPortSampleRate::Stated(#rate) }
        }
        ChannelsOutputPortSampleRate::FromConfig { config_key } => quote! {
            #descriptors::ChannelsOutputPortSampleRate::FromConfig {
                config_key: ::std::string::String::from(#config_key),
            }
        },
    }));
    let frame_rate = optional(family.frame_rate.as_ref().map(|frame_rate| match frame_rate {
        ImageOutputPortFrameRate::Stated(frame_rate) => {
            let numerator = frame_rate.numerator;
            let denominator = frame_rate.denominator;
            quote! {
                #descriptors::ImageOutputPortFrameRate::Stated(#descriptors::ImageStreamFrameRate {
                    numerator: #numerator,
                    denominator: #denominator,
                })
            }
        }
        ImageOutputPortFrameRate::FromConfig { config_key } => quote! {
            #descriptors::ImageOutputPortFrameRate::FromConfig {
                config_key: ::std::string::String::from(#config_key),
            }
        },
    }));
    let dtype = optional(family.dtype.map(|dtype| match dtype {
        ChannelsOutputPortSampleType::F32 => {
            quote! { #descriptors::ChannelsOutputPortSampleType::F32 }
        }
        ChannelsOutputPortSampleType::F64 => {
            quote! { #descriptors::ChannelsOutputPortSampleType::F64 }
        }
    }));
    let formats = strings(&family.formats);
    let residency = optional(family.residency.map(|residency| match residency {
        ArrayOutputPortResidency::Gpu => quote! { #descriptors::ArrayOutputPortResidency::Gpu },
        ArrayOutputPortResidency::Cpu => quote! { #descriptors::ArrayOutputPortResidency::Cpu },
    }));
    quote! {
        #descriptors::PortFamilyDeclaration {
            family: #descriptors::PortFamily::#family_ident,
            names: #names,
            requires: #requires,
            codec: #codec,
            requires_codec: #requires_codec,
            rate: #rate,
            frame_rate: #frame_rate,
            dtype: #dtype,
            formats: #formats,
            residency: #residency,
            also_takes_families: ::std::vec::Vec::new(),
            family_from_config_key: ::std::option::Option::None,
            codec_from_config_key: ::std::option::Option::None,
            family_follows_input_port: ::std::option::Option::None,
        }
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
                 — a port states its own values or `match_device`"
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
mod processor_struct_emit_tests;
