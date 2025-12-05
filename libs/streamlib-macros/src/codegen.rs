// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Code generation for processor attribute macro
//!
//! Generates module wrapper with:
//! - `Processor` struct with public fields
//! - `InputLink` module with port markers
//! - `OutputLink` module with port markers
//! - Trait implementations (BaseProcessor, Processor)

use crate::analysis::{AnalysisResult, PortDirection};
use crate::attributes::ExecutionMode;
use proc_macro2::TokenStream;
use quote::quote;

/// Generate a module wrapping the processor and port markers.
pub fn generate_processor_module(analysis: &AnalysisResult) -> TokenStream {
    let module_name = &analysis.struct_name;

    let processor_struct = generate_processor_struct(analysis);
    let input_link_module = generate_input_link_module(analysis);
    let output_link_module = generate_output_link_module(analysis);
    let base_processor_impl = generate_base_processor_impl(analysis);
    let processor_impl = generate_processor_impl(analysis);

    let unsafe_send_impl = if analysis.processor_attrs.unsafe_send {
        quote! {
            unsafe impl Send for Processor {}
        }
    } else {
        quote! {}
    };

    quote! {
        #[allow(non_snake_case)]
        pub mod #module_name {
            use super::*;

            #processor_struct

            #input_link_module

            #output_link_module

            #base_processor_impl

            #processor_impl

            #unsafe_send_impl
        }
    }
}

/// Generate the Processor struct with public fields
fn generate_processor_struct(analysis: &AnalysisResult) -> TokenStream {
    let port_fields: Vec<TokenStream> = analysis
        .port_fields
        .iter()
        .map(|field| {
            let name = &field.field_name;
            let ty = &field.field_type;
            quote! { pub #name: #ty }
        })
        .collect();

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

    quote! {
        pub struct Processor {
            #(#port_fields,)*
            #config_field
            #(#state_fields,)*
        }
    }
}

/// Generate InputLink module with port markers
fn generate_input_link_module(analysis: &AnalysisResult) -> TokenStream {
    let input_ports: Vec<_> = analysis.input_ports().collect();

    if input_ports.is_empty() {
        return quote! { pub mod InputLink {} };
    }

    let markers: Vec<TokenStream> = input_ports
        .iter()
        .map(|port| {
            let name = &port.field_name;
            let port_name = &port.port_name;
            quote! {
                #[allow(non_camel_case_types)]
                #[derive(Debug, Clone, Copy)]
                pub struct #name;

                impl ::streamlib::core::InputPortMarker for #name {
                    const PORT_NAME: &'static str = #port_name;
                    type Processor = super::Processor;
                }
            }
        })
        .collect();

    quote! {
        pub mod InputLink {
            #(#markers)*
        }
    }
}

/// Generate OutputLink module with port markers
fn generate_output_link_module(analysis: &AnalysisResult) -> TokenStream {
    let output_ports: Vec<_> = analysis.output_ports().collect();

    if output_ports.is_empty() {
        return quote! { pub mod OutputLink {} };
    }

    let markers: Vec<TokenStream> = output_ports
        .iter()
        .map(|port| {
            let name = &port.field_name;
            let port_name = &port.port_name;
            quote! {
                #[allow(non_camel_case_types)]
                #[derive(Debug, Clone, Copy)]
                pub struct #name;

                impl ::streamlib::core::OutputPortMarker for #name {
                    const PORT_NAME: &'static str = #port_name;
                    type Processor = super::Processor;
                }
            }
        })
        .collect();

    quote! {
        pub mod OutputLink {
            #(#markers)*
        }
    }
}

