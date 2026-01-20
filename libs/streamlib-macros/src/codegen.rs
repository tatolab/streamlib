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
#[allow(unused_imports)]
use streamlib_codegen_shared::ProcessExecution;
use streamlib_schema::ProcessorSchema;
use syn::ItemStruct;

// ============================================================================
// YAML-based code generation
// ============================================================================

/// Generate a processor module from a YAML ProcessorSchema.
pub fn generate_from_processor_schema(item: &ItemStruct, schema: &ProcessorSchema) -> TokenStream {
    let module_name = &item.ident;

    // Derive config type from schema reference if present
    let config_type = schema
        .config
        .as_ref()
        .map(|c| derive_config_type_from_schema(&c.schema))
        .unwrap_or_else(|| quote! { ::streamlib::core::EmptyConfig });

    let config_field_name = schema
        .config
        .as_ref()
        .map(|c| Ident::new(&c.name, Span::call_site()));

    let processor_struct = generate_processor_struct_from_schema(schema, &config_field_name);
    let input_link_module = generate_input_link_module_from_schema(schema);
    let output_link_module = generate_output_link_module_from_schema(schema);
    let processor_impl =
        generate_processor_impl_from_schema(schema, &config_type, &config_field_name);

    // Auto-registration via inventory crate
    let inventory_submit = quote! {
        ::streamlib::inventory::submit! {
            ::streamlib::core::processors::macro_codegen::FactoryRegistration {
                register_fn: |factory| factory.register::<Processor>(),
            }
        }
    };

    quote! {
        #[allow(non_snake_case)]
        pub mod #module_name {
            use super::*;

            /// Configuration type for this processor.
            pub type Config = #config_type;

            /// Create a [`ProcessorSpec`] for adding this processor to a runtime.
            ///
            /// Convenience wrapper around [`Processor::node`].
            pub fn node(config: Config) -> ::streamlib::core::ProcessorSpec {
                Processor::node(config)
            }

            #processor_struct

            #input_link_module

            #output_link_module

            #processor_impl

            #inventory_submit
        }
    }
}

