//! Code generation for Processor derive macro
//!
//! Generates:
//! - Config struct (or uses existing/EmptyConfig)
//! - from_config() method implementation
//! - descriptor() method with type-safe schemas
//! - as_any_mut() downcasting method
//!
//! Smart defaults automatically generate:
//! - Descriptions from struct/port names
//! - Usage context from port configuration
//! - Tags from port types and processor category
//! - Examples from LinkPortMessage::examples()
//! - Audio requirements for audio processors

// TODO(@jonathan): Review unused code generation functions - many appear to be from old macro implementation
// Functions like generate_config_struct(), generate_descriptor(), generate_tags(), humanize_*() are unused
// Consider removing if not needed for future features
#![allow(dead_code)]

use crate::analysis::{AnalysisResult, PortDirection, PortField};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::Type;

/// Generate all code for the Processor implementation
pub fn generate_processor_impl(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;

    // Generate view structs and ports() method
    let view_structs = generate_ports_view_structs(analysis);
    let ports_method = generate_ports_convenience_method(analysis);

    // Generate port introspection methods (these are added to existing impls)
    let port_introspection = generate_port_introspection_methods(analysis);

    // Generate port marker types for compile-time safe port access
    let port_markers = generate_port_marker_types(analysis);

    // Always generate complete trait implementations
    let stream_element_impl = generate_stream_element_impl(analysis);
    let stream_processor_impl = generate_stream_processor_impl(analysis);

    // Optionally generate unsafe impl Send
    let unsafe_send_impl = if analysis.processor_attrs.unsafe_send {
        quote! {
            // SAFETY: Type contains !Send fields (e.g., hardware resources) that are safe to send between threads
            unsafe impl Send for #struct_name {}
        }
    } else {
        quote! {}
    };

    quote! {
        #view_structs

        #port_markers

        impl #struct_name {
            #ports_method
        }

        // Generate extension impl with port introspection methods
        impl #struct_name {
            #port_introspection
        }

        // Generate complete BaseProcessor implementation
        #stream_element_impl

        // Generate complete Processor implementation
        #stream_processor_impl

        // Generate unsafe impl Send if requested
        #unsafe_send_impl
    }
}

/// Generate port marker types for compile-time safe port access
///
/// For a processor like:
/// ```ignore
/// #[derive(Processor)]
/// struct CameraProcessor {
///     #[output] video: LinkOutput<VideoFrame>,
///     #[output] thumbnail: LinkOutput<VideoFrame>,
/// }
/// ```
///
/// Generates:
/// ```ignore
/// impl CameraProcessor {
///     pub mod outputs {
///         pub struct video;
///         pub struct thumbnail;
///     }
/// }
///
/// impl ::streamlib::core::OutputPortMarker for CameraProcessor::outputs::video {
///     const PORT_NAME: &'static str = "video";
///     type Processor = CameraProcessor;
/// }
/// ```
fn generate_port_marker_types(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;

    let input_ports: Vec<_> = analysis.input_ports().collect();
    let output_ports: Vec<_> = analysis.output_ports().collect();

    // Generate input marker structs
    // Allow non-camel-case since port names like "video" become struct names
    let input_markers: Vec<TokenStream> = input_ports
        .iter()
        .map(|port| {
            let field_name = &port.field_name;
            quote! {
                #[allow(non_camel_case_types)]
                #[derive(Debug, Clone, Copy)]
                pub struct #field_name;
            }
        })
        .collect();

    // Generate output marker structs
    // Allow non-camel-case since port names like "video" become struct names
    let output_markers: Vec<TokenStream> = output_ports
        .iter()
        .map(|port| {
            let field_name = &port.field_name;
            quote! {
                #[allow(non_camel_case_types)]
                #[derive(Debug, Clone, Copy)]
                pub struct #field_name;
            }
        })
        .collect();

    // Generate InputPortMarker trait impls
    let input_marker_impls: Vec<TokenStream> = input_ports
        .iter()
        .map(|port| {
            let field_name = &port.field_name;
            let port_name = &port.port_name;
            quote! {
                impl ::streamlib::core::InputPortMarker for inputs::#field_name {
                    const PORT_NAME: &'static str = #port_name;
                    type Processor = #struct_name;
                }
            }
        })
        .collect();

    // Generate OutputPortMarker trait impls
    let output_marker_impls: Vec<TokenStream> = output_ports
        .iter()
        .map(|port| {
            let field_name = &port.field_name;
            let port_name = &port.port_name;
            quote! {
                impl ::streamlib::core::OutputPortMarker for outputs::#field_name {
                    const PORT_NAME: &'static str = #port_name;
                    type Processor = #struct_name;
                }
            }
        })
        .collect();

    // Only generate modules if there are ports
    let inputs_mod = if !input_markers.is_empty() {
        quote! {
            /// Input port markers for compile-time safe connections
            pub mod inputs {
                #(#input_markers)*
            }
        }
    } else {
        quote! {
            /// Input port markers (none defined)
            pub mod inputs {}
        }
    };

    let outputs_mod = if !output_markers.is_empty() {
        quote! {
            /// Output port markers for compile-time safe connections
            pub mod outputs {
                #(#output_markers)*
            }
        }
    } else {
        quote! {
            /// Output port markers (none defined)
            pub mod outputs {}
        }
    };

    quote! {
        #inputs_mod
        #outputs_mod
        #(#input_marker_impls)*
        #(#output_marker_impls)*
    }
}

