// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Field analysis for processor attribute macro
//!
//! Classifies struct fields as ports, config, or state fields.

use crate::attributes::{PortAttributes, ProcessorAttributes, StateAttributes};
use proc_macro2::{Ident, TokenStream};
use syn::{Error, Fields, GenericArgument, ItemStruct, PathArguments, Result, Type};

/// Direction of a port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
}

/// Information about a port field
#[derive(Debug)]
pub struct PortField {
    pub field_name: Ident,
    pub port_name: String,
    pub direction: PortDirection,
    pub message_type: Type,
    pub is_arc_wrapped: bool,
    pub field_type: Type,
    pub attributes: PortAttributes,
}

/// Information about a state field
#[derive(Debug)]
pub struct StateField {
    pub field_name: Ident,
    pub field_type: Type,
    pub attributes: StateAttributes,
}

/// Complete analysis result
#[derive(Debug)]
pub struct AnalysisResult {
    pub struct_name: Ident,
    pub port_fields: Vec<PortField>,
    pub state_fields: Vec<StateField>,
    pub config_field_type: Option<Type>,
    pub config_field_name: Option<Ident>,
    pub processor_attrs: ProcessorAttributes,
}

impl AnalysisResult {
    /// Analyze an ItemStruct from attribute macro
    pub fn analyze(item: &ItemStruct, args: TokenStream) -> Result<Self> {
        let struct_name = item.ident.clone();
        let processor_attrs = ProcessorAttributes::parse_from_args(args)?;

        let fields = match &item.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(Error::new_spanned(item, "Processor requires named fields"));
            }
        };

        let mut port_fields = Vec::new();
        let mut state_fields = Vec::new();
        let mut config_field_type = None;
        let mut config_field_name = None;

        for field in fields {
            let field_name = field
                .ident
                .clone()
                .ok_or_else(|| Error::new_spanned(field, "Field must have a name"))?;

            // Check for input port
            if has_attr(&field.attrs, "input") {
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

            // Check for output port
            if has_attr(&field.attrs, "output") {
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

            // Check for config field
            if has_attr(&field.attrs, "config") {
                config_field_type = Some(field.ty.clone());
                config_field_name = Some(field_name);
                continue;
            }

            // Check for explicit state
            if has_attr(&field.attrs, "state") {
                let state_attrs = StateAttributes::parse(&field.attrs)?;
                state_fields.push(StateField {
                    field_name,
                    field_type: field.ty.clone(),
                    attributes: state_attrs,
                });
                continue;
            }

            // Default: treat as state field
            state_fields.push(StateField {
                field_name,
                field_type: field.ty.clone(),
                attributes: StateAttributes::default(),
            });
        }

        if port_fields.is_empty() {
            return Err(Error::new_spanned(
                item,
                "Processor must have at least one #[input] or #[output] port",
            ));
        }

        Ok(AnalysisResult {
            struct_name,
            port_fields,
            state_fields,
            config_field_type,
            config_field_name,
            processor_attrs,
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

/// Check if field has attribute (supports `#[name]`, `#[streamlib::name]`, and `#[crate::name]`)
fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        // Simple: #[name]
        if path.is_ident(name) {
            return true;
        }
        // Qualified: #[streamlib::name] or #[crate::name]
        if path.segments.len() == 2 {
            let first = &path.segments[0].ident;
            let second = &path.segments[1].ident;
            return (first == "streamlib" || first == "crate") && second == name;
        }
        false
    })
}

/// Extract message type from LinkInput<T>, LinkOutput<T>, or Arc<...>
fn extract_message_type(ty: &Type) -> Result<(Type, bool)> {
    let Type::Path(type_path) = ty else {
        return Err(Error::new_spanned(
            ty,
            "Port field must be LinkInput<T>, LinkOutput<T>, or Arc<...>",
        ));
    };

    let segment = type_path
        .path
        .segments
        .last()
        .ok_or_else(|| Error::new_spanned(ty, "Expected type path"))?;

    let ident = &segment.ident;

    // Handle Arc<...>
    if ident == "Arc" {
        let PathArguments::AngleBracketed(args) = &segment.arguments else {
            return Err(Error::new_spanned(ty, "Arc requires type parameter"));
        };

        let GenericArgument::Type(inner_type) = args
            .args
            .first()
            .ok_or_else(|| Error::new_spanned(ty, "Arc requires type parameter"))?
        else {
            return Err(Error::new_spanned(ty, "Expected type parameter in Arc"));
        };

        let (message_type, _) = extract_message_type(inner_type)?;
        return Ok((message_type, true));
    }

    // Handle Mutex<...> (for Arc<Mutex<...>> pattern)
    if ident == "Mutex" {
        let PathArguments::AngleBracketed(args) = &segment.arguments else {
            return Err(Error::new_spanned(ty, "Mutex requires type parameter"));
        };

        let GenericArgument::Type(inner_type) = args
            .args
            .first()
            .ok_or_else(|| Error::new_spanned(ty, "Mutex requires type parameter"))?
        else {
            return Err(Error::new_spanned(ty, "Expected type parameter in Mutex"));
        };

        return extract_message_type(inner_type);
    }

    // Handle LinkInput/LinkOutput
    if ident != "LinkInput" && ident != "LinkOutput" {
        return Err(Error::new_spanned(
            ty,
            "Port fields must be LinkInput<T>, LinkOutput<T>, or Arc<...>",
        ));
    }

    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(Error::new_spanned(
            ty,
            "LinkInput/LinkOutput requires type parameter",
        ));
    };

    let GenericArgument::Type(inner_type) = args
        .args
        .first()
        .ok_or_else(|| Error::new_spanned(ty, "LinkInput/LinkOutput requires type parameter"))?
    else {
        return Err(Error::new_spanned(ty, "Expected type parameter"));
    };

    Ok((inner_type.clone(), false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_extract_message_type() {
        let ty: Type = parse_quote! { LinkInput<VideoFrame> };
        let (result, is_arc) = extract_message_type(&ty).unwrap();
        assert!(!is_arc);
        if let Type::Path(p) = result {
            assert_eq!(p.path.segments.last().unwrap().ident, "VideoFrame");
        }
    }

    #[test]
    fn test_extract_arc_wrapped() {
        let ty: Type = parse_quote! { Arc<LinkOutput<AudioFrame>> };
        let (result, is_arc) = extract_message_type(&ty).unwrap();
        assert!(is_arc);
        if let Type::Path(p) = result {
            assert_eq!(p.path.segments.last().unwrap().ident, "AudioFrame");
        }
    }
}
