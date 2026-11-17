// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use streamlib_node_schema::pin_type::PinType;

/// Emit the `Option<PinDeclaration>` a port declaring `pin_type` carries: exactly that type, in
/// its default flow.
pub(super) fn pin_declaration_tokens(pin_type: Option<&PinType>) -> TokenStream {
    let Some(pin_type) = pin_type else {
        return quote! { ::std::option::Option::None };
    };
    let pin_type = pin_type_tokens(pin_type);
    quote! {
        ::std::option::Option::Some(
            __streamlib_sdk::descriptors::PinDeclaration::exactly(#pin_type)
        )
    }
}

/// Emit an expression that builds `pin_type`: one of the types the port grammar declares.
fn pin_type_tokens(pin_type: &PinType) -> TokenStream {
    let descriptors = quote! { __streamlib_sdk::descriptors };
    // The derived `Debug` of a fieldless enum is its variant's name.
    let variant =
        |value: &dyn std::fmt::Debug| Ident::new(&format!("{value:?}"), Span::call_site());
    let optional = |tokens: Option<TokenStream>| match tokens {
        Some(tokens) => quote! { ::std::option::Option::Some(#tokens) },
        None => quote! { ::std::option::Option::None },
    };
    match pin_type {
        PinType::Boolean {} => quote! { #descriptors::PinType::Boolean {} },
        PinType::Integer { exact_size } => {
            let exact_size = optional(exact_size.map(|size| {
                let size = variant(&size);
                quote! { #descriptors::IntegerSize::#size }
            }));
            quote! { #descriptors::PinType::Integer { exact_size: #exact_size } }
        }
        PinType::Float { exact_size } => {
            let exact_size = optional(exact_size.map(|size| {
                let size = variant(&size);
                quote! { #descriptors::FloatSize::#size }
            }));
            quote! { #descriptors::PinType::Float { exact_size: #exact_size } }
        }
        PinType::Texture { pixel_formats } => {
            let pixel_formats = pixel_formats.iter().map(|format| variant(format));
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
