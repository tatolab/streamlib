// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Implementation of the `#[streamlib::schema]` attribute macro.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::Parser, punctuated::Punctuated, Expr, Fields, ItemStruct, Lit, Meta, Token, Type,
};

/// Parsed attributes from `#[streamlib::schema(...)]`
#[derive(Default)]
pub struct SchemaAttributes {
    pub version: Option<String>,
    pub read_behavior: Option<String>,
    pub name: Option<String>,
    /// Content hint sets defaults for buffer_size and read_behavior.
    pub content_hint: Option<String>,
    /// Explicit buffer size override (takes precedence over content_hint).
    pub buffer_size: Option<usize>,
    /// Temporary: port_type for backwards compatibility (Video, Audio, Data)
    pub port_type: Option<String>,
}

impl SchemaAttributes {
    pub fn parse_from_args(args: TokenStream) -> syn::Result<Self> {
        let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
        let metas = parser.parse2(args)?;

        let mut result = SchemaAttributes::default();

        for meta in metas {
            if let Meta::NameValue(nv) = &meta {
                let ident = nv.path.get_ident().map(|i| i.to_string());
                match ident.as_deref() {
                    Some("version") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Str(s) = &lit.lit {
                                result.version = Some(s.value());
                            }
                        }
                    }
                    Some("read_behavior") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Str(s) = &lit.lit {
                                result.read_behavior = Some(s.value());
                            }
                        }
                    }
                    Some("name") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Str(s) = &lit.lit {
                                result.name = Some(s.value());
                            }
                        }
                    }
                    Some("content_hint") => {
                        if let Expr::Path(path) = &nv.value {
                            // Handle content_hint = Audio (identifier)
                            if let Some(ident) = path.path.get_ident() {
                                result.content_hint = Some(ident.to_string());
                            }
                        } else if let Expr::Lit(lit) = &nv.value {
                            // Handle content_hint = "Audio" (string)
                            if let Lit::Str(s) = &lit.lit {
                                result.content_hint = Some(s.value());
                            }
                        }
                    }
                    Some("buffer_size") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Int(i) = &lit.lit {
                                if let Ok(size) = i.base10_parse::<usize>() {
                                    result.buffer_size = Some(size);
                                }
                            }
                        }
                    }
                    Some("port_type") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Str(s) = &lit.lit {
                                result.port_type = Some(s.value());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(result)
    }
}

/// Parsed field attributes from `#[crate::field(...)]` or `#[streamlib::field(...)]`
#[derive(Default)]
pub struct FieldAttributes {
    /// Skip this field entirely from the schema.
    pub skip: bool,
    /// Mark this field as internal (visible in API but not usable).
    pub internal: bool,
    /// Custom type string for internal fields (e.g., "wgpu::Texture").
    pub type_override: Option<String>,
    /// Description of this field.
    pub description: Option<String>,
}

impl FieldAttributes {
    pub fn parse_from_attrs(attrs: &[syn::Attribute]) -> Self {
        let mut result = FieldAttributes::default();

        for attr in attrs {
            // Check for #[streamlib::field(...)] or #[crate::field(...)]
            let segments: Vec<_> = attr.path().segments.iter().collect();

            if segments.len() == 2 {
                let first = segments[0].ident.to_string();
                let second = segments[1].ident.to_string();
                if (first == "streamlib" || first == "crate") && second == "field" {
                    // Parse #[crate::field(...)] or #[streamlib::field(...)]
                    if let Meta::List(list) = &attr.meta {
                        result.parse_field_args(&list.tokens);
                    }
                }
            }
        }

        result
    }