/// Generate BaseProcessor trait implementation
fn generate_base_processor_impl(analysis: &AnalysisResult) -> TokenStream {
    let processor_name = analysis.struct_name.to_string();

    let has_inputs = analysis.input_ports().next().is_some();
    let has_outputs = analysis.output_ports().next().is_some();

    let processor_type = match (has_inputs, has_outputs) {
        (false, true) => quote! { ::streamlib::core::ProcessorType::Source },
        (true, false) => quote! { ::streamlib::core::ProcessorType::Sink },
        _ => quote! { ::streamlib::core::ProcessorType::Transform },
    };

    quote! {
        impl ::streamlib::core::BaseProcessor for Processor {
            fn name(&self) -> &str {
                #processor_name
            }

            fn processor_type(&self) -> ::streamlib::core::ProcessorType {
                #processor_type
            }

            fn descriptor(&self) -> Option<::streamlib::core::ProcessorDescriptor> {
                <Self as ::streamlib::core::Processor>::descriptor()
            }

            fn __generated_setup(&mut self, ctx: &::streamlib::core::RuntimeContext) -> ::streamlib::core::Result<()> {
                self.setup(ctx)
            }

            fn __generated_teardown(&mut self) -> ::streamlib::core::Result<()> {
                self.teardown()
            }
        }
    }
}

/// Generate Processor trait implementation
fn generate_processor_impl(analysis: &AnalysisResult) -> TokenStream {
    let config_type = analysis
        .config_field_type
        .as_ref()
        .map(|ty| quote! { #ty })
        .unwrap_or_else(|| quote! { ::streamlib::core::EmptyConfig });

    let from_config_body = generate_from_config(analysis);

    // Generate execution config based on parsed execution_mode
    let execution_variant = match &analysis.processor_attrs.execution_mode {
        Some(ExecutionMode::Continuous { interval_ms }) => {
            let interval = interval_ms.unwrap_or(0);
            quote! { ::streamlib::core::ProcessExecution::Continuous { interval_ms: #interval } }
        }
        Some(ExecutionMode::Reactive) => {
            quote! { ::streamlib::core::ProcessExecution::Reactive }
        }
        Some(ExecutionMode::Manual) => {
            quote! { ::streamlib::core::ProcessExecution::Manual }
        }
        // Default: Reactive (process() called when input data arrives)
        None => quote! { ::streamlib::core::ProcessExecution::Reactive },
    };

    let descriptor_impl = generate_descriptor(analysis);
    let get_output_port_type = generate_get_output_port_type(analysis);
    let get_input_port_type = generate_get_input_port_type(analysis);
    let add_link_output_data_writer = generate_add_link_output_data_writer(analysis);
    let add_link_input_data_reader = generate_add_link_input_data_reader(analysis);
    let remove_link_output_data_writer = generate_remove_link_output_data_writer(analysis);
    let remove_link_input_data_reader = generate_remove_link_input_data_reader(analysis);
    let set_link_output_to_processor_message_writer =
        generate_set_link_output_to_processor_message_writer(analysis);

    let update_config = analysis.config_field_name.as_ref().map(|name| {
        quote! {
            fn update_config(&mut self, config: Self::Config) -> ::streamlib::core::Result<()> {
                self.#name = config;
                Ok(())
            }
        }
    });

    // Generate execution mode description for debugging
    let execution_description = match &analysis.processor_attrs.execution_mode {
        Some(ExecutionMode::Continuous { interval_ms }) => {
            let interval = interval_ms.unwrap_or(0);
            if interval > 0 {
                format!("Continuous ({}ms interval)", interval)
            } else {
                "Continuous (no interval)".to_string()
            }
        }
        Some(ExecutionMode::Reactive) => "Reactive".to_string(),
        Some(ExecutionMode::Manual) => "Manual".to_string(),
        None => "Reactive (default)".to_string(),
    };

    quote! {
        impl Processor {
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

        impl ::streamlib::core::Processor for Processor {
            type Config = #config_type;

            #from_config_body

            fn process(&mut self) -> ::streamlib::core::Result<()> {
                self.process()
            }

            #update_config

            fn execution_config(&self) -> ::streamlib::core::ExecutionConfig {
                ::streamlib::core::ExecutionConfig {
                    execution: #execution_variant,
                    priority: ::streamlib::core::ThreadPriority::Normal,
                }
            }

            #descriptor_impl
            #get_output_port_type
            #get_input_port_type
            #add_link_output_data_writer
            #add_link_input_data_reader
            #remove_link_output_data_writer
            #remove_link_input_data_reader
            #set_link_output_to_processor_message_writer
        }
    }
}

/// Generate from_config method
fn generate_from_config(analysis: &AnalysisResult) -> TokenStream {
    let port_inits: Vec<TokenStream> = analysis
        .port_fields
        .iter()
        .map(|field| {
            let name = &field.field_name;
            let port_name = &field.port_name;
            let msg_type = &field.message_type;
            let is_arc = field.is_arc_wrapped;

            match field.direction {
                PortDirection::Input => {
                    if is_arc {
                        quote! { #name: std::sync::Arc::new(::streamlib::core::LinkInput::<#msg_type>::new(#port_name)) }
                    } else {
                        quote! { #name: ::streamlib::core::LinkInput::<#msg_type>::new(#port_name) }
                    }
                }
                PortDirection::Output => {
                    if is_arc {
                        quote! { #name: std::sync::Arc::new(::streamlib::core::LinkOutput::<#msg_type>::new(#port_name)) }
                    } else {
                        quote! { #name: ::streamlib::core::LinkOutput::<#msg_type>::new(#port_name) }
                    }
                }
            }
        })
        .collect();

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
                #(#port_inits,)*
                #config_init
                #(#state_inits,)*
            })
        }
    }
}

/// Generate descriptor method
fn generate_descriptor(analysis: &AnalysisResult) -> TokenStream {
    let name = analysis.struct_name.to_string();

    let desc = analysis
        .processor_attrs
        .description
        .as_deref()
        .unwrap_or("Processor");

    let input_ports: Vec<TokenStream> = analysis
        .input_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let msg_type = &p.message_type;
            let port_desc = p.attributes.description.as_deref().unwrap_or("");
            quote! {
                .with_input(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#msg_type as ::streamlib::core::links::LinkPortMessage>::schema(),
                    required: true,
                    description: #port_desc.to_string(),
                })
            }
        })
        .collect();

    let output_ports: Vec<TokenStream> = analysis
        .output_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let msg_type = &p.message_type;
            let port_desc = p.attributes.description.as_deref().unwrap_or("");
            quote! {
                .with_output(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#msg_type as ::streamlib::core::links::LinkPortMessage>::schema(),
                    required: true,
                    description: #port_desc.to_string(),
                })
            }
        })
        .collect();

    quote! {
        fn descriptor() -> Option<::streamlib::core::ProcessorDescriptor> {
            Some(
                ::streamlib::core::ProcessorDescriptor::new(#name, #desc)
                    #(#input_ports)*
                    #(#output_ports)*
            )
        }
    }
}