/// Generate config struct
///
/// Three cases:
/// 1. User specified custom config type -> use that (don't generate)
/// 2. Has config fields -> generate Config struct
/// 3. No config fields -> use EmptyConfig
fn generate_config_struct(analysis: &AnalysisResult) -> TokenStream {
    // Case 1: User specified custom config type
    if analysis.processor_attrs.config_type.is_some() {
        return quote! {};
    }

    // Case 2: Has config fields -> generate struct
    if !analysis.config_fields.is_empty() {
        let field_defs: Vec<TokenStream> = analysis
            .config_fields
            .iter()
            .map(|field| {
                let name = &field.field_name;
                let ty = &field.field_type;
                quote! { pub #name: #ty }
            })
            .collect();

        let field_names: Vec<&proc_macro2::Ident> = analysis
            .config_fields
            .iter()
            .map(|field| &field.field_name)
            .collect();

        return quote! {
            #[derive(Debug, Clone)]
            pub struct Config {
                #(#field_defs),*
            }

            impl Default for Config {
                fn default() -> Self {
                    Self {
                        #(#field_names: Default::default()),*
                    }
                }
            }
        };
    }

    // Case 3: No config fields -> use EmptyConfig (already defined)
    quote! {
        pub type Config = ::streamlib::core::EmptyConfig;
    }
}

/// Generate from_config() method body
fn generate_from_config_body(analysis: &AnalysisResult) -> TokenStream {
    // Generate port construction
    let port_constructions: Vec<TokenStream> = analysis
        .port_fields
        .iter()
        .map(|field| {
            let field_name = &field.field_name;
            let port_name = &field.port_name;
            let message_type = &field.message_type;

            match field.direction {
                PortDirection::Input => {
                    quote! {
                        #field_name: ::streamlib::core::LinkInput::<#message_type>::new(#port_name)
                    }
                }
                PortDirection::Output => {
                    quote! {
                        #field_name: ::streamlib::core::LinkOutput::<#message_type>::new(#port_name)
                    }
                }
            }
        })
        .collect();

    // Generate config field assignments
    let config_assignments: Vec<TokenStream> = analysis
        .config_fields
        .iter()
        .map(|field| {
            let field_name = &field.field_name;
            quote! { #field_name: config.#field_name }
        })
        .collect();

    quote! {
        Ok(Self {
            #(#port_constructions,)*
            #(#config_assignments,)*
        })
    }
}

/// Generate descriptor() method with type-safe schemas
fn generate_descriptor(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;

    // Description: use attribute or generate smart default
    let description = analysis
        .processor_attrs
        .description
        .clone()
        .unwrap_or_else(|| generate_description(analysis));

    // Usage context: use attribute or generate smart default
    let usage_context = analysis
        .processor_attrs
        .usage_context
        .clone()
        .unwrap_or_else(|| generate_usage_context(analysis));

    // Tags: use attribute or generate smart defaults
    let tags = if analysis.processor_attrs.tags.is_empty() {
        generate_tags(analysis)
    } else {
        analysis.processor_attrs.tags.clone()
    };

    // Generate input port descriptors
    let input_ports: Vec<TokenStream> = analysis
        .input_ports()
        .map(|field| generate_port_descriptor(field, "with_input"))
        .collect();

    // Generate output port descriptors
    let output_ports: Vec<TokenStream> = analysis
        .output_ports()
        .map(|field| generate_port_descriptor(field, "with_output"))
        .collect();

    // Audio requirements: use attribute or auto-detect
    let audio_requirements = if let Some(reqs) = &analysis.processor_attrs.audio_requirements {
        quote! {
            .with_audio_requirements(::streamlib::core::AudioRequirements {
                #reqs
            })
        }
    } else if analysis.has_audio_ports() {
        quote! {
            .with_audio_requirements(::streamlib::core::AudioRequirements::default())
        }
    } else {
        quote! {}
    };

    quote! {
        fn descriptor() -> Option<::streamlib::core::ProcessorDescriptor> {
            Some(
                ::streamlib::core::ProcessorDescriptor::new(
                    stringify!(#struct_name),
                    #description
                )
                .with_usage_context(#usage_context)
                .with_tags(vec![#(#tags.to_string()),*])
                #(#input_ports)*
                #(#output_ports)*
                #audio_requirements
            )
        }
    }
}

/// Generate a port descriptor with type-safe schema extraction
fn generate_port_descriptor(field: &PortField, method_name: &str) -> TokenStream {
    let port_name = &field.port_name;
    let message_type = &field.message_type;
    let method = format_ident!("{}", method_name);

    // Description: use attribute or generate from field name
    let description = field
        .attributes
        .description
        .clone()
        .unwrap_or_else(|| humanize_field_name(&field.field_name));

    // Required flag (only for inputs)
    let required = if field.direction == PortDirection::Input {
        field.attributes.required.unwrap_or(false)
    } else {
        false
    };

    // Type-safe schema extraction: T::schema()
    // Create a PortDescriptor object and pass it to with_input/with_output
    quote! {
        .#method(::streamlib::core::PortDescriptor::new(
            #port_name,
            <#message_type as ::streamlib::core::LinkPortMessage>::schema(),
            #required,
            #description
        ))
    }
}

/// Generate as_any_mut() for downcasting
fn generate_as_any_mut(_struct_name: &Ident) -> TokenStream {
    quote! {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }
}

// === Smart Defaults ===

/// Generate smart description from struct name and ports
fn generate_description(analysis: &AnalysisResult) -> String {
    let struct_name_str = analysis.struct_name.to_string();
    let readable_name = humanize_struct_name(&struct_name_str);

    // Count ports
    let input_count = analysis.input_ports().count();
    let output_count = analysis.output_ports().count();

    // Categorize
    if input_count == 0 && output_count > 0 {
        format!("{} source processor", readable_name)
    } else if input_count > 0 && output_count == 0 {
        format!("{} sink processor", readable_name)
    } else if input_count == 1 && output_count == 1 {
        format!("{} effect processor", readable_name)
    } else {
        format!("{} processor", readable_name)
    }
}

/// Generate smart usage context from port configuration
fn generate_usage_context(analysis: &AnalysisResult) -> String {
    let inputs: Vec<String> = analysis
        .input_ports()
        .map(|f| format!("{} ({})", f.port_name, type_name(&f.message_type)))
        .collect();

    let outputs: Vec<String> = analysis
        .output_ports()
        .map(|f| format!("{} ({})", f.port_name, type_name(&f.message_type)))
        .collect();

    let mut parts = Vec::new();

    if !inputs.is_empty() {
        parts.push(format!("Inputs: {}", inputs.join(", ")));
    }

    if !outputs.is_empty() {
        parts.push(format!("Outputs: {}", outputs.join(", ")));
    }

    parts.join(". ")
}

/// Generate smart tags from port types and processor category
fn generate_tags(analysis: &AnalysisResult) -> Vec<String> {
    let mut tags = Vec::new();

    // Add port type tags
    let has_video = analysis
        .port_fields
        .iter()
        .any(|f| type_name(&f.message_type) == "VideoFrame");
    let has_audio = analysis.has_audio_ports();
    let has_data = analysis
        .port_fields
        .iter()
        .any(|f| type_name(&f.message_type) == "DataMessage");

    if has_video {
        tags.push("video".to_string());
    }
    if has_audio {
        tags.push("audio".to_string());
    }
    if has_data {
        tags.push("data".to_string());
    }

    // Add category tag
    let input_count = analysis.input_ports().count();
    let output_count = analysis.output_ports().count();

    if input_count == 0 && output_count > 0 {
        tags.push("source".to_string());
    } else if input_count > 0 && output_count == 0 {
        tags.push("sink".to_string());
    } else if input_count > 0 && output_count > 0 {
        tags.push("effect".to_string());
    }

    tags
}

// === Helper Functions ===

/// Convert field name to human-readable description
fn humanize_field_name(ident: &Ident) -> String {
    let name = ident.to_string();
    let words: Vec<&str> = name.split('_').collect();
    words
        .iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convert struct name to human-readable name
fn humanize_struct_name(name: &str) -> String {
    // Remove "Processor" suffix if present
    let base = name.strip_suffix("Processor").unwrap_or(name);

    // Split on uppercase letters
    let mut result = String::new();
    for (i, c) in base.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            result.push(' ');
        }
        result.push(c);
    }

    result.to_lowercase()
}

/// Get simple type name from Type (e.g., "VideoFrame" from path)
fn type_name(ty: &Type) -> String {
    if let syn::Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident.to_string();
        }
    }
    "Unknown".to_string()
}

/// Generate view structs for the ports() convenience method
fn generate_ports_view_structs(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;
    let ports_struct_name = format_ident!("{}Ports", struct_name);
    let input_ports_struct_name = format_ident!("{}InputPorts", struct_name);
    let output_ports_struct_name = format_ident!("{}OutputPorts", struct_name);

    let input_ports: Vec<_> = analysis.input_ports().collect();
    let output_ports: Vec<_> = analysis.output_ports().collect();

    // Generate input ports view struct
    let input_fields = input_ports.iter().map(|p| {
        let name = &p.field_name;
        if p.is_arc_wrapped {
            // Arc-wrapped: &'a Arc<LinkInput<T>>
            let field_type = &p.field_type;
            quote! { pub #name: &'a #field_type }
        } else {
            // Normal: &'a LinkInput<T>
            let message_type = &p.message_type;
            quote! { pub #name: &'a ::streamlib::core::LinkInput<#message_type> }
        }
    });

    let input_struct = if !input_ports.is_empty() {
        quote! {
            pub struct #input_ports_struct_name<'a> {
                #(#input_fields),*
            }
        }
    } else {
        quote! {
            pub struct #input_ports_struct_name<'a> {
                _phantom: std::marker::PhantomData<&'a ()>,
            }
        }
    };

    // Generate output ports view struct
    let output_fields = output_ports.iter().map(|p| {
        let name = &p.field_name;
        if p.is_arc_wrapped {
            // Arc-wrapped: &'a Arc<LinkOutput<T>>
            let field_type = &p.field_type;
            quote! { pub #name: &'a #field_type }
        } else {
            // Normal: &'a LinkOutput<T>
            let message_type = &p.message_type;
            quote! { pub #name: &'a ::streamlib::core::LinkOutput<#message_type> }
        }
    });

    let output_struct = if !output_ports.is_empty() {
        quote! {
            pub struct #output_ports_struct_name<'a> {
                #(#output_fields),*
            }
        }
    } else {
        quote! {
            pub struct #output_ports_struct_name<'a> {
                _phantom: std::marker::PhantomData<&'a ()>,
            }
        }
    };

    // Generate main ports view struct
    let ports_struct = quote! {
        pub struct #ports_struct_name<'a> {
            pub inputs: #input_ports_struct_name<'a>,
            pub outputs: #output_ports_struct_name<'a>,
        }
    };

    quote! {
        #input_struct
        #output_struct
        #ports_struct
    }
}

