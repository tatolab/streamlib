// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Code generation for processor attribute macro
//!
//! Generates module wrapper with:
//! - `Processor` struct with public fields
//! - `InputLink` module with port markers
//! - `OutputLink` module with port markers
//! - Processor trait implementation

use crate::analysis::AnalysisResult;
use proc_macro2::TokenStream;
use quote::quote;
use streamlib_codegen_shared::ProcessExecution;

/// Generate a module wrapping the processor and port markers.
pub fn generate_processor_module(analysis: &AnalysisResult) -> TokenStream {
    let module_name = &analysis.struct_name;

    let processor_struct = generate_processor_struct(analysis);
    let input_link_module = generate_input_link_module(analysis);
    let output_link_module = generate_output_link_module(analysis);
    let processor_impl = generate_processor_impl(analysis);

    let config_type = analysis
        .config_field_type
        .as_ref()
        .map(|ty| quote! { #ty })
        .unwrap_or_else(|| quote! { ::streamlib::core::EmptyConfig });

    let unsafe_send_impl = if analysis.processor_attrs.unsafe_send {
        quote! {
            unsafe impl Send for Processor {}
        }
    } else {
        quote! {}
    };

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

            #unsafe_send_impl

            #inventory_submit
        }
    }
}

/// Generate the Processor struct with public fields
fn generate_processor_struct(analysis: &AnalysisResult) -> TokenStream {
    // Legacy port fields removed - use iceoryx2 ports via inputs = [...] / outputs = [...] syntax

    let config_field = analysis
        .config_field_name
        .as_ref()
        .zip(analysis.config_field_type.as_ref())
        .map(|(name, ty)| quote! { pub #name: #ty, })
        .unwrap_or_default();

    let state_fields: Vec<TokenStream> = analysis
        .state_fields
        .iter()
        .map(|field| {
            let name = &field.field_name;
            let ty = &field.field_type;
            quote! { pub #name: #ty }
        })
        .collect();

    // Generate iceoryx2-based IPC fields if new port syntax is used
    let ipc_input_field = if !analysis.processor_attrs.inputs.is_empty() {
        quote! { pub inputs: ::streamlib::iceoryx2::InputMailboxes, }
    } else {
        quote! {}
    };

    let ipc_output_field = if !analysis.processor_attrs.outputs.is_empty() {
        quote! { pub outputs: ::streamlib::iceoryx2::OutputWriter, }
    } else {
        quote! {}
    };

    quote! {
        pub struct Processor {
            #ipc_input_field
            #ipc_output_field
            #config_field
            #(#state_fields,)*
        }
    }
}

/// Generate InputLink module with port markers (empty - legacy port markers removed)
fn generate_input_link_module(_analysis: &AnalysisResult) -> TokenStream {
    // Legacy port markers removed - use iceoryx2 ports via inputs = [...] syntax
    quote! { pub mod InputLink {} }
}

/// Generate OutputLink module with port markers (empty - legacy port markers removed)
fn generate_output_link_module(_analysis: &AnalysisResult) -> TokenStream {
    // Legacy port markers removed - use iceoryx2 ports via outputs = [...] syntax
    quote! { pub mod OutputLink {} }
}

/// Generate Processor trait implementation
fn generate_processor_impl(analysis: &AnalysisResult) -> TokenStream {
    // Use custom name from attribute, or fall back to struct name
    let processor_name = analysis
        .processor_attrs
        .name
        .clone()
        .unwrap_or_else(|| analysis.struct_name.to_string());

    let config_type = analysis
        .config_field_type
        .as_ref()
        .map(|ty| quote! { #ty })
        .unwrap_or_else(|| quote! { ::streamlib::core::EmptyConfig });

    let from_config_body = generate_from_config(analysis);

    // Generate execution config based on parsed execution_mode
    let execution_variant = match &analysis.processor_attrs.execution_mode {
        Some(ProcessExecution::Continuous { interval_ms }) => {
            quote! { ::streamlib::core::ProcessExecution::Continuous { interval_ms: #interval_ms } }
        }
        Some(ProcessExecution::Reactive) => {
            quote! { ::streamlib::core::ProcessExecution::Reactive }
        }
        Some(ProcessExecution::Manual) => {
            quote! { ::streamlib::core::ProcessExecution::Manual }
        }
        // Default: Reactive (process() called when input data arrives)
        None => quote! { ::streamlib::core::ProcessExecution::Reactive },
    };

    let descriptor_impl = generate_descriptor(analysis);
    // Legacy link methods removed - iceoryx2 ports use InputMailboxes/OutputWriter
    let iceoryx2_accessors = generate_iceoryx2_accessors(analysis);

    let update_config = analysis.config_field_name.as_ref().map(|name| {
        quote! {
            fn update_config(&mut self, config: Self::Config) -> ::streamlib::core::Result<()> {
                self.#name = config;
                Ok(())
            }
        }
    });

    // Generate execution mode description for debugging (uses Display impl)
    let execution_description = analysis
        .processor_attrs
        .execution_mode
        .as_ref()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "Reactive (default)".to_string());

    // Determine which mode-specific trait to use based on execution mode
    let processor_trait = match &analysis.processor_attrs.execution_mode {
        Some(ProcessExecution::Continuous { .. }) => {
            quote! { ::streamlib::core::ContinuousProcessor }
        }
        Some(ProcessExecution::Manual) => {
            quote! { ::streamlib::core::ManualProcessor }
        }
        Some(ProcessExecution::Reactive) | None => {
            quote! { ::streamlib::core::ReactiveProcessor }
        }
    };

    // Generate mode-specific implementations for process(), start(), and stop()
    // Manual mode: start()/stop() work, process() returns error
    // Continuous/Reactive: process() works, start()/stop() return error
    let (process_impl, start_impl, stop_impl) = match &analysis.processor_attrs.execution_mode {
        Some(ProcessExecution::Manual) => (
            quote! {
                Err(::streamlib::core::StreamError::Runtime(
                    "process() is not valid for Manual execution mode. Use start() instead.".into()
                ))
            },
            quote! {
                <Self as ::streamlib::core::ManualProcessor>::start(self)
            },
            quote! {
                <Self as ::streamlib::core::ManualProcessor>::stop(self)
            },
        ),
        Some(ProcessExecution::Continuous { .. }) => (
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
        ),
        Some(ProcessExecution::Reactive) | None => (
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
    };

    // Generate node() function - optionally extracts display_name from config field
    let node_fn = if let Some(field_name) = &analysis.processor_attrs.display_name_from_config {
        let field_ident = syn::Ident::new(field_name, proc_macro2::Span::call_site());
        quote! {
            /// Create a ProcessorSpec for adding this processor to a runtime.
            pub fn node(config: #config_type) -> ::streamlib::core::ProcessorSpec {
                let display_name = config.#field_ident.clone();
                ::streamlib::core::ProcessorSpec {
                    name: Self::NAME.to_string(),
                    config: ::streamlib::serde_json::to_value(&config)
                        .expect("Config serialization failed"),
                    display_name: Some(display_name),
                }
            }
        }
    } else {
        quote! {
            /// Create a ProcessorSpec for adding this processor to a runtime.
            pub fn node(config: #config_type) -> ::streamlib::core::ProcessorSpec {
                ::streamlib::core::ProcessorSpec {
                    name: Self::NAME.to_string(),
                    config: ::streamlib::serde_json::to_value(&config)
                        .expect("Config serialization failed"),
                    display_name: None,
                }
            }
        }
    };

    quote! {
        impl Processor {
            /// Processor name for registration and lookup.
            pub const NAME: &'static str = #processor_name;

            #node_fn

            /// Returns the execution mode for this processor.
            ///
            /// Useful for debugging and logging to understand when `process()` will be called.
            pub fn execution_mode(&self) -> ::streamlib::core::ProcessExecution {
                #execution_variant
            }

            /// Returns a human-readable description of the execution mode.
            ///
            /// Useful for debug output and logs.
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

/// Generate from_config method
fn generate_from_config(analysis: &AnalysisResult) -> TokenStream {
    // Legacy port_inits removed - use iceoryx2 ports via inputs = [...] / outputs = [...] syntax

    // Generate iceoryx2-based IPC field initializers
    let ipc_input_init = if !analysis.processor_attrs.inputs.is_empty() {
        let add_port_calls: Vec<TokenStream> = analysis
            .processor_attrs
            .inputs
            .iter()
            .map(|port| {
                let name = &port.name;
                let history = port.history.unwrap_or(1);
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

    let ipc_output_init = if !analysis.processor_attrs.outputs.is_empty() {
        // Note: For outputs, the dest_port is set during wiring, not at construction time.
        // We just store the schema for now; add_port will be called during wiring.
        quote! { outputs: ::streamlib::iceoryx2::OutputWriter::new(), }
    } else {
        quote! {}
    };

    let config_init = analysis
        .config_field_name
        .as_ref()
        .map(|_| quote! { config: config, })
        .unwrap_or_default();

    let state_inits: Vec<TokenStream> = analysis
        .state_fields
        .iter()
        .map(|field| {
            let name = &field.field_name;
            if let Some(expr) = &field.attributes.default_expr {
                let tokens: TokenStream = expr
                    .parse()
                    .unwrap_or_else(|_| quote! { Default::default() });
                quote! { #name: #tokens }
            } else {
                quote! { #name: Default::default() }
            }
        })
        .collect();

    quote! {
        fn from_config(config: Self::Config) -> ::streamlib::core::Result<Self> {
            Ok(Self {
                #ipc_input_init
                #ipc_output_init
                #config_init
                #(#state_inits,)*
            })
        }
    }
}

/// Generate descriptor method
fn generate_descriptor(analysis: &AnalysisResult) -> TokenStream {
    // Use custom name from attribute, or fall back to struct name
    let name = analysis
        .processor_attrs
        .name
        .clone()
        .unwrap_or_else(|| analysis.struct_name.to_string());

    let desc = analysis
        .processor_attrs
        .description
        .as_deref()
        .unwrap_or("Processor");

    // Legacy field-based input/output ports removed
    // Use iceoryx2 ports via inputs = [...] / outputs = [...] syntax

    // iceoryx2-based input ports (from processor attribute)
    let ipc_input_ports: Vec<TokenStream> = analysis
        .processor_attrs
        .inputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let schema = &p.schema;
            quote! {
                .with_input(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    description: String::new(),
                    schema: #schema.to_string(),
                    required: true,
                    is_iceoryx2: true,
                })
            }
        })
        .collect();

    // New iceoryx2-based output ports (from processor attribute)
    let ipc_output_ports: Vec<TokenStream> = analysis
        .processor_attrs
        .outputs
        .iter()
        .map(|p| {
            let port_name = &p.name;
            let schema = &p.schema;
            quote! {
                .with_output(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    description: String::new(),
                    schema: #schema.to_string(),
                    required: true,
                    is_iceoryx2: true,
                })
            }
        })
        .collect();

    // Default version and repository for built-in processors
    let version = "0.1.0";
    let repository = "https://github.com/tatolab/streamlib";

    // Generate config fields call if config type is available
    let config_fields = analysis.config_field_type.as_ref().map(|config_type| {
        quote! {
            .with_config(<#config_type as ::streamlib::core::ConfigDescriptor>::config_fields())
        }
    });

    quote! {
        fn descriptor() -> Option<::streamlib::core::ProcessorDescriptor> {
            Some(
                ::streamlib::core::ProcessorDescriptor::new(#name, #desc)
                    .with_version(#version)
                    .with_repository(#repository)
                    #config_fields
                    #(#ipc_input_ports)*
                    #(#ipc_output_ports)*
            )
        }
    }
}

/// Generate iceoryx2 accessor methods for processors that use iceoryx2 ports.
fn generate_iceoryx2_accessors(analysis: &AnalysisResult) -> TokenStream {
    let has_iceoryx2_outputs = !analysis.processor_attrs.outputs.is_empty();
    let has_iceoryx2_inputs = !analysis.processor_attrs.inputs.is_empty();

    if !has_iceoryx2_outputs && !has_iceoryx2_inputs {
        // No iceoryx2 ports, use default implementations (return false/None)
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