/// Generate get_output_port_type method
fn generate_get_output_port_type(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let msg_type = &p.message_type;
            quote! {
                #port_name => Some(<#msg_type as ::streamlib::core::links::LinkPortMessage>::port_type())
            }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn get_output_port_type(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
            match port_name {
                #(#arms,)*
                _ => None
            }
        }
    }
}

/// Generate get_input_port_type method
fn generate_get_input_port_type(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .input_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let msg_type = &p.message_type;
            quote! {
                #port_name => Some(<#msg_type as ::streamlib::core::links::LinkPortMessage>::port_type())
            }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn get_input_port_type(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
            match port_name {
                #(#arms,)*
                _ => None
            }
        }
    }
}

/// Generate add_link_output_data_writer method
fn generate_add_link_output_data_writer(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let field_name = &p.field_name;
            let msg_type = &p.message_type;
            let is_arc = p.is_arc_wrapped;

            let add_data_writer = if is_arc {
                quote! { self.#field_name.as_ref().add_data_writer(wrapper.link_id, wrapper.data_writer) }
            } else {
                quote! { self.#field_name.add_data_writer(wrapper.link_id, wrapper.data_writer) }
            };

            quote! {
                #port_name => {
                    if let Ok(wrapper) = data_writer.downcast::<::streamlib::core::compiler::wiring::LinkOutputDataWriterWrapper<#msg_type>>() {
                        let _ = #add_data_writer;
                        return Ok(());
                    }
                    Err(::streamlib::core::StreamError::PortError(format!("Type mismatch for output port '{}'", #port_name)))
                }
            }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn add_link_output_data_writer(&mut self, port_name: &str, data_writer: Box<dyn std::any::Any + Send>) -> ::streamlib::core::Result<()> {
            match port_name {
                #(#arms,)*
                _ => Err(::streamlib::core::StreamError::PortError(format!("Output port '{}' not found", port_name)))
            }
        }
    }
}

/// Generate add_link_input_data_reader method
fn generate_add_link_input_data_reader(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .input_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let field_name = &p.field_name;
            let msg_type = &p.message_type;
            let is_arc = p.is_arc_wrapped;

            let add_data_reader = if is_arc {
                quote! { self.#field_name.as_ref().add_data_reader(wrapper.link_id, wrapper.data_reader, None) }
            } else {
                quote! { self.#field_name.add_data_reader(wrapper.link_id, wrapper.data_reader, None) }
            };

            quote! {
                #port_name => {
                    if let Ok(wrapper) = data_reader.downcast::<::streamlib::core::compiler::wiring::LinkInputDataReaderWrapper<#msg_type>>() {
                        let _ = #add_data_reader;
                        return Ok(());
                    }
                    Err(::streamlib::core::StreamError::PortError(format!("Type mismatch for input port '{}'", #port_name)))
                }
            }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn add_link_input_data_reader(&mut self, port_name: &str, data_reader: Box<dyn std::any::Any + Send>) -> ::streamlib::core::Result<()> {
            match port_name {
                #(#arms,)*
                _ => Err(::streamlib::core::StreamError::PortError(format!("Input port '{}' not found", port_name)))
            }
        }
    }
}

/// Generate remove_link_output_data_writer method
fn generate_remove_link_output_data_writer(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let field_name = &p.field_name;
            let is_arc = p.is_arc_wrapped;

            let remove = if is_arc {
                quote! { self.#field_name.as_ref().remove_data_writer(link_id) }
            } else {
                quote! { self.#field_name.remove_data_writer(link_id) }
            };

            quote! { #port_name => { #remove } }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn remove_link_output_data_writer(&mut self, port_name: &str, link_id: &::streamlib::core::LinkId) -> ::streamlib::core::Result<()> {
            match port_name {
                #(#arms,)*
                _ => Err(::streamlib::core::StreamError::PortError(format!("Unknown output port: {}", port_name)))
            }
        }
    }
}

/// Generate remove_link_input_data_reader method
fn generate_remove_link_input_data_reader(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .input_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let field_name = &p.field_name;
            let is_arc = p.is_arc_wrapped;

            let remove = if is_arc {
                quote! { self.#field_name.as_ref().remove_data_reader(link_id) }
            } else {
                quote! { self.#field_name.remove_data_reader(link_id) }
            };

            quote! { #port_name => { #remove } }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn remove_link_input_data_reader(&mut self, port_name: &str, link_id: &::streamlib::core::LinkId) -> ::streamlib::core::Result<()> {
            match port_name {
                #(#arms,)*
                _ => Err(::streamlib::core::StreamError::PortError(format!("Unknown input port: {}", port_name)))
            }
        }
    }
}

/// Generate set_link_output_to_processor_message_writer method
fn generate_set_link_output_to_processor_message_writer(analysis: &AnalysisResult) -> TokenStream {
    let arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|p| {
            let port_name = &p.port_name;
            let field_name = &p.field_name;
            let is_arc = p.is_arc_wrapped;

            let set_writer = if is_arc {
                quote! { self.#field_name.as_ref().set_link_output_to_processor_message_writer(message_writer) }
            } else {
                quote! { self.#field_name.set_link_output_to_processor_message_writer(message_writer) }
            };

            quote! { #port_name => { #set_writer } }
        })
        .collect();

    if arms.is_empty() {
        return quote! {};
    }

    quote! {
        fn set_link_output_to_processor_message_writer(&mut self, port_name: &str, message_writer: ::streamlib::crossbeam_channel::Sender<::streamlib::core::links::LinkOutputToProcessorMessage>) {
            match port_name {
                #(#arms,)*
                _ => {}
            }
        }
    }
}