/// Generate the ports() convenience method
fn generate_ports_convenience_method(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;
    let ports_struct_name = format_ident!("{}Ports", struct_name);
    let input_ports_struct_name = format_ident!("{}InputPorts", struct_name);
    let output_ports_struct_name = format_ident!("{}OutputPorts", struct_name);

    let input_ports: Vec<_> = analysis.input_ports().collect();
    let output_ports: Vec<_> = analysis.output_ports().collect();

    // Generate input field initialization
    let input_field_inits = input_ports.iter().map(|p| {
        let name = &p.field_name;
        quote! { #name: &self.#name }
    });

    let input_init = if !input_ports.is_empty() {
        quote! {
            #input_ports_struct_name {
                #(#input_field_inits),*
            }
        }
    } else {
        quote! {
            #input_ports_struct_name {
                _phantom: std::marker::PhantomData,
            }
        }
    };

    // Generate output field initialization
    let output_field_inits = output_ports.iter().map(|p| {
        let name = &p.field_name;
        quote! { #name: &self.#name }
    });

    let output_init = if !output_ports.is_empty() {
        quote! {
            #output_ports_struct_name {
                #(#output_field_inits),*
            }
        }
    } else {
        quote! {
            #output_ports_struct_name {
                _phantom: std::marker::PhantomData,
            }
        }
    };

    quote! {
        pub fn ports(&self) -> #ports_struct_name<'_> {
            #ports_struct_name {
                inputs: #input_init,
                outputs: #output_init,
            }
        }
    }
}

