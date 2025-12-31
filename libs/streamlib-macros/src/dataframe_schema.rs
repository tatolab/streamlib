// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Derive macro for DataFrameSchema trait.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    spanned::Spanned,
    Data, DeriveInput, Error, Expr, ExprLit, Fields, Lit, Meta, Result, Token, Type,
};

/// Parsed schema attribute: `#[schema(name = "...")]`
struct SchemaAttr {
    name: String,
}

impl Parse for SchemaAttr {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut name = None;

        let content: Punctuated<Meta, Token![,]> = Punctuated::parse_terminated(input)?;

        for meta in content {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("name") {
                    if let Expr::Lit(ExprLit {
                        lit: Lit::Str(lit_str),
                        ..
                    }) = &nv.value
                    {
                        name = Some(lit_str.value());
                    }
                }
            }
        }

        Ok(SchemaAttr {
            name: name.unwrap_or_default(),
        })
    }
}

/// Information about a field in the schema struct.
struct FieldInfo {
    name: String,
    type_name: String,
    primitive: TokenStream,
    shape: Vec<usize>,
    byte_size: usize,
}

/// Parse a Rust type into PrimitiveType, type_name, shape, and byte size.
fn parse_type(ty: &Type) -> Result<(TokenStream, String, Vec<usize>, usize)> {
    match ty {
        Type::Path(type_path) => {
            let segment = type_path
                .path
                .segments
                .last()
                .ok_or_else(|| Error::new(ty.span(), "Empty type path"))?;

            let ident = &segment.ident;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "bool" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::Bool),
                    "bool".to_string(),
                    vec![],
                    1,
                )),
                "i32" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::I32),
                    "i32".to_string(),
                    vec![],
                    4,
                )),
                "i64" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::I64),
                    "i64".to_string(),
                    vec![],
                    8,
                )),
                "u32" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::U32),
                    "u32".to_string(),
                    vec![],
                    4,
                )),
                "u64" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::U64),
                    "u64".to_string(),
                    vec![],
                    8,
                )),
                "f32" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::F32),
                    "f32".to_string(),
                    vec![],
                    4,
                )),
                "f64" => Ok((
                    quote!(::streamlib::core::schema::PrimitiveType::F64),
                    "f64".to_string(),
                    vec![],
                    8,
                )),
                _ => Err(Error::new(
                    ty.span(),
                    format!("Unsupported type: {}. Supported types: bool, i32, i64, u32, u64, f32, f64", ident_str),
                )),
            }
        }
        Type::Array(type_array) => {
            // Parse the element type recursively
            let (primitive, type_name, mut shape, elem_size) = parse_type(&type_array.elem)?;

            // Extract array length
            let len = match &type_array.len {
                Expr::Lit(ExprLit {
                    lit: Lit::Int(lit_int),
                    ..
                }) => lit_int.base10_parse::<usize>()?,
                _ => {
                    return Err(Error::new(
                        type_array.len.span(),
                        "Array length must be a literal integer",
                    ))
                }
            };

            // Prepend this dimension to the shape
            shape.insert(0, len);
            let total_size = elem_size * len;

            Ok((primitive, type_name, shape, total_size))
        }
        _ => Err(Error::new(
            ty.span(),
            "Unsupported type. Use primitives (bool, i32, i64, u32, u64, f32, f64) or fixed-size arrays.",
        )),
    }
}

/// Generate the DataFrameSchema derive implementation.
pub fn derive_dataframe_schema(input: DeriveInput) -> Result<TokenStream> {
    let struct_name = &input.ident;

    // Parse #[schema(name = "...")] attribute
    let mut schema_name = struct_name.to_string();
    for attr in &input.attrs {
        if attr.path().is_ident("schema") {
            let schema_attr: SchemaAttr = attr.parse_args()?;
            if !schema_attr.name.is_empty() {
                schema_name = schema_attr.name;
            }
        }
    }

    // Extract fields from struct
    let fields = match &input.data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(fields_named) => &fields_named.named,
            _ => {
                return Err(Error::new(
                    input.ident.span(),
                    "DataFrameSchema can only be derived for structs with named fields",
                ))
            }
        },
        _ => {
            return Err(Error::new(
                input.ident.span(),
                "DataFrameSchema can only be derived for structs",
            ))
        }
    };

    // Parse each field
    let mut field_infos: Vec<FieldInfo> = Vec::new();
    for field in fields {
        let field_name = field
            .ident
            .as_ref()
            .ok_or_else(|| Error::new(field.span(), "Field must have a name"))?
            .to_string();

        let (primitive, type_name, shape, byte_size) = parse_type(&field.ty)?;

        field_infos.push(FieldInfo {
            name: field_name,
            type_name,
            primitive,
            shape,
            byte_size,
        });
    }

    // Compute total byte size and offsets
    let mut total_byte_size: usize = 0;
    let mut field_layouts: Vec<(String, usize, usize)> = Vec::new();

    for info in &field_infos {
        field_layouts.push((info.name.clone(), total_byte_size, info.byte_size));
        total_byte_size += info.byte_size;
    }

    // Generate field descriptors
    let field_descriptors: Vec<TokenStream> = field_infos
        .iter()
        .map(|info| {
            let name = &info.name;
            let type_name = &info.type_name;
            let primitive = &info.primitive;
            let shape = &info.shape;

            quote! {
                ::streamlib::core::schema::DataFrameSchemaField {
                    name: #name.to_string(),
                    description: ::std::string::String::new(),
                    type_name: #type_name.to_string(),
                    shape: vec![#(#shape),*],
                    internal: false,
                    primitive: ::core::option::Option::Some(#primitive),
                }
            }
        })
        .collect();

    // Generate field_layout match arms
    let field_layout_arms: Vec<TokenStream> = field_layouts
        .iter()
        .map(|(name, offset, size)| {
            quote! {
                #name => ::core::option::Option::Some((#offset, #size)),
            }
        })
        .collect();

    // Generate the implementation
    let expanded = quote! {
        impl ::streamlib::core::schema::DataFrameSchema for #struct_name {
            fn name(&self) -> &str {
                #schema_name
            }

            fn fields(&self) -> &[::streamlib::core::schema::DataFrameSchemaField] {
                static FIELDS: ::std::sync::OnceLock<::std::vec::Vec<::streamlib::core::schema::DataFrameSchemaField>> = ::std::sync::OnceLock::new();
                FIELDS.get_or_init(|| {
                    vec![
                        #(#field_descriptors),*
                    ]
                })
            }

            fn byte_size(&self) -> usize {
                #total_byte_size
            }

            fn field_layout(&self, name: &str) -> ::core::option::Option<(usize, usize)> {
                match name {
                    #(#field_layout_arms)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    };

    Ok(expanded)
}
