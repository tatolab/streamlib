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

    // Generate config struct (or use existing/EmptyConfig)
    let config_struct = generate_config_struct(analysis);

    // Generate from_config() body
    let from_config_body = generate_from_config_body(analysis);

    // Generate descriptor() implementation
    let descriptor_impl = generate_descriptor(analysis);

    // Generate as_any_mut() implementation
    let as_any_mut_impl = generate_as_any_mut(struct_name);

    quote! {
        #config_struct

        impl crate::core::StreamProcessor for #struct_name {
            type Config = Config;

            fn from_config(config: Self::Config) -> crate::core::Result<Self> {
                #from_config_body
            }

            #descriptor_impl

            #as_any_mut_impl

            fn process(&mut self) -> crate::core::Result<()> {
                // Default implementation - users must provide their own process() method
                Ok(())
            }
        }
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