/// Generate port introspection methods for MCP server compatibility
fn generate_port_introspection_methods(analysis: &AnalysisResult) -> TokenStream {
    let input_ports: Vec<_> = analysis.input_ports().collect();
    let output_ports: Vec<_> = analysis.output_ports().collect();

    // Generate get_input_port_type implementation
    let input_port_type_arms = input_ports.iter().map(|p| {
        let port_name = &p.port_name;
        let message_type = &p.message_type;
        quote! {
            #port_name => Some(<#message_type as ::streamlib::core::LinkPortMessage>::port_type())
        }
    });

    let get_input_port_type_impl = if !input_ports.is_empty() {
        quote! {
            pub fn get_input_port_type_impl(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                match port_name {
                    #(#input_port_type_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {
            pub fn get_input_port_type_impl(&self, _port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                None
            }
        }
    };

    // Generate get_output_port_type implementation
    let output_port_type_arms = output_ports.iter().map(|p| {
        let port_name = &p.port_name;
        let message_type = &p.message_type;
        quote! {
            #port_name => Some(<#message_type as ::streamlib::core::LinkPortMessage>::port_type())
        }
    });

    let get_output_port_type_impl = if !output_ports.is_empty() {
        quote! {
            pub fn get_output_port_type_impl(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                match port_name {
                    #(#output_port_type_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {
            pub fn get_output_port_type_impl(&self, _port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                None
            }
        }
    };

    // Generate wire_input_consumer implementation
    let input_wire_arms = input_ports.iter().map(|p| {
        let field_name = &p.field_name;
        let port_name = &p.port_name;
        let message_type = &p.message_type;

        quote! {
            #port_name => {
                if let Ok(typed_consumer) = consumer.downcast::<::streamlib::core::LinkOwnedConsumer<#message_type>>() {
                    let temp_id = ::streamlib::core::link_channel::link_id::__private::new_unchecked(
                        format!("{}.wire_compat_{}", #port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                    );
                    let (tx, _rx) = ::crossbeam_channel::bounded(1);
                    let source_addr = ::streamlib::core::LinkPortAddress::new("unknown", #port_name);
                    let _ = self.#field_name.add_link(temp_id, *typed_consumer, source_addr, tx);
                    return true;
                }
                false
            }
        }
    });

    let wire_input_consumer_impl = if !input_ports.is_empty() {
        quote! {
            pub fn wire_input_consumer_impl(&mut self, port_name: &str, consumer: Box<dyn std::any::Any + Send>) -> bool {
                match port_name {
                    #(#input_wire_arms,)*
                    _ => false,
                }
            }

            // Backward compatibility alias
            pub fn wire_input_connection_impl(&mut self, port_name: &str, consumer: Box<dyn std::any::Any + Send>) -> bool {
                self.wire_input_consumer_impl(port_name, consumer)
            }
        }
    } else {
        quote! {
            pub fn wire_input_consumer_impl(&mut self, _port_name: &str, _consumer: Box<dyn std::any::Any + Send>) -> bool {
                false
            }

            pub fn wire_input_connection_impl(&mut self, _port_name: &str, _consumer: Box<dyn std::any::Any + Send>) -> bool {
                false
            }
        }
    };

    // Generate wire_output_producer implementation
    let output_wire_arms = output_ports.iter().map(|p| {
        let field_name = &p.field_name;
        let port_name = &p.port_name;
        let message_type = &p.message_type;

        quote! {
            #port_name => {
                if let Ok(typed_producer) = producer.downcast::<::streamlib::core::LinkOwnedProducer<#message_type>>() {
                    let temp_id = ::streamlib::core::link_channel::link_id::__private::new_unchecked(
                        format!("{}.wire_compat_{}", #port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                    );
                    let (tx, _rx) = ::crossbeam_channel::bounded(1);
                    let _ = self.#field_name.add_link(temp_id, *typed_producer, tx);
                    return true;
                }
                false
            }
        }
    });

    let wire_output_producer_impl = if !output_ports.is_empty() {
        quote! {
            pub fn wire_output_producer_impl(&mut self, port_name: &str, producer: Box<dyn std::any::Any + Send>) -> bool {
                match port_name {
                    #(#output_wire_arms,)*
                    _ => false,
                }
            }

            // Backward compatibility alias
            pub fn wire_output_connection_impl(&mut self, port_name: &str, producer: Box<dyn std::any::Any + Send>) -> bool {
                self.wire_output_producer_impl(port_name, producer)
            }
        }
    } else {
        quote! {
            pub fn wire_output_producer_impl(&mut self, _port_name: &str, _producer: Box<dyn std::any::Any + Send>) -> bool {
                false
            }

            pub fn wire_output_connection_impl(&mut self, _port_name: &str, _producer: Box<dyn std::any::Any + Send>) -> bool {
                false
            }
        }
    };

    // Generate Processor trait method implementations that delegate to the _impl methods
    let has_inputs = !input_ports.is_empty();
    let has_outputs = !output_ports.is_empty();

    let wire_input_consumer_trait = if has_inputs {
        quote! {
            fn wire_input_consumer(&mut self, port_name: &str, consumer: Box<dyn std::any::Any + Send>) -> bool {
                self.wire_input_consumer_impl(port_name, consumer)
            }
        }
    } else {
        quote! {}
    };

    let wire_output_producer_trait = if has_outputs {
        quote! {
            fn wire_output_producer(&mut self, port_name: &str, producer: Box<dyn std::any::Any + Send>) -> bool {
                self.wire_output_producer_impl(port_name, producer)
            }
        }
    } else {
        quote! {}
    };

    let get_input_port_type_trait = if has_inputs {
        quote! {
            fn get_input_port_type(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                self.get_input_port_type_impl(port_name)
            }
        }
    } else {
        quote! {}
    };

    let get_output_port_type_trait = if has_outputs {
        quote! {
            fn get_output_port_type(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                self.get_output_port_type_impl(port_name)
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #get_input_port_type_impl
        #get_output_port_type_impl
        #wire_input_consumer_impl
        #wire_output_producer_impl
        #get_input_port_type_trait
        #get_output_port_type_trait
        #wire_input_consumer_trait
        #wire_output_producer_trait
    }
}

/// Generate complete BaseProcessor trait implementation
///
/// This generates all methods of the BaseProcessor trait, eliminating the need for
/// users to manually implement the trait.
pub fn generate_stream_element_impl(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;

    // Determine the processor name (from attribute or struct name)
    let processor_name = analysis
        .processor_attrs
        .processor_name
        .as_ref()
        .cloned()
        .unwrap_or_else(|| struct_name.to_string());

    // Determine processor type from port configuration
    let has_inputs = !analysis.input_ports().collect::<Vec<_>>().is_empty();
    let has_outputs = !analysis.output_ports().collect::<Vec<_>>().is_empty();

    let processor_type = match (has_inputs, has_outputs) {
        (false, true) => quote! { ::streamlib::core::ProcessorType::Source },
        (true, false) => quote! { ::streamlib::core::ProcessorType::Sink },
        (true, true) => quote! { ::streamlib::core::ProcessorType::Transform },
        (false, false) => {
            // No ports - treat as transform (unusual case)
            quote! { ::streamlib::core::ProcessorType::Transform }
        }
    };

    // Generate descriptor call (delegates to Processor::descriptor)
    let descriptor_impl = quote! {
        fn descriptor(&self) -> Option<::streamlib::core::ProcessorDescriptor> {
            <Self as ::streamlib::core::Processor>::descriptor()
        }
    };

    let setup_method = analysis
        .processor_attrs
        .on_start_method
        .as_ref()
        .map(|s| format_ident!("{}", s))
        .unwrap_or_else(|| format_ident!("setup"));

    let setup_impl = quote! {
        fn __generated_setup(&mut self, ctx: &::streamlib::core::RuntimeContext) -> ::streamlib::core::Result<()> {
            self.#setup_method(ctx)
        }
    };

    let teardown_method = analysis
        .processor_attrs
        .on_stop_method
        .as_ref()
        .map(|s| format_ident!("{}", s))
        .unwrap_or_else(|| format_ident!("teardown"));

    let teardown_impl = quote! {
        fn __generated_teardown(&mut self) -> ::streamlib::core::Result<()> {
            self.#teardown_method()
        }
    };

    quote! {
        impl ::streamlib::core::BaseProcessor for #struct_name {
            fn name(&self) -> &str {
                #processor_name
            }

            fn processor_type(&self) -> ::streamlib::core::ProcessorType {
                #processor_type
            }

            #descriptor_impl

            #setup_impl

            #teardown_impl
        }
    }
}

/// Generate complete Processor trait implementation
///
/// This generates all methods of the Processor trait, eliminating the need for
/// users to manually implement the trait.
pub fn generate_stream_processor_impl(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;

    // Determine config type - prefer #[config] field type, fall back to processor attribute, then EmptyConfig
    let config_type = if let Some(config_ty) = &analysis.config_field_type {
        // Use type from #[config] field
        quote! { #config_ty }
    } else if let Some(config_ty) = &analysis.processor_attrs.config_type {
        // Fall back to processor attribute (backward compat)
        quote! { #config_ty }
    } else if !analysis.config_fields.is_empty() {
        // Legacy pattern with config fields - would need to generate Config struct
        // For now, default to EmptyConfig
        quote! { ::streamlib::core::EmptyConfig }
    } else {
        // No config at all - use EmptyConfig
        quote! { ::streamlib::core::EmptyConfig }
    };

    // Generate from_config() method body
    let from_config_body = generate_from_config_impl(analysis);

    // Determine process method name (from attribute or default to "process")
    let process_method = analysis
        .processor_attrs
        .process_method
        .as_ref()
        .map(|s| format_ident!("{}", s))
        .unwrap_or_else(|| format_ident!("process"));

    // Generate process() delegation
    let process_impl = quote! {
        fn process(&mut self) -> ::streamlib::core::Result<()> {
            self.#process_method()
        }
    };

    // Determine scheduling mode (from attribute or default to Pull)
    let scheduling_mode = analysis
        .processor_attrs
        .scheduling_mode
        .as_deref()
        .unwrap_or("Pull");

    let mode_variant = match scheduling_mode {
        "Push" => quote! { ::streamlib::core::SchedulingMode::Push },
        "Pull" => quote! { ::streamlib::core::SchedulingMode::Pull },
        "Loop" => quote! { ::streamlib::core::SchedulingMode::Loop },
        _ => quote! { ::streamlib::core::SchedulingMode::Pull }, // Default fallback
    };

    let scheduling_config_impl = quote! {
        fn scheduling_config(&self) -> ::streamlib::core::SchedulingConfig {
            ::streamlib::core::SchedulingConfig {
                mode: #mode_variant,
                priority: ::streamlib::core::ThreadPriority::Normal,
            }
        }
    };

    // Generate descriptor() - reuse existing logic
    let descriptor_impl = generate_descriptor_impl(analysis);

    // Generate get_output_port_type()
    let output_port_type_arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            quote! {
                #port_name => Some(<#message_type as ::streamlib::core::link_channel::LinkPortMessage>::port_type())
            }
        })
        .collect();

    let get_output_port_type_impl = if output_port_type_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn get_output_port_type(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                match port_name {
                    #(#output_port_type_arms,)*
                    _ => None
                }
            }
        }
    };

    // Generate get_input_port_type()
    let input_port_type_arms: Vec<TokenStream> = analysis
        .input_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            quote! {
                #port_name => Some(<#message_type as ::streamlib::core::link_channel::LinkPortMessage>::port_type())
            }
        })
        .collect();

    let get_input_port_type_impl = if input_port_type_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn get_input_port_type(&self, port_name: &str) -> Option<::streamlib::core::LinkPortType> {
                match port_name {
                    #(#input_port_type_arms,)*
                    _ => None
                }
            }
        }
    };

    // Generate wire_output_producer()
    let wire_output_producer_arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let field_name = &field.field_name;
            let message_type = &field.message_type;
            let is_arc_wrapped = field.is_arc_wrapped;

            if is_arc_wrapped {
                quote! {
                    #port_name => {
                        if let Ok(typed_producer) = producer.downcast::<::streamlib::core::LinkOwnedProducer<#message_type>>() {
                            let temp_id = ::streamlib::core::link_channel::link_id::__private::new_unchecked(
                                format!("{}.wire_compat_{}", #port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                            );
                            let (tx, _rx) = ::crossbeam_channel::bounded(1);
                            let _ = self.#field_name.as_ref().add_link(temp_id, *typed_producer, tx);
                            return true;
                        }
                        false
                    }
                }
            } else {
                quote! {
                    #port_name => {
                        if let Ok(typed_producer) = producer.downcast::<::streamlib::core::LinkOwnedProducer<#message_type>>() {
                            let temp_id = ::streamlib::core::link_channel::link_id::__private::new_unchecked(
                                format!("{}.wire_compat_{}", #port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                            );
                            let (tx, _rx) = ::crossbeam_channel::bounded(1);
                            let _ = self.#field_name.add_link(temp_id, *typed_producer, tx);
                            return true;
                        }
                        false
                    }
                }
            }
        })
        .collect();

    let wire_output_producer_impl = if wire_output_producer_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn wire_output_producer(&mut self, port_name: &str, producer: Box<dyn std::any::Any + Send>) -> bool {
                match port_name {
                    #(#wire_output_producer_arms,)*
                    _ => false
                }
            }
        }
    };

    // Generate wire_input_consumer()
    let wire_input_consumer_arms: Vec<TokenStream> = analysis
        .input_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let field_name = &field.field_name;
            let message_type = &field.message_type;
            let is_arc_wrapped = field.is_arc_wrapped;

            if is_arc_wrapped {
                quote! {
                    #port_name => {
                        if let Ok(typed_consumer) = consumer.downcast::<::streamlib::core::LinkOwnedConsumer<#message_type>>() {
                            let temp_id = ::streamlib::core::link_channel::link_id::__private::new_unchecked(
                                format!("{}.wire_compat_{}", #port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                            );
                            let (tx, _rx) = ::crossbeam_channel::bounded(1);
                            let source_addr = ::streamlib::core::LinkPortAddress::new("unknown", #port_name);
                            let _ = self.#field_name.as_ref().add_link(temp_id, *typed_consumer, source_addr, tx);
                            return true;
                        }
                        false
                    }
                }
            } else {
                quote! {
                    #port_name => {
                        if let Ok(typed_consumer) = consumer.downcast::<::streamlib::core::LinkOwnedConsumer<#message_type>>() {
                            let temp_id = ::streamlib::core::link_channel::link_id::__private::new_unchecked(
                                format!("{}.wire_compat_{}", #port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                            );
                            let (tx, _rx) = ::crossbeam_channel::bounded(1);
                            let source_addr = ::streamlib::core::LinkPortAddress::new("unknown", #port_name);
                            let _ = self.#field_name.add_link(temp_id, *typed_consumer, source_addr, tx);
                            return true;
                        }
                        false
                    }
                }
            }
        })
        .collect();

    let wire_input_consumer_impl = if wire_input_consumer_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn wire_input_consumer(&mut self, port_name: &str, consumer: Box<dyn std::any::Any + Send>) -> bool {
                match port_name {
                    #(#wire_input_consumer_arms,)*
                    _ => false
                }
            }
        }
    };

    // Generate set_output_wakeup()
    let set_output_wakeup_arms: Vec<TokenStream> = analysis
        .output_ports()
        .map(|field| {
            let port_name = &field.port_name;
            quote! {
                #port_name => {
                    // Wakeups are per-connection, passed in add_link()
                }
            }
        })
        .collect();

    let set_output_wakeup_impl = if set_output_wakeup_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn set_output_wakeup(&mut self, port_name: &str, _wakeup_tx: crossbeam_channel::Sender<::streamlib::core::link_channel::LinkWakeupEvent>) {
                match port_name {
                    #(#set_output_wakeup_arms,)*
                    _ => {},
                }
            }
        }
    };

    quote! {
        impl ::streamlib::core::Processor for #struct_name {
            type Config = #config_type;

            #from_config_body

            #process_impl

            #scheduling_config_impl

            #descriptor_impl

            #get_output_port_type_impl

            #get_input_port_type_impl

            #wire_output_producer_impl

            #wire_input_consumer_impl

            #set_output_wakeup_impl
        }
    }
}

