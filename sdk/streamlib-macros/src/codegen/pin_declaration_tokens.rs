// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use streamlib_node_schema::pin_type::{PinFlow, PinType};

/// Emit the `Option<PinDeclaration>` a port declaring `pin_type` carries: exactly that type, in
/// the flow it states, else in its type's default flow, with the names it had and whether it may
/// be unset, as it states them.
pub(super) fn pin_declaration_tokens(
    pin_type: Option<&PinType>,
    flow: Option<PinFlow>,
    former_names: &[String],
    may_be_unset: bool,
) -> TokenStream {
    let Some(pin_type) = pin_type else {
        return quote! { ::std::option::Option::None };
    };
    let descriptors = quote! { __streamlib_sdk::descriptors };
    let pin_type = pin_type_tokens(pin_type);
    let exactly = quote! { #descriptors::PinDeclaration::exactly(#pin_type) };
    let mut stated = Vec::new();
    if let Some(flow) = flow {
        let flow = Ident::new(flow.declaration_name(), Span::call_site());
        stated.push(quote! { flow: #descriptors::PinFlow::#flow, });
    }
    if !former_names.is_empty() {
        stated.push(quote! {
            former_names: ::std::vec![#(::std::string::String::from(#former_names)),*],
        });
    }
    if may_be_unset {
        stated.push(quote! { may_be_unset: true, });
    }
    if stated.is_empty() {
        return quote! { ::std::option::Option::Some(#exactly) };
    }
    quote! {
        ::std::option::Option::Some(#descriptors::PinDeclaration {
            #(#stated)*
            ..#exactly
        })
    }
}

/// Emit an expression that builds `pin_type`: one of the types the port grammar declares.
fn pin_type_tokens(pin_type: &PinType) -> TokenStream {
    let descriptors = quote! { __streamlib_sdk::descriptors };
    let variant = |name: &str| Ident::new(name, Span::call_site());
    let optional = |tokens: Option<TokenStream>| match tokens {
        Some(tokens) => quote! { ::std::option::Option::Some(#tokens) },
        None => quote! { ::std::option::Option::None },
    };
    match pin_type {
        PinType::Boolean {} => quote! { #descriptors::PinType::Boolean {} },
        PinType::Integer { exact_size } => {
            let exact_size = optional(exact_size.map(|size| {
                let size = variant(size.variant_name());
                quote! { #descriptors::IntegerSize::#size }
            }));
            quote! { #descriptors::PinType::Integer { exact_size: #exact_size } }
        }
        PinType::Float { exact_size } => {
            let exact_size = optional(exact_size.map(|size| {
                let size = variant(size.variant_name());
                quote! { #descriptors::FloatSize::#size }
            }));
            quote! { #descriptors::PinType::Float { exact_size: #exact_size } }
        }
        PinType::Texture { pixel_formats } => {
            let pixel_formats = pixel_formats
                .iter()
                .map(|format| variant(format.variant_name()));
            quote! {
                #descriptors::PinType::Texture {
                    pixel_formats: ::std::vec![
                        #(#descriptors::TexturePixelFormat::#pixel_formats),*
                    ],
                }
            }
        }
        PinType::Audio {} => quote! { #descriptors::PinType::Audio {} },
        PinType::Tensor {} => quote! { #descriptors::PinType::Tensor {} },
        other => {
            let refusal = format!(
                "`pin_type = {:?}` is not a pin type a Rust port declares",
                other.built_in_name()
            );
            quote! { ::std::compile_error!(#refusal) }
        }
    }
}
