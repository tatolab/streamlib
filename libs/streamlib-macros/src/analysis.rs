//! Field analysis for StreamProcessor derive macro
//!
//! Classifies struct fields as ports or config fields,
//! extracts type parameters, and builds analysis result.

use crate::attributes::{PortAttributes, ProcessorAttributes};
use proc_macro2::Ident;
use syn::{
    Data, DeriveInput, Error, Fields, GenericArgument, PathArguments, Result, Type,
};

/// Direction of a port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
}

/// Information about a port field
#[derive(Debug)]
pub struct PortField {
    /// Rust field name
    pub field_name: Ident,

    /// Port name (field_name or custom from attribute)
    pub port_name: String,

    /// Input or Output
    pub direction: PortDirection,

    /// Message type (e.g., VideoFrame, AudioFrame)
    pub message_type: Type,

    /// Whether the port is Arc-wrapped (Arc<StreamInput/Output<T>>)
    pub is_arc_wrapped: bool,

    /// Full field type (for code generation)
    pub field_type: Type,

    /// Parsed port attributes
    pub attributes: PortAttributes,
}

/// Information about a config field
#[derive(Debug)]
pub struct ConfigField {
    /// Rust field name
    pub field_name: Ident,

    /// Field type
    pub field_type: Type,
}

/// Complete analysis result
#[derive(Debug)]
pub struct AnalysisResult {
    /// Struct name
    pub struct_name: Ident,

    /// Port fields (inputs and outputs)
    pub port_fields: Vec<PortField>,

    /// Config fields (non-ports)
    pub config_fields: Vec<ConfigField>,

    /// Processor-level attributes
    pub processor_attrs: ProcessorAttributes,
}

impl AnalysisResult {
    /// Analyze a struct and extract all information
    pub fn analyze(input: &DeriveInput) -> Result<Self> {
        let struct_name = input.ident.clone();

        // Parse processor-level attributes
        let processor_attrs = ProcessorAttributes::parse(&input.attrs)?;

        // Extract named fields
        let fields = match &input.data {
            Data::Struct(data) => match &data.fields {
                Fields::Named(fields) => &fields.named,
                _ => {
                    return Err(Error::new_spanned(
                        input,
                        "StreamProcessor only works with structs with named fields",
                    ));
                }
            },
            _ => {
                return Err(Error::new_spanned(
                    input,
                    "StreamProcessor only works with structs",
                ));
            }
        };

        // Classify fields
        let mut port_fields = Vec::new();
        let mut config_fields = Vec::new();

        for field in fields {
            let field_name = field.ident.clone().ok_or_else(|| {
                Error::new_spanned(field, "Field must have a name")
            })?;

            // Check for #[input] attribute
            if has_attribute(&field.attrs, "input") {
                let port_attrs = PortAttributes::parse(&field.attrs, "input")?;
                let (message_type, is_arc_wrapped) = extract_message_type(&field.ty)?;

                port_fields.push(PortField {
                    port_name: port_attrs
                        .custom_name
                        .clone()
                        .unwrap_or_else(|| field_name.to_string()),
                    field_name,
                    direction: PortDirection::Input,
                    message_type,
                    is_arc_wrapped,
                    field_type: field.ty.clone(),
                    attributes: port_attrs,
                });
                continue;
            }

            // Check for #[output] attribute
            if has_attribute(&field.attrs, "output") {
                let port_attrs = PortAttributes::parse(&field.attrs, "output")?;
                let (message_type, is_arc_wrapped) = extract_message_type(&field.ty)?;

                port_fields.push(PortField {
                    port_name: port_attrs
                        .custom_name
                        .clone()
                        .unwrap_or_else(|| field_name.to_string()),
                    field_name,
                    direction: PortDirection::Output,
                    message_type,
                    is_arc_wrapped,
                    field_type: field.ty.clone(),
                    attributes: port_attrs,
                });
                continue;
            }

            // Not a port, must be a config field
            config_fields.push(ConfigField {
                field_name,
                field_type: field.ty.clone(),
            });
        }

        // Validate that we have at least one port
        if port_fields.is_empty() {
            return Err(Error::new_spanned(
                input,
                "Processor must have at least one #[input] or #[output] port",
            ));
        }

        Ok(AnalysisResult {
            struct_name,
            port_fields,
            config_fields,
            processor_attrs,
        })
    }