/// Generate from_config() implementation
fn generate_from_config_impl(analysis: &AnalysisResult) -> TokenStream {
    // Generate port construction
    let port_constructions: Vec<TokenStream> = analysis
        .port_fields
        .iter()
        .map(|field| {
            let field_name = &field.field_name;
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            let is_arc_wrapped = field.is_arc_wrapped;

            match field.direction {
                PortDirection::Input => {
                    if is_arc_wrapped {
                        quote! {
                            #field_name: std::sync::Arc::new(::streamlib::core::LinkInput::<#message_type>::new(#port_name))
                        }
                    } else {
                        quote! {
                            #field_name: ::streamlib::core::LinkInput::<#message_type>::new(#port_name)
                        }
                    }
                }
                PortDirection::Output => {
                    if is_arc_wrapped {
                        quote! {
                            #field_name: std::sync::Arc::new(::streamlib::core::LinkOutput::<#message_type>::new(#port_name))
                        }
                    } else {
                        quote! {
                            #field_name: ::streamlib::core::LinkOutput::<#message_type>::new(#port_name)
                        }
                    }
                }
            }
        })
        .collect();

    // Auto-add config field if there's a #[config] field or processor config attribute
    let config_init = if analysis.config_field_type.is_some() {
        // User has #[config] field - initialize it
        quote! { config: config }
    } else if analysis.processor_attrs.config_type.is_some() {
        // Backward compat: processor attribute specifies config - store it
        quote! { config: config }
    } else if !analysis.config_fields.is_empty() {
        // Has config fields in the struct - keep old behavior for backward compatibility
        quote! {}
    } else {
        // No config type and no config fields - use EmptyConfig, no field needed
        quote! {}
    };

    // Generate config field assignments (for backward compatibility with old pattern)
    let config_assignments: Vec<TokenStream> = analysis
        .config_fields
        .iter()
        .map(|field| {
            let field_name = &field.field_name;
            quote! { #field_name: config.#field_name }
        })
        .collect();

    // Generate state field initializations
    let state_initializations: Vec<TokenStream> = analysis
        .state_fields
        .iter()
        .map(|field| {
            let field_name = &field.field_name;
            if let Some(default_expr) = &field.attributes.default_expr {
                // Custom default expression
                let expr: proc_macro2::TokenStream = default_expr.parse().unwrap_or_else(|_| {
                    quote! { Default::default() }
                });
                quote! { #field_name: #expr }
            } else {
                // Use Default::default()
                quote! { #field_name: Default::default() }
            }
        })
        .collect();

    // Combine all initializations
    let has_config_init =
        analysis.config_field_type.is_some() || analysis.processor_attrs.config_type.is_some();

    if has_config_init {
        quote! {
            fn from_config(config: Self::Config) -> ::streamlib::core::Result<Self> {
                Ok(Self {
                    #(#port_constructions,)*
                    #config_init,
                    #(#state_initializations,)*
                })
            }
        }
    } else {
        quote! {
            fn from_config(config: Self::Config) -> ::streamlib::core::Result<Self> {
                Ok(Self {
                    #(#port_constructions,)*
                    #(#config_assignments,)*
                    #(#state_initializations,)*
                })
            }
        }
    }
}

/// Generate descriptor() static method implementation
fn generate_descriptor_impl(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;
    let processor_name = analysis
        .processor_attrs
        .processor_name
        .as_ref()
        .cloned()
        .unwrap_or_else(|| struct_name.to_string());

    // Description: use attribute or generate smart default
    let description = analysis
        .processor_attrs
        .description
        .as_deref()
        .unwrap_or("Generated processor");

    // Usage context: use attribute or generate smart default
    let usage_context = analysis
        .processor_attrs
        .usage_context
        .clone()
        .unwrap_or_else(|| generate_usage_context(analysis));

    // Tags: use attribute or generate smart defaults
    let tags = if analysis.processor_attrs.tags.is_empty() {
        generate_tags(analysis)
    } else {
        analysis.processor_attrs.tags.clone()
    };

    // Generate input port descriptors
    let input_ports: Vec<TokenStream> = analysis
        .input_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            let description = field.attributes.description.as_deref().unwrap_or("");
            let required = field.attributes.required.unwrap_or(true);

            quote! {
                .with_input(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#message_type as ::streamlib::core::link_channel::LinkPortMessage>::schema(),
                    required: #required,
                    description: #description.to_string(),
                })
            }
        })
        .collect();

    // Generate output port descriptors
    let output_ports: Vec<TokenStream> = analysis
        .output_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            let description = field.attributes.description.as_deref().unwrap_or("");

            quote! {
                .with_output(::streamlib::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#message_type as ::streamlib::core::link_channel::LinkPortMessage>::schema(),
                    required: true,
                    description: #description.to_string(),
                })
            }
        })
        .collect();

    quote! {
        fn descriptor() -> Option<::streamlib::core::ProcessorDescriptor> {
            Some(
                ::streamlib::core::ProcessorDescriptor::new(#processor_name, #description)
                    .with_usage_context(#usage_context)
                    .with_tags(vec![#(#tags.to_string()),*])
                    #(#input_ports)*
                    #(#output_ports)*
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_field_name() {
        let ident: Ident = syn::parse_str("video_input").unwrap();
        assert_eq!(humanize_field_name(&ident), "Video Input");

        let ident: Ident = syn::parse_str("audio").unwrap();
        assert_eq!(humanize_field_name(&ident), "Audio");
    }

    #[test]
    fn test_humanize_struct_name() {
        assert_eq!(humanize_struct_name("CameraProcessor"), "camera");
        assert_eq!(humanize_struct_name("AudioMixerProcessor"), "audio mixer");
        assert_eq!(humanize_struct_name("Display"), "display");
    }

    #[test]
    fn test_type_name() {
        let ty: Type = syn::parse_quote! { VideoFrame };
        assert_eq!(type_name(&ty), "VideoFrame");

        let ty: Type = syn::parse_quote! { streamlib::AudioFrame };
        assert_eq!(type_name(&ty), "AudioFrame");
    }
}
