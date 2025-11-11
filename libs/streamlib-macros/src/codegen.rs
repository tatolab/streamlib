//! Code generation for StreamProcessor derive macro
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
//! - Examples from PortMessage::examples()
//! - Audio requirements for audio processors

use crate::analysis::{AnalysisResult, PortDirection, PortField};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::Type;

/// Generate all code for the StreamProcessor implementation
pub fn generate_processor_impl(analysis: &AnalysisResult) -> TokenStream {
    let struct_name = &analysis.struct_name;

    // Generate view structs and ports() method
    let view_structs = generate_ports_view_structs(analysis);
    let ports_method = generate_ports_convenience_method(analysis);

    // Generate port introspection methods (these are added to existing impls)
    let port_introspection = generate_port_introspection_methods(analysis);

    // Always generate complete trait implementations
    let stream_element_impl = generate_stream_element_impl(analysis);
    let stream_processor_impl = generate_stream_processor_impl(analysis);

    quote! {
        #view_structs

        impl #struct_name {
            #ports_method
        }

        // Generate extension impl with port introspection methods
        impl #struct_name {
            #port_introspection
        }

        // Generate complete StreamElement implementation
        #stream_element_impl

        // Generate complete StreamProcessor implementation
        #stream_processor_impl
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
        pub type Config = crate::core::EmptyConfig;
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
                        #field_name: crate::core::StreamInput::<#message_type>::new(#port_name)
                    }
                }
                PortDirection::Output => {
                    quote! {
                        #field_name: crate::core::StreamOutput::<#message_type>::new(#port_name)
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
        .as_ref()
        .map(|s| s.clone())
        .unwrap_or_else(|| generate_description(analysis));

    // Usage context: use attribute or generate smart default
    let usage_context = analysis
        .processor_attrs
        .usage_context
        .as_ref()
        .map(|s| s.clone())
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
            .with_audio_requirements(crate::core::AudioRequirements {
                #reqs
            })
        }
    } else if analysis.has_audio_ports() {
        quote! {
            .with_audio_requirements(crate::core::AudioRequirements::default())
        }
    } else {
        quote! {}
    };

    quote! {
        fn descriptor() -> Option<crate::core::ProcessorDescriptor> {
            Some(
                crate::core::ProcessorDescriptor::new(
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
        .as_ref()
        .map(|s| s.clone())
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
        .#method(crate::core::PortDescriptor::new(
            #port_name,
            <#message_type as crate::core::PortMessage>::schema(),
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

/// Generate port connection methods (take_output_consumer, connect_input_consumer, set_output_wakeup)
fn generate_port_methods(analysis: &AnalysisResult) -> TokenStream {
    let output_ports: Vec<_> = analysis.output_ports().collect();
    let input_ports: Vec<_> = analysis.input_ports().collect();

    // Generate take_output_consumer method
    let take_output_impl = if !output_ports.is_empty() {
        let port_matches: Vec<TokenStream> = output_ports
            .iter()
            .map(|port| {
                let port_name = &port.port_name;
                let field_name = &port.field_name;
                let message_type_name = type_name(&port.message_type);

                // Determine the PortConsumer variant based on message type
                let port_consumer_variant = match message_type_name.as_str() {
                    "AudioFrame" => quote! { crate::core::traits::PortConsumer::Audio },
                    "VideoFrame" => quote! { crate::core::traits::PortConsumer::Video },
                    _ => quote! { crate::core::traits::PortConsumer::Audio }, // Default to Audio
                };

                quote! {
                    #port_name => {
                        self.#field_name
                            .consumer_holder()
                            .lock()
                            .take()
                            .map(|consumer| #port_consumer_variant(consumer))
                    }
                }
            })
            .collect();

        quote! {
            fn take_output_consumer(&mut self, port_name: &str) -> Option<crate::core::traits::PortConsumer> {
                match port_name {
                    #(#port_matches,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {}
    };

    // Generate connect_input_consumer method
    let connect_input_impl = if !input_ports.is_empty() {
        let port_matches: Vec<TokenStream> = input_ports
            .iter()
            .map(|port| {
                let port_name = &port.port_name;
                let field_name = &port.field_name;
                let message_type_name = type_name(&port.message_type);

                // Determine the PortConsumer variant to match against
                let port_consumer_variant = match message_type_name.as_str() {
                    "AudioFrame" => quote! { crate::core::traits::PortConsumer::Audio },
                    "VideoFrame" => quote! { crate::core::traits::PortConsumer::Video },
                    _ => quote! { crate::core::traits::PortConsumer::Audio },
                };

                quote! {
                    #port_name => {
                        match consumer {
                            #port_consumer_variant(c) => {
                                self.#field_name.connect_consumer(c);
                                true
                            }
                            _ => false,
                        }
                    }
                }
            })
            .collect();

        quote! {
            fn connect_input_consumer(&mut self, port_name: &str, consumer: crate::core::traits::PortConsumer) -> bool {
                match port_name {
                    #(#port_matches,)*
                    _ => false,
                }
            }
        }
    } else {
        quote! {}
    };

    // Generate set_output_wakeup method
    let set_wakeup_impl = if !output_ports.is_empty() {
        let port_matches: Vec<TokenStream> = output_ports
            .iter()
            .map(|port| {
                let port_name = &port.port_name;
                let field_name = &port.field_name;

                quote! {
                    #port_name => {
                        self.#field_name.set_downstream_wakeup(wakeup_tx);
                    }
                }
            })
            .collect();

        quote! {
            fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
                match port_name {
                    #(#port_matches,)*
                    _ => {},
                }
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #take_output_impl
        #connect_input_impl
        #set_wakeup_impl
    }
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
            // Arc-wrapped: &'a Arc<StreamInput<T>>
            let field_type = &p.field_type;
            quote! { pub #name: &'a #field_type }
        } else {
            // Normal: &'a StreamInput<T>
            let message_type = &p.message_type;
            quote! { pub #name: &'a crate::core::StreamInput<#message_type> }
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
            // Arc-wrapped: &'a Arc<StreamOutput<T>>
            let field_type = &p.field_type;
            quote! { pub #name: &'a #field_type }
        } else {
            // Normal: &'a StreamOutput<T>
            let message_type = &p.message_type;
            quote! { pub #name: &'a crate::core::StreamOutput<#message_type> }
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
            #port_name => Some(<#message_type as crate::core::PortMessage>::port_type())
        }
    });

    let get_input_port_type_impl = if !input_ports.is_empty() {
        quote! {
            pub fn get_input_port_type_impl(&self, port_name: &str) -> Option<crate::core::PortType> {
                match port_name {
                    #(#input_port_type_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {
            pub fn get_input_port_type_impl(&self, _port_name: &str) -> Option<crate::core::PortType> {
                None
            }
        }
    };

    // Generate get_output_port_type implementation
    let output_port_type_arms = output_ports.iter().map(|p| {
        let port_name = &p.port_name;
        let message_type = &p.message_type;
        quote! {
            #port_name => Some(<#message_type as crate::core::PortMessage>::port_type())
        }
    });

    let get_output_port_type_impl = if !output_ports.is_empty() {
        quote! {
            pub fn get_output_port_type_impl(&self, port_name: &str) -> Option<crate::core::PortType> {
                match port_name {
                    #(#output_port_type_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {
            pub fn get_output_port_type_impl(&self, _port_name: &str) -> Option<crate::core::PortType> {
                None
            }
        }
    };

    // Generate wire_input_consumer implementation (Phase 2: lock-free)
    let input_wire_arms = input_ports.iter().map(|p| {
        let field_name = &p.field_name;
        let port_name = &p.port_name;
        let message_type = &p.message_type;
        let is_arc_wrapped = p.is_arc_wrapped;

        if is_arc_wrapped {
            // Arc-wrapped: need to call .as_ref() to get &StreamInput
            quote! {
                #port_name => {
                    if let Ok(typed_consumer) = consumer.downcast::<crate::core::OwnedConsumer<#message_type>>() {
                        self.#field_name.as_ref().set_consumer(*typed_consumer);
                        return true;
                    }
                    false
                }
            }
        } else {
            // Normal: direct access
            quote! {
                #port_name => {
                    if let Ok(typed_consumer) = consumer.downcast::<crate::core::OwnedConsumer<#message_type>>() {
                        self.#field_name.set_consumer(*typed_consumer);
                        return true;
                    }
                    false
                }
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

    // Generate wire_output_producer implementation (Phase 2: lock-free)
    let output_wire_arms = output_ports.iter().map(|p| {
        let field_name = &p.field_name;
        let port_name = &p.port_name;
        let message_type = &p.message_type;
        let is_arc_wrapped = p.is_arc_wrapped;

        if is_arc_wrapped {
            // Arc-wrapped: need to call .as_ref() to get &StreamOutput
            quote! {
                #port_name => {
                    if let Ok(typed_producer) = producer.downcast::<crate::core::OwnedProducer<#message_type>>() {
                        self.#field_name.as_ref().add_producer(*typed_producer);
                        return true;
                    }
                    false
                }
            }
        } else {
            // Normal: direct access
            quote! {
                #port_name => {
                    if let Ok(typed_producer) = producer.downcast::<crate::core::OwnedProducer<#message_type>>() {
                        self.#field_name.add_producer(*typed_producer);
                        return true;
                    }
                    false
                }
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

    // Generate StreamProcessor trait method implementations that delegate to the _impl methods
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
            fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::PortType> {
                self.get_input_port_type_impl(port_name)
            }
        }
    } else {
        quote! {}
    };

    let get_output_port_type_trait = if has_outputs {
        quote! {
            fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::PortType> {
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

/// Generate complete StreamElement trait implementation
///
/// This generates all methods of the StreamElement trait, eliminating the need for
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

    // Determine element type from port configuration
    let has_inputs = !analysis.input_ports().collect::<Vec<_>>().is_empty();
    let has_outputs = !analysis.output_ports().collect::<Vec<_>>().is_empty();

    let element_type = match (has_inputs, has_outputs) {
        (false, true) => quote! { crate::core::ElementType::Source },
        (true, false) => quote! { crate::core::ElementType::Sink },
        (true, true) => quote! { crate::core::ElementType::Transform },
        (false, false) => {
            // No ports - treat as transform (unusual case)
            quote! { crate::core::ElementType::Transform }
        }
    };

    // Generate descriptor call (delegates to StreamProcessor::descriptor)
    let descriptor_impl = quote! {
        fn descriptor(&self) -> Option<crate::core::ProcessorDescriptor> {
            <Self as crate::core::StreamProcessor>::descriptor()
        }
    };

    // Determine lifecycle method names (from attributes or default to "on_start"/"on_stop")
    // Try to call user's method if it exists, otherwise do nothing
    let on_start_method = analysis
        .processor_attrs
        .on_start_method
        .as_ref()
        .map(|s| format_ident!("{}", s))
        .unwrap_or_else(|| format_ident!("on_start"));

    let start_impl = quote! {
        fn start(&mut self, ctx: &crate::core::RuntimeContext) -> crate::core::Result<()> {
            // Try to call user's on_start method (will fail gracefully if not present)
            #[allow(unreachable_code)]
            {
                return self.#on_start_method(ctx);
            }
            Ok(())
        }
    };

    let on_stop_method = analysis
        .processor_attrs
        .on_stop_method
        .as_ref()
        .map(|s| format_ident!("{}", s))
        .unwrap_or_else(|| format_ident!("on_stop"));

    let stop_impl = quote! {
        fn stop(&mut self) -> crate::core::Result<()> {
            // Try to call user's on_stop method (will fail gracefully if not present)
            #[allow(unreachable_code)]
            {
                return self.#on_stop_method();
            }
            Ok(())
        }
    };

    // Generate input_ports() method
    let input_port_descriptors: Vec<TokenStream> = analysis
        .input_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            let description = field
                .attributes
                .description
                .as_deref()
                .unwrap_or("");
            let required = field.attributes.required.unwrap_or(true);

            quote! {
                crate::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#message_type as crate::core::PortMessage>::schema(),
                    required: #required,
                    description: #description.to_string(),
                }
            }
        })
        .collect();

    let input_ports_impl = quote! {
        fn input_ports(&self) -> Vec<crate::core::PortDescriptor> {
            vec![#(#input_port_descriptors),*]
        }
    };

    // Generate output_ports() method
    let output_port_descriptors: Vec<TokenStream> = analysis
        .output_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            let description = field
                .attributes
                .description
                .as_deref()
                .unwrap_or("");

            quote! {
                crate::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#message_type as crate::core::PortMessage>::schema(),
                    required: true,
                    description: #description.to_string(),
                }
            }
        })
        .collect();

    let output_ports_impl = quote! {
        fn output_ports(&self) -> Vec<crate::core::PortDescriptor> {
            vec![#(#output_port_descriptors),*]
        }
    };

    // Generate as_source/as_sink/as_transform methods based on element type
    let (as_source, as_source_mut, as_sink, as_sink_mut, as_transform, as_transform_mut) =
        match (has_inputs, has_outputs) {
            (false, true) => {
                // Source
                (
                    quote! {
                        fn as_source(&self) -> Option<&dyn std::any::Any> {
                            Some(self)
                        }
                    },
                    quote! {
                        fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
                            Some(self)
                        }
                    },
                    quote! {},
                    quote! {},
                    quote! {},
                    quote! {},
                )
            },
            (true, false) => {
                // Sink
                (
                    quote! {},
                    quote! {},
                    quote! {
                        fn as_sink(&self) -> Option<&dyn std::any::Any> {
                            Some(self)
                        }
                    },
                    quote! {
                        fn as_sink_mut(&mut self) -> Option<&mut dyn std::any::Any> {
                            Some(self)
                        }
                    },
                    quote! {},
                    quote! {},
                )
            },
            (true, true) => {
                // Transform
                (
                    quote! {},
                    quote! {},
                    quote! {},
                    quote! {},
                    quote! {
                        fn as_transform(&self) -> Option<&dyn std::any::Any> {
                            Some(self)
                        }
                    },
                    quote! {
                        fn as_transform_mut(&mut self) -> Option<&mut dyn std::any::Any> {
                            Some(self)
                        }
                    },
                )
            },
            (false, false) => {
                // No ports - treat as transform
                (
                    quote! {},
                    quote! {},
                    quote! {},
                    quote! {},
                    quote! {
                        fn as_transform(&self) -> Option<&dyn std::any::Any> {
                            Some(self)
                        }
                    },
                    quote! {
                        fn as_transform_mut(&mut self) -> Option<&mut dyn std::any::Any> {
                            Some(self)
                        }
                    },
                )
            }
        };

    quote! {
        impl crate::core::StreamElement for #struct_name {
            fn name(&self) -> &str {
                #processor_name
            }

            fn element_type(&self) -> crate::core::ElementType {
                #element_type
            }

            #descriptor_impl

            #start_impl

            #stop_impl

            #input_ports_impl

            #output_ports_impl

            #as_source
            #as_source_mut
            #as_sink
            #as_sink_mut
            #as_transform
            #as_transform_mut
        }
    }
}

/// Generate complete StreamProcessor trait implementation
///
/// This generates all methods of the StreamProcessor trait, eliminating the need for
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
        quote! { crate::core::EmptyConfig }
    } else {
        // No config at all - use EmptyConfig
        quote! { crate::core::EmptyConfig }
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
        fn process(&mut self) -> crate::core::Result<()> {
            self.#process_method()
        }
    };

    // Determine scheduling mode (from attribute or default to Pull)
    let scheduling_mode = analysis
        .processor_attrs
        .scheduling_mode
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("Pull");

    let scheduling_config_impl = if scheduling_mode == "Push" {
        quote! {
            fn scheduling_config(&self) -> crate::core::SchedulingConfig {
                crate::core::SchedulingConfig {
                    mode: crate::core::SchedulingMode::Push,
                    priority: crate::core::ThreadPriority::Normal,
                }
            }
        }
    } else {
        quote! {
            fn scheduling_config(&self) -> crate::core::SchedulingConfig {
                crate::core::SchedulingConfig {
                    mode: crate::core::SchedulingMode::Pull,
                    priority: crate::core::ThreadPriority::Normal,
                }
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
                #port_name => Some(<#message_type as crate::core::bus::PortMessage>::port_type())
            }
        })
        .collect();

    let get_output_port_type_impl = if output_port_type_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::PortType> {
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
                #port_name => Some(<#message_type as crate::core::bus::PortMessage>::port_type())
            }
        })
        .collect();

    let get_input_port_type_impl = if input_port_type_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::PortType> {
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
                        if let Ok(typed_producer) = producer.downcast::<crate::core::OwnedProducer<#message_type>>() {
                            self.#field_name.as_ref().add_producer(*typed_producer);
                            return true;
                        }
                        false
                    }
                }
            } else {
                quote! {
                    #port_name => {
                        if let Ok(typed_producer) = producer.downcast::<crate::core::OwnedProducer<#message_type>>() {
                            self.#field_name.add_producer(*typed_producer);
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
                        if let Ok(typed_consumer) = consumer.downcast::<crate::core::OwnedConsumer<#message_type>>() {
                            self.#field_name.as_ref().set_consumer(*typed_consumer);
                            return true;
                        }
                        false
                    }
                }
            } else {
                quote! {
                    #port_name => {
                        if let Ok(typed_consumer) = consumer.downcast::<crate::core::OwnedConsumer<#message_type>>() {
                            self.#field_name.set_consumer(*typed_consumer);
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

    quote! {
        impl crate::core::StreamProcessor for #struct_name {
            type Config = #config_type;

            #from_config_body

            #process_impl

            #scheduling_config_impl

            #descriptor_impl

            #get_output_port_type_impl

            #get_input_port_type_impl

            #wire_output_producer_impl

            #wire_input_consumer_impl
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
                            #field_name: std::sync::Arc::new(crate::core::StreamInput::<#message_type>::new(#port_name))
                        }
                    } else {
                        quote! {
                            #field_name: crate::core::StreamInput::<#message_type>::new(#port_name)
                        }
                    }
                }
                PortDirection::Output => {
                    if is_arc_wrapped {
                        quote! {
                            #field_name: std::sync::Arc::new(crate::core::StreamOutput::<#message_type>::new(#port_name))
                        }
                    } else {
                        quote! {
                            #field_name: crate::core::StreamOutput::<#message_type>::new(#port_name)
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
    let has_config_init = analysis.config_field_type.is_some() || analysis.processor_attrs.config_type.is_some();

    if has_config_init {
        quote! {
            fn from_config(config: Self::Config) -> crate::core::Result<Self> {
                Ok(Self {
                    #(#port_constructions,)*
                    #config_init,
                    #(#state_initializations,)*
                })
            }
        }
    } else {
        quote! {
            fn from_config(config: Self::Config) -> crate::core::Result<Self> {
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

    // Generate input port descriptors
    let input_ports: Vec<TokenStream> = analysis
        .input_ports()
        .map(|field| {
            let port_name = &field.port_name;
            let message_type = &field.message_type;
            let description = field
                .attributes
                .description
                .as_deref()
                .unwrap_or("");
            let required = field.attributes.required.unwrap_or(true);

            quote! {
                .with_input(crate::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#message_type as crate::core::bus::PortMessage>::schema(),
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
            let description = field
                .attributes
                .description
                .as_deref()
                .unwrap_or("");

            quote! {
                .with_output(crate::core::PortDescriptor {
                    name: #port_name.to_string(),
                    schema: <#message_type as crate::core::bus::PortMessage>::schema(),
                    required: true,
                    description: #description.to_string(),
                })
            }
        })
        .collect();

    quote! {
        fn descriptor() -> Option<crate::core::ProcessorDescriptor> {
            Some(
                crate::core::ProcessorDescriptor::new(#processor_name, #description)
                    #(#input_ports)*
                    #(#output_ports)*
            )
        }
    }
}
