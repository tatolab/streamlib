// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Derive macro for ConfigDescriptor trait.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, GenericArgument, PathArguments, Result, Type};

/// Extract doc comments from attributes as a single description string.
fn extract_doc_comments(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("doc") {
                if let syn::Meta::NameValue(nv) = &attr.meta {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit_str),
                        ..
                    }) = &nv.value
                    {
                        return Some(lit_str.value().trim().to_string());
                    }
                }
            }
            None
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convert a Rust type to a string representation for the config field.
fn type_to_string(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => {
            let segment = type_path.path.segments.last();
            if let Some(seg) = segment {
                let ident = seg.ident.to_string();

                // Handle Option<T>
                if ident == "Option" {
                    if let PathArguments::AngleBracketed(args) = &seg.arguments {
                        if let Some(GenericArgument::Type(inner_ty)) = args.args.first() {
                            return format!("Option<{}>", type_to_string(inner_ty));
                        }
                    }
                }

                // Handle Vec<T>
                if ident == "Vec" {
                    if let PathArguments::AngleBracketed(args) = &seg.arguments {
                        if let Some(GenericArgument::Type(inner_ty)) = args.args.first() {
                            return format!("Vec<{}>", type_to_string(inner_ty));
                        }
                    }
                }

                // Handle other generic types
                if let PathArguments::AngleBracketed(args) = &seg.arguments {
                    let inner_types: Vec<String> = args
                        .args
                        .iter()
                        .filter_map(|arg| {
                            if let GenericArgument::Type(inner_ty) = arg {
                                Some(type_to_string(inner_ty))
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !inner_types.is_empty() {
                        return format!("{}<{}>", ident, inner_types.join(", "));
                    }
                }

                ident
            } else {
                "unknown".to_string()
            }
        }
        Type::Array(arr) => {
            let elem = type_to_string(&arr.elem);
            // Try to extract the array length
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(lit_int),
                ..
            }) = &arr.len
            {
                format!("[{}; {}]", elem, lit_int)
            } else {
                format!("[{}; N]", elem)
            }
        }
        Type::Tuple(tuple) => {
            let elems: Vec<String> = tuple.elems.iter().map(type_to_string).collect();
            format!("({})", elems.join(", "))
        }
        Type::Reference(reference) => {
            let inner = type_to_string(&reference.elem);
            if reference.mutability.is_some() {
                format!("&mut {}", inner)
            } else {
                format!("&{}", inner)
            }
        }
        _ => "unknown".to_string(),
    }
}

/// Check if a type is Option<T>
fn is_option_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(seg) = type_path.path.segments.last() {
            return seg.ident == "Option";
        }
    }
    false
}

/// Generate the ConfigDescriptor derive implementation.
pub fn derive_config_descriptor(input: DeriveInput) -> Result<TokenStream> {
    let struct_name = &input.ident;

    // Extract fields from struct
    let fields = match &input.data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(fields_named) => &fields_named.named,
            Fields::Unit => {
                // Unit struct - no fields
                return Ok(quote! {
                    impl ::streamlib::core::ConfigDescriptor for #struct_name {
                        fn config_fields() -> ::std::vec::Vec<::streamlib::core::ConfigField> {
                            ::std::vec::Vec::new()
                        }
                    }
                });
            }
            _ => {
                return Err(Error::new(
                    input.ident.span(),
                    "ConfigDescriptor can only be derived for structs with named fields or unit structs",
                ))
            }
        },
        _ => {
            return Err(Error::new(
                input.ident.span(),
                "ConfigDescriptor can only be derived for structs",
            ))
        }
    };

    // Generate field descriptors
    let field_descriptors: Vec<TokenStream> = fields
        .iter()
        .filter_map(|field| {
            let field_name = field.ident.as_ref()?.to_string();
            let field_type = type_to_string(&field.ty);
            let required = !is_option_type(&field.ty);
            let description = extract_doc_comments(&field.attrs);

            Some(quote! {
                ::streamlib::core::ConfigField {
                    name: #field_name.to_string(),
                    field_type: #field_type.to_string(),
                    required: #required,
                    description: #description.to_string(),
                }
            })
        })
        .collect();

    // Generate the implementation
    let expanded = quote! {
        impl ::streamlib::core::ConfigDescriptor for #struct_name {
            fn config_fields() -> ::std::vec::Vec<::streamlib::core::ConfigField> {
                vec![
                    #(#field_descriptors),*
                ]
            }
        }
    };

    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_to_string_primitives() {
        // This is a unit test for the type_to_string function
        // We can't easily test with actual Type objects, but we verify the logic is sound
        assert_eq!(type_to_string(&syn::parse_quote!(u32)), "u32");
        assert_eq!(type_to_string(&syn::parse_quote!(String)), "String");
        assert_eq!(type_to_string(&syn::parse_quote!(bool)), "bool");
    }

    #[test]
    fn test_type_to_string_option() {
        assert_eq!(
            type_to_string(&syn::parse_quote!(Option<String>)),
            "Option<String>"
        );
        assert_eq!(
            type_to_string(&syn::parse_quote!(Option<u32>)),
            "Option<u32>"
        );
    }

    #[test]
    fn test_type_to_string_vec() {
        assert_eq!(type_to_string(&syn::parse_quote!(Vec<u8>)), "Vec<u8>");
    }

    #[test]
    fn test_type_to_string_array() {
        assert_eq!(type_to_string(&syn::parse_quote!([f32; 4])), "[f32; 4]");
    }

    #[test]
    fn test_is_option_type() {
        assert!(is_option_type(&syn::parse_quote!(Option<String>)));
        assert!(!is_option_type(&syn::parse_quote!(String)));
        assert!(!is_option_type(&syn::parse_quote!(Vec<String>)));
    }
}