    fn parse_field_args(&mut self, tokens: &TokenStream) {
        // Manual parsing because `type` is a Rust keyword and can't be parsed as Meta
        let tokens_str = tokens.to_string();

        // Check for `internal` (standalone keyword)
        if tokens_str.contains("internal") {
            self.internal = true;
        }

        // Check for `skip` (standalone keyword)
        if tokens_str.contains("skip") {
            self.skip = true;
        }

        // Parse `field_type = "..."` - used instead of `type` since type is a Rust keyword
        if let Some(type_start) = tokens_str.find("field_type") {
            let after_type = &tokens_str[type_start + 10..];
            if let Some(eq_pos) = after_type.find('=') {
                let after_eq = after_type[eq_pos + 1..].trim_start();
                if let Some(stripped) = after_eq.strip_prefix('"') {
                    // Find the closing quote
                    if let Some(end_quote) = stripped.find('"') {
                        let type_value = &stripped[..end_quote];
                        self.type_override = Some(type_value.to_string());
                    }
                }
            }
        }

        // Parse `description = "..."` - this could also use manual parsing for consistency
        if let Some(desc_start) = tokens_str.find("description") {
            let after_desc = &tokens_str[desc_start + 11..];
            if let Some(eq_pos) = after_desc.find('=') {
                let after_eq = after_desc[eq_pos + 1..].trim_start();
                if let Some(stripped) = after_eq.strip_prefix('"') {
                    // Find the closing quote (handle escaped quotes if needed)
                    if let Some(end_quote) = stripped.find('"') {
                        let desc_value = &stripped[..end_quote];
                        self.description = Some(desc_value.to_string());
                    }
                }
            }
        }
    }
}

/// Information about a schema field.
pub struct SchemaFieldInfo {
    pub name: String,
    pub description: String,
    /// Type name for API output (e.g., "i64", "wgpu::Texture").
    pub type_name: String,
    /// Primitive type enum name (e.g., "I64"). None for internal fields.
    pub primitive_type_name: Option<String>,
    /// Field type enum name for Schema (e.g., "Int64"). None for internal fields.
    pub field_type_name: Option<String>,
    pub shape: Vec<usize>,
    pub internal: bool,
}

/// Primitive type info with both PrimitiveType and FieldType names.
pub struct PrimitiveInfo {
    /// Name for PrimitiveType enum (e.g., "I32", "F32")
    pub primitive_type_name: String,
    /// Name for FieldType enum (e.g., "Int32", "Float32")
    pub field_type_name: String,
}