    /// Check if processor has audio ports
    pub fn has_audio_ports(&self) -> bool {
        self.port_fields.iter().any(|field| {
            is_audio_frame_type(&field.message_type)
        })
    }

    /// Get input ports
    pub fn input_ports(&self) -> impl Iterator<Item = &PortField> {
        self.port_fields
            .iter()
            .filter(|f| f.direction == PortDirection::Input)
    }

    /// Get output ports
    pub fn output_ports(&self) -> impl Iterator<Item = &PortField> {
        self.port_fields
            .iter()
            .filter(|f| f.direction == PortDirection::Output)
    }
}

/// Extract message type from StreamInput<T>, StreamOutput<T>, or Arc<StreamInput/Output<T>>
/// Returns (message_type, is_arc_wrapped)
fn extract_message_type(ty: &Type) -> Result<(Type, bool)> {
    match ty {
        Type::Path(type_path) => {
            let last_segment = type_path.path.segments.last().ok_or_else(|| {
                Error::new_spanned(ty, "Expected type path")
            })?;

            let ident = &last_segment.ident;

            // Check if it's Arc<...>
            if ident == "Arc" {
                // Extract inner type from Arc<T>
                match &last_segment.arguments {
                    PathArguments::AngleBracketed(args) => {
                        let first_arg = args.args.first().ok_or_else(|| {
                            Error::new_spanned(ty, "Arc requires type parameter")
                        })?;

                        if let GenericArgument::Type(inner_type) = first_arg {
                            // Recursively extract from inner type (should be StreamInput/Output<T>)
                            let (message_type, _) = extract_message_type(inner_type)?;
                            return Ok((message_type, true)); // Arc-wrapped!
                        } else {
                            return Err(Error::new_spanned(ty, "Expected type parameter in Arc"));
                        }
                    }
                    _ => {
                        return Err(Error::new_spanned(
                            ty,
                            "Arc must have angle-bracketed type parameter",
                        ));
                    }
                }
            }

            // Check if it's StreamInput, StreamOutput, or V2 variants
            if ident != "StreamInput" && ident != "StreamOutput" &&
               ident != "StreamInputV2" && ident != "StreamOutputV2" {
                return Err(Error::new_spanned(
                    ty,
                    "Port fields must be StreamInput<T>, StreamOutput<T>, StreamInputV2<T>, StreamOutputV2<T>, or Arc<...>",
                ));
            }

            // Extract generic argument
            match &last_segment.arguments {
                PathArguments::AngleBracketed(args) => {
                    let first_arg = args.args.first().ok_or_else(|| {
                        Error::new_spanned(ty, "StreamInput/StreamOutput requires type parameter")
                    })?;

                    if let GenericArgument::Type(inner_type) = first_arg {
                        Ok((inner_type.clone(), false)) // Not Arc-wrapped
                    } else {
                        Err(Error::new_spanned(
                            ty,
                            "Expected type parameter",
                        ))
                    }
                }
                _ => Err(Error::new_spanned(
                    ty,
                    "StreamInput/StreamOutput must have angle-bracketed type parameter",
                )),
            }
        }
        _ => Err(Error::new_spanned(
            ty,
            "Port field must be StreamInput<T>, StreamOutput<T>, or Arc<StreamInput/Output<T>>",
        )),
    }
}

/// Check if field has attribute with given name
fn has_attribute(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident(name))
}

/// Check if a type is AudioFrame
pub fn is_audio_frame_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "AudioFrame";
        }
    }
    false
}

/// Check if a type is VideoFrame
pub fn is_video_frame_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "VideoFrame";
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_extract_message_type() {
        let ty: Type = parse_quote! { StreamInput<VideoFrame> };
        let (result, is_arc) = extract_message_type(&ty).unwrap();
        assert!(is_video_frame_type(&result));
        assert!(!is_arc);
    }

    #[test]
    fn test_extract_message_type_arc_wrapped() {
        let ty: Type = parse_quote! { Arc<StreamOutput<AudioFrame>> };
        let (result, is_arc) = extract_message_type(&ty).unwrap();
        assert!(is_audio_frame_type(&result));
        assert!(is_arc);
    }

    #[test]
    fn test_is_audio_frame() {
        let ty: Type = parse_quote! { AudioFrame };
        assert!(is_audio_frame_type(&ty));

        let ty: Type = parse_quote! { VideoFrame };
        assert!(!is_audio_frame_type(&ty));
    }
}
