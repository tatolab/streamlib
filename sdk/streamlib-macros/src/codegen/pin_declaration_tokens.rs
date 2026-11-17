// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use streamlib_node_schema::pin_type::{
    FloatSize, IntegerSize, PinFlow, PinType, TexturePixelFormat,
};

/// Emit the `Option<PinDeclaration>` a port declaring `pin_type` carries: exactly that type, in
/// the flow it states, else in its type's default flow.
pub(super) fn pin_declaration_tokens(
    pin_type: Option<&PinType>,
    flow: Option<PinFlow>,
) -> TokenStream {
    let Some(pin_type) = pin_type else {
        return quote! { ::std::option::Option::None };
    };
    let descriptors = quote! { __streamlib_sdk::descriptors };
    let pin_type = pin_type_tokens(pin_type);
    let exactly = quote! { #descriptors::PinDeclaration::exactly(#pin_type) };
    let Some(flow) = flow else {
        return quote! { ::std::option::Option::Some(#exactly) };
    };
    let flow = Ident::new(flow.declaration_name(), Span::call_site());
    quote! {
        ::std::option::Option::Some(#descriptors::PinDeclaration {
            flow: #descriptors::PinFlow::#flow,
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
                let size = variant(integer_size_variant_name(size));
                quote! { #descriptors::IntegerSize::#size }
            }));
            quote! { #descriptors::PinType::Integer { exact_size: #exact_size } }
        }
        PinType::Float { exact_size } => {
            let exact_size = optional(exact_size.map(|size| {
                let size = variant(float_size_variant_name(size));
                quote! { #descriptors::FloatSize::#size }
            }));
            quote! { #descriptors::PinType::Float { exact_size: #exact_size } }
        }
        PinType::Texture { pixel_formats } => {
            let pixel_formats = pixel_formats
                .iter()
                .map(|format| variant(texture_pixel_format_variant_name(*format)));
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

fn integer_size_variant_name(size: IntegerSize) -> &'static str {
    match size {
        IntegerSize::Int8 => "Int8",
        IntegerSize::Int16 => "Int16",
        IntegerSize::Int32 => "Int32",
        IntegerSize::Int64 => "Int64",
        IntegerSize::UInt8 => "UInt8",
        IntegerSize::UInt16 => "UInt16",
        IntegerSize::UInt32 => "UInt32",
        IntegerSize::UInt64 => "UInt64",
    }
}

fn float_size_variant_name(size: FloatSize) -> &'static str {
    match size {
        FloatSize::Float32 => "Float32",
        FloatSize::Float64 => "Float64",
    }
}

fn texture_pixel_format_variant_name(format: TexturePixelFormat) -> &'static str {
    match format {
        TexturePixelFormat::Rgba8Unorm => "Rgba8Unorm",
        TexturePixelFormat::Rgba8UnormSrgb => "Rgba8UnormSrgb",
        TexturePixelFormat::Bgra8Unorm => "Bgra8Unorm",
        TexturePixelFormat::Bgra8UnormSrgb => "Bgra8UnormSrgb",
        TexturePixelFormat::Rgba16Float => "Rgba16Float",
        TexturePixelFormat::Rgba32Float => "Rgba32Float",
        TexturePixelFormat::Nv12 => "Nv12",
    }
}