/// Extract primitive type and shape from a Rust type.
fn extract_primitive_info(ty: &Type) -> Option<(PrimitiveInfo, Vec<usize>)> {
    match ty {
        Type::Path(type_path) => {
            let ident = type_path.path.get_ident()?;
            let name = ident.to_string();
            match name.as_str() {
                "bool" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "Bool".to_string(),
                        field_type_name: "Bool".to_string(),
                    },
                    vec![],
                )),
                "i32" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "I32".to_string(),
                        field_type_name: "Int32".to_string(),
                    },
                    vec![],
                )),
                "i64" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "I64".to_string(),
                        field_type_name: "Int64".to_string(),
                    },
                    vec![],
                )),
                "u32" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "U32".to_string(),
                        field_type_name: "UInt32".to_string(),
                    },
                    vec![],
                )),
                "u64" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "U64".to_string(),
                        field_type_name: "UInt64".to_string(),
                    },
                    vec![],
                )),
                "f32" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "F32".to_string(),
                        field_type_name: "Float32".to_string(),
                    },
                    vec![],
                )),
                "f64" => Some((
                    PrimitiveInfo {
                        primitive_type_name: "F64".to_string(),
                        field_type_name: "Float64".to_string(),
                    },
                    vec![],
                )),
                _ => None, // Non-primitive type (skip for schema fields)
            }
        }
        Type::Array(array) => {
            // Get inner type info
            let (primitive, mut shape) = extract_primitive_info(&array.elem)?;

            // Extract array length
            if let Expr::Lit(lit) = &array.len {
                if let Lit::Int(int) = &lit.lit {
                    if let Ok(len) = int.base10_parse::<usize>() {
                        // Prepend this dimension to shape
                        shape.insert(0, len);
                        return Some((primitive, shape));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Get the lowercase type name for a primitive type.
fn primitive_to_type_name(primitive_type_name: &str) -> String {
    match primitive_type_name {
        "Bool" => "bool".to_string(),
        "I32" => "i32".to_string(),
        "I64" => "i64".to_string(),
        "U32" => "u32".to_string(),
        "U64" => "u64".to_string(),
        "F32" => "f32".to_string(),
        "F64" => "f64".to_string(),
        _ => primitive_type_name.to_lowercase(),
    }
}

/// Analyze struct fields and extract schema field info.
fn analyze_fields(item: &ItemStruct) -> Vec<SchemaFieldInfo> {
    let mut fields = Vec::new();

    if let Fields::Named(named) = &item.fields {
        for field in &named.named {
            let field_attrs = FieldAttributes::parse_from_attrs(&field.attrs);

            // Skip fields marked with #[crate::field(skip)]
            if field_attrs.skip {
                continue;
            }

            let field_name = field
                .ident
                .as_ref()
                .map(|i| i.to_string())
                .unwrap_or_default();

            let description = field_attrs.description.unwrap_or_default();

            // Try to extract primitive info
            if let Some((primitive_info, shape)) = extract_primitive_info(&field.ty) {
                // Primitive field (regular or internal with description)
                fields.push(SchemaFieldInfo {
                    name: field_name,
                    description,
                    type_name: primitive_to_type_name(&primitive_info.primitive_type_name),
                    primitive_type_name: Some(primitive_info.primitive_type_name),
                    field_type_name: Some(primitive_info.field_type_name),
                    shape,
                    internal: field_attrs.internal,
                });
            } else if field_attrs.internal {
                // Internal field with custom type (non-primitive)
                let type_name = field_attrs
                    .type_override
                    .unwrap_or_else(|| "unknown".to_string());
                fields.push(SchemaFieldInfo {
                    name: field_name,
                    description,
                    type_name,
                    primitive_type_name: None,
                    field_type_name: None,
                    shape: vec![],
                    internal: true,
                });
            }
            // Non-primitive fields without internal attribute are silently skipped
        }
    }

    fields
}

/// Generate the schema macro output.
pub fn generate_schema(attrs: SchemaAttributes, item: ItemStruct) -> TokenStream {
    let struct_name = &item.ident;
    let struct_name_str = struct_name.to_string();
    let schema_name = attrs.name.unwrap_or_else(|| struct_name_str.clone());

    // Parse version
    let version_str = attrs.version.unwrap_or_else(|| "1.0.0".to_string());
    let version_parts: Vec<u32> = version_str
        .split('.')
        .map(|s| s.parse().unwrap_or(0))
        .collect();
    let major = version_parts.first().copied().unwrap_or(1);
    let minor = version_parts.get(1).copied().unwrap_or(0);
    let patch = version_parts.get(2).copied().unwrap_or(0);

    // Determine defaults from content_hint
    let (hint_buffer_size, hint_read_behavior): (usize, &str) = match attrs.content_hint.as_deref()
    {
        Some("Audio") | Some("audio") => (32, "read_next_in_order"),
        Some("Video") | Some("video") => (4, "skip_to_latest"),
        Some("Control") | Some("control") => (2, "read_next_in_order"),
        Some("Sensor") | Some("sensor") => (64, "skip_to_latest"),
        _ => (16, "skip_to_latest"), // Default
    };

    // Apply explicit overrides
    let final_buffer_size = attrs.buffer_size.unwrap_or(hint_buffer_size);
    let final_read_behavior = attrs.read_behavior.as_deref().unwrap_or(hint_read_behavior);

    // Parse read behavior to token stream
    let read_behavior = match final_read_behavior {
        "read_next_in_order" => {
            quote! { ::streamlib::core::links::LinkBufferReadMode::ReadNextInOrder }
        }
        _ => quote! { ::streamlib::core::links::LinkBufferReadMode::SkipToLatest },
    };

    // Parse port_type (temporary for backwards compatibility)
    let port_type_impl = match attrs.port_type.as_deref() {
        Some("Video") => quote! {
            fn port_type() -> ::streamlib::core::links::LinkPortType {
                ::streamlib::core::links::LinkPortType::Video
            }
        },
        Some("Audio") => quote! {
            fn port_type() -> ::streamlib::core::links::LinkPortType {
                ::streamlib::core::links::LinkPortType::Audio
            }
        },
        Some("Data") => quote! {
            fn port_type() -> ::streamlib::core::links::LinkPortType {
                ::streamlib::core::links::LinkPortType::Data
            }
        },
        _ => quote! {
            fn port_type() -> ::streamlib::core::links::LinkPortType {
                ::streamlib::core::links::LinkPortType::Data
            }
        },
    };

    // Analyze fields
    let schema_fields = analyze_fields(&item);

    // Generate static field definitions
    let static_fields: Vec<TokenStream> = schema_fields
        .iter()
        .map(|f| {
            let name = &f.name;
            let description = &f.description;
            let type_name = &f.type_name;
            let shape: Vec<TokenStream> = f.shape.iter().map(|s| quote! { #s }).collect();
            let internal = f.internal;

            // Generate primitive as Option<PrimitiveType>
            let primitive_tokens = match &f.primitive_type_name {
                Some(prim) => {
                    let primitive = format_ident!("{}", prim);
                    quote! { Some(::streamlib::core::schema::PrimitiveType::#primitive) }
                }
                None => quote! { None },
            };

            quote! {
                ::streamlib::core::StaticSchemaField {
                    name: #name,
                    description: #description,
                    type_name: #type_name,
                    shape: &[#(#shape),*],
                    internal: #internal,
                    primitive: #primitive_tokens,
                }
            }
        })
        .collect();

    // Generate Schema fields for LinkPortMessage::schema() (uses FieldType)
    // Only include non-internal fields with primitive types
    let schema_field_defs: Vec<TokenStream> = schema_fields
        .iter()
        .filter(|f| !f.internal && f.field_type_name.is_some())
        .map(|f| {
            let name = &f.name;
            let field_type_ident = format_ident!("{}", f.field_type_name.as_ref().unwrap());

            // Build the field type, wrapping in Array for each dimension
            let base_type = quote! { ::streamlib::core::FieldType::#field_type_ident };

            let field_type = if f.shape.is_empty() {
                base_type
            } else {
                // Wrap in Array for each dimension (from innermost to outermost)
                f.shape.iter().fold(base_type, |inner, _| {
                    quote! { ::streamlib::core::FieldType::Array(Box::new(#inner)) }
                })
            };

            quote! {
                ::streamlib::core::Field::new(#name, #field_type)
            }
        })
        .collect();

    // Factory struct name
    let factory_name = format_ident!("{}LinkFactory", struct_name);

    // Generate the output
    let vis = &item.vis;
    let attrs_without_schema: Vec<_> = item
        .attrs
        .iter()
        .filter(|a| {
            // Filter out attributes that start with "streamlib" or "crate"
            let first_segment = a.path().segments.first().map(|s| s.ident.to_string());
            first_segment.as_deref() != Some("streamlib")
                && first_segment.as_deref() != Some("crate")
        })
        .collect();

    // Strip streamlib::field attributes from struct fields
    let mut cleaned_fields = item.fields.clone();
    if let Fields::Named(ref mut named) = cleaned_fields {
        for field in &mut named.named {
            field.attrs.retain(|a| {
                let first_segment = a.path().segments.first().map(|s| s.ident.to_string());
                first_segment.as_deref() != Some("streamlib")
                    && first_segment.as_deref() != Some("crate")
            });
        }
    }

    let generics = &item.generics;

    quote! {
        // Original struct (with non-schema attributes preserved)
        #(#attrs_without_schema)*
        #vis struct #struct_name #generics #cleaned_fields

        // Sealed trait implementation
        impl ::streamlib::core::links::LinkPortMessageImplementor for #struct_name {}

        // LinkPortMessage implementation
        impl ::streamlib::core::links::LinkPortMessage for #struct_name {
            #port_type_impl

            fn schema_name() -> &'static str {
                #schema_name
            }

            fn schema() -> ::std::sync::Arc<::streamlib::core::Schema> {
                static SCHEMA: ::std::sync::LazyLock<::std::sync::Arc<::streamlib::core::Schema>> =
                    ::std::sync::LazyLock::new(|| {
                        ::std::sync::Arc::new(::streamlib::core::Schema::new(
                            #schema_name,
                            ::streamlib::core::SemanticVersion::new(#major, #minor, #patch),
                            vec![
                                #(#schema_field_defs),*
                            ],
                            ::streamlib::core::SerializationFormat::Bincode,
                        ))
                    });
                ::std::sync::Arc::clone(&SCHEMA)
            }

            fn link_read_behavior() -> ::streamlib::core::links::LinkBufferReadMode {
                #read_behavior
            }
        }

        // Link factory for this schema
        struct #factory_name;

        impl ::streamlib::core::SchemaLinkFactory for #factory_name {
            fn create_link_instance(
                &self,
                capacity: ::streamlib::core::graph::LinkCapacity,
                link_id: &::streamlib::core::graph::LinkUniqueId,
            ) -> ::streamlib::core::Result<::streamlib::core::links::LinkInstanceCreationResult> {
                ::streamlib::core::create_typed_link_instance::<#struct_name>(capacity, link_id)
            }
        }

        // Compile-time registration via inventory
        ::streamlib::inventory::submit! {
            ::streamlib::core::SchemaRegistration {
                name: #schema_name,
                version: ::streamlib::core::SemanticVersion::new(#major, #minor, #patch),
                fields: &[
                    #(#static_fields),*
                ],
                read_behavior: #read_behavior,
                default_capacity: #final_buffer_size,
                factory: &#factory_name,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn test_parse_schema_attributes() {
        let args = quote! { version = "1.2.3", read_behavior = "read_next_in_order" };
        let attrs = SchemaAttributes::parse_from_args(args).unwrap();
        assert_eq!(attrs.version, Some("1.2.3".to_string()));
        assert_eq!(attrs.read_behavior, Some("read_next_in_order".to_string()));
    }

    #[test]
    fn test_parse_schema_attributes_defaults() {
        let args = quote! {};
        let attrs = SchemaAttributes::parse_from_args(args).unwrap();
        assert_eq!(attrs.version, None);
        assert_eq!(attrs.read_behavior, None);
    }

    #[test]
    fn test_extract_primitive_scalar() {
        let ty: Type = syn::parse_quote! { f32 };
        let (prim, shape) = extract_primitive_info(&ty).unwrap();
        assert_eq!(prim.primitive_type_name, "F32");
        assert_eq!(prim.field_type_name, "Float32");
        assert!(shape.is_empty());
    }

    #[test]
    fn test_extract_primitive_array() {
        let ty: Type = syn::parse_quote! { [f32; 512] };
        let (prim, shape) = extract_primitive_info(&ty).unwrap();
        assert_eq!(prim.primitive_type_name, "F32");
        assert_eq!(prim.field_type_name, "Float32");
        assert_eq!(shape, vec![512]);
    }

    #[test]
    fn test_extract_primitive_2d_array() {
        let ty: Type = syn::parse_quote! { [[f32; 4]; 4] };
        let (prim, shape) = extract_primitive_info(&ty).unwrap();
        assert_eq!(prim.primitive_type_name, "F32");
        assert_eq!(prim.field_type_name, "Float32");
        assert_eq!(shape, vec![4, 4]);
    }

    #[test]
    fn test_parse_content_hint_audio() {
        let args = quote! { content_hint = Audio };
        let attrs = SchemaAttributes::parse_from_args(args).unwrap();
        assert_eq!(attrs.content_hint, Some("Audio".to_string()));
        assert_eq!(attrs.buffer_size, None);
        assert_eq!(attrs.read_behavior, None);
    }

    #[test]
    fn test_parse_content_hint_with_buffer_size_override() {
        let args = quote! { content_hint = Audio, buffer_size = 64 };
        let attrs = SchemaAttributes::parse_from_args(args).unwrap();
        assert_eq!(attrs.content_hint, Some("Audio".to_string()));
        assert_eq!(attrs.buffer_size, Some(64));
    }

    #[test]
    fn test_parse_buffer_size_only() {
        let args = quote! { buffer_size = 128 };
        let attrs = SchemaAttributes::parse_from_args(args).unwrap();
        assert_eq!(attrs.buffer_size, Some(128));
        assert_eq!(attrs.content_hint, None);
    }

    #[test]
    fn test_parse_content_hint_as_string() {
        let args = quote! { content_hint = "Video" };
        let attrs = SchemaAttributes::parse_from_args(args).unwrap();
        assert_eq!(attrs.content_hint, Some("Video".to_string()));
    }
}