/// Derive a Rust config type from a schema reference.
///
/// For "com.example.blur.config@1.0.0", derives "BlurConfig" as the type name.
/// The actual type must be defined by the user and match this name.
fn derive_config_type_from_schema(schema_ref: &str) -> TokenStream {
    // Extract the name part before @ (e.g., "com.example.blur.config")
    let name_part = schema_ref.split('@').next().unwrap_or(schema_ref);

    // Get the last segment (e.g., "config" from "com.example.blur.config")
    let last_segment = name_part.split('.').next_back().unwrap_or(name_part);

    // Convert to PascalCase (e.g., "blur_config" -> "BlurConfig")
    let pascal_name = to_pascal_case(last_segment);
    let ident = Ident::new(&pascal_name, Span::call_site());

    quote! { #ident }
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

/// Generate the Processor struct from schema.
fn generate_processor_struct_from_schema(
    schema: &ProcessorSchema,
    config_field_name: &Option<Ident>,
) -> TokenStream {
    let config_field = config_field_name.as_ref().map(|name| {
        quote! { pub #name: Config, }
    });

    // Generate iceoryx2-based IPC fields if ports are defined
    let ipc_input_field = if !schema.inputs.is_empty() {
        quote! { pub inputs: ::streamlib::iceoryx2::InputMailboxes, }
    } else {
        quote! {}
    };

    let ipc_output_field = if !schema.outputs.is_empty() {
        quote! { pub outputs: ::streamlib::iceoryx2::OutputWriter, }
    } else {
        quote! {}
    };

    // Generate state fields from schema
    let state_fields: Vec<TokenStream> = schema
        .state
        .iter()
        .map(|field| {
            let field_name = Ident::new(&field.name, Span::call_site());
            let field_type: TokenStream = field.field_type.parse().unwrap_or_else(|_| {
                // Fallback for complex types
                let ty = Ident::new(&field.field_type, Span::call_site());
                quote! { #ty }
            });
            quote! { pub #field_name: #field_type, }
        })
        .collect();

    quote! {
        pub struct Processor {
            #ipc_input_field
            #ipc_output_field
            #config_field
            #(#state_fields)*
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
                impl ::streamlib::core::InputPortMarker for #port_name {
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
                impl ::streamlib::core::OutputPortMarker for #port_name {
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
) -> TokenStream {
    use streamlib_schema::ProcessExecution;

    let processor_name = &schema.name;
    let description = schema.description.as_deref().unwrap_or("Processor");
    let version = &schema.version;

    // Derive execution mode from schema
    let (
        execution_variant,
        execution_description,
        processor_trait,
        process_impl,
        start_impl,
        stop_impl,
    ) = match &schema.execution {
        ProcessExecution::Reactive => (
            quote! { ::streamlib::core::ProcessExecution::Reactive },
            "Reactive",
            quote! { ::streamlib::core::ReactiveProcessor },
            quote! {
                <Self as ::streamlib::core::ReactiveProcessor>::process(self)
            },
            quote! {
                Err(::streamlib::core::StreamError::Runtime(
                    "start() is only valid for Manual execution mode.".into()
                ))
            },
            quote! {
                Err(::streamlib::core::StreamError::Runtime(
                    "stop() is only valid for Manual execution mode.".into()
                ))
            },
        ),
        ProcessExecution::Manual => (
            quote! { ::streamlib::core::ProcessExecution::Manual },
            "Manual",
            quote! { ::streamlib::core::ManualProcessor },
            quote! {
                Err(::streamlib::core::StreamError::Runtime(
                    "process() is only valid for Reactive/Continuous execution modes.".into()
                ))
            },
            quote! {
                <Self as ::streamlib::core::ManualProcessor>::start(self)
            },
            quote! {
                <Self as ::streamlib::core::ManualProcessor>::stop(self)
            },
        ),
        ProcessExecution::Continuous { interval_ms } => {
            let interval = *interval_ms;
            (
                quote! { ::streamlib::core::ProcessExecution::Continuous { interval_ms: #interval } },
                "Continuous",
                quote! { ::streamlib::core::ContinuousProcessor },
                quote! {
                    <Self as ::streamlib::core::ContinuousProcessor>::process(self)
                },
                quote! {
                    Err(::streamlib::core::StreamError::Runtime(
                        "start() is only valid for Manual execution mode.".into()
                    ))
                },
                quote! {
                    Err(::streamlib::core::StreamError::Runtime(
                        "stop() is only valid for Manual execution mode.".into()
                    ))
                },
            )
        }
    };

    let from_config_body = generate_from_config_from_schema(schema, config_field_name);
    let descriptor_impl = generate_descriptor_from_schema(schema, description, version);
    let iceoryx2_accessors = generate_iceoryx2_accessors_from_schema(schema);

    let update_config = config_field_name.as_ref().map(|name| {
        quote! {
            fn update_config(&mut self, config: Self::Config) -> ::streamlib::core::Result<()> {
                self.#name = config;
                Ok(())
            }
        }
    });

    quote! {
        impl Processor {
            /// Processor name for registration and lookup.
            pub const NAME: &'static str = #processor_name;

            /// Create a ProcessorSpec for adding this processor to a runtime.
            pub fn node(config: #config_type) -> ::streamlib::core::ProcessorSpec {
                ::streamlib::core::ProcessorSpec {
                    name: Self::NAME.to_string(),
                    config: ::streamlib::serde_json::to_value(&config)
                        .expect("Config serialization failed"),
                    display_name: None,
                }
            }

            /// Returns the execution mode for this processor.
            pub fn execution_mode(&self) -> ::streamlib::core::ProcessExecution {
                #execution_variant
            }

            /// Returns a human-readable description of the execution mode.
            pub fn execution_mode_description(&self) -> &'static str {
                #execution_description
            }
        }

        impl ::streamlib::core::__generated_private::GeneratedProcessor for Processor {
            type Config = #config_type;

            fn name(&self) -> &str {
                Self::NAME
            }

            #from_config_body

            fn process(&mut self) -> ::streamlib::core::Result<()> {
                #process_impl
            }

            fn start(&mut self) -> ::streamlib::core::Result<()> {
                #start_impl
            }

            fn stop(&mut self) -> ::streamlib::core::Result<()> {
                #stop_impl
            }

            #update_config

            fn execution_config(&self) -> ::streamlib::core::ExecutionConfig {
                ::streamlib::core::ExecutionConfig {
                    execution: #execution_variant,
                    priority: ::streamlib::core::ThreadPriority::Normal,
                }
            }

            #descriptor_impl
            #iceoryx2_accessors

            fn __generated_setup(&mut self, ctx: ::streamlib::core::RuntimeContext) -> impl ::std::future::Future<Output = ::streamlib::core::Result<()>> + Send {
                <Self as #processor_trait>::setup(self, ctx)
            }

            fn __generated_teardown(&mut self) -> impl ::std::future::Future<Output = ::streamlib::core::Result<()>> + Send {
                <Self as #processor_trait>::teardown(self)
            }

            fn __generated_on_pause(&mut self) -> impl ::std::future::Future<Output = ::streamlib::core::Result<()>> + Send {
                <Self as #processor_trait>::on_pause(self)
            }

            fn __generated_on_resume(&mut self) -> impl ::std::future::Future<Output = ::streamlib::core::Result<()>> + Send {
                <Self as #processor_trait>::on_resume(self)
            }
        }
    }
}

/// Generate from_config method from schema.
fn generate_from_config_from_schema(
    schema: &ProcessorSchema,
    config_field_name: &Option<Ident>,
) -> TokenStream {
    // Generate iceoryx2-based IPC field initializers
    let ipc_input_init = if !schema.inputs.is_empty() {
        let add_port_calls: Vec<TokenStream> = schema
            .inputs
            .iter()
            .map(|port| {
                let name = &port.name;
                let history = 1usize; // Default history depth
                quote! { inputs.add_port(#name, #history); }
            })
            .collect();
        quote! {
            inputs: {
                let mut inputs = ::streamlib::iceoryx2::InputMailboxes::new();
                #(#add_port_calls)*
                inputs
            },
        }
    } else {
        quote! {}
    };

    let ipc_output_init = if !schema.outputs.is_empty() {
        quote! { outputs: ::streamlib::iceoryx2::OutputWriter::new(), }
    } else {
        quote! {}
    };

    let config_init = config_field_name
        .as_ref()
        .map(|name| quote! { #name: config, })
        .unwrap_or_default();

    // Generate state field initializers
    let state_inits: Vec<TokenStream> = schema
        .state
        .iter()
        .map(|field| {
            let field_name = Ident::new(&field.name, Span::call_site());
            let default_expr: TokenStream = field
                .default
                .as_ref()
                .map(|d| d.parse().unwrap_or_else(|_| quote! { Default::default() }))
                .unwrap_or_else(|| quote! { Default::default() });
            quote! { #field_name: #default_expr, }
        })
        .collect();

    quote! {
        fn from_config(config: Self::Config) -> ::streamlib::core::Result<Self> {
            Ok(Self {
                #ipc_input_init
                #ipc_output_init
                #config_init
                #(#state_inits)*
            })
        }
    }
}

/// Generate descriptor method from schema.
fn generate_descriptor_from_schema(
    schema: &ProcessorSchema,
    description: &str,
    version: &str,
) -> TokenStream {
    let name = &schema.name;
    let repository = "https://github.com/tatolab/streamlib";

    // iceoryx2-based input ports
    let ipc_input_ports: Vec<TokenStream> = schema
        .inputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let port_schema = &p.schema;
            let port_desc = p.description.as_deref().unwrap_or("");
            quote! {
                .with_input(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    schema: #port_schema.to_string(),
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
            let port_schema = &p.schema;
            let port_desc = p.description.as_deref().unwrap_or("");
            quote! {
                .with_output(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    schema: #port_schema.to_string(),
                    required: true,
                    is_iceoryx2: true,
                })
            }
        })
        .collect();

    // Config schema reference (if present)
    let config_schema = schema.config.as_ref().map(|c| {
        let schema_ref = &c.schema;
        quote! {
            .with_config_schema(#schema_ref)
        }
    });

    quote! {
        fn descriptor() -> Option<::streamlib::core::ProcessorDescriptor> {
            Some(
                ::streamlib::core::ProcessorDescriptor::new(#name, #description)
                    .with_version(#version)
                    .with_repository(#repository)
                    #config_schema
                    #(#ipc_input_ports)*
                    #(#ipc_output_ports)*
            )
        }
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
            fn get_iceoryx2_output_writer(&mut self) -> Option<&mut ::streamlib::iceoryx2::OutputWriter> {
                Some(&mut self.outputs)
            }
        }
    } else {
        quote! {}
    };

    let get_input_mailboxes_impl = if has_iceoryx2_inputs {
        quote! {
            fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut ::streamlib::iceoryx2::InputMailboxes> {
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
