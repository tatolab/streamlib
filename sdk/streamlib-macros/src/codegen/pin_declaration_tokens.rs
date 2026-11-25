// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use streamlib_node_schema::pin_type::{PinAnnotations, PinFlow, PinType};
use streamlib_node_schema::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    ImageOutputPortFrameRate,
};

/// Emit the `Option<PinDeclaration>` a port declaring `pin_type` carries: exactly that type, in the
/// flow it states, else in its type's default flow, with the names it had and whether it may be
/// unset and the annotations of its stream, as it states them. A port typed by the setting
/// `pin_type_from_setting` carries the type that setting chooses, a value unless it states another
/// flow.
pub(super) fn pin_declaration_tokens(
    pin_type: Option<&PinType>,
    pin_type_from_setting: Option<&str>,
    flow: Option<PinFlow>,
    former_names: &[String],
    may_be_unset: bool,
    annotations: &PinAnnotations,
) -> TokenStream {
    let descriptors = quote! { __streamlib_sdk::descriptors };
    if let Some(setting_name) = pin_type_from_setting {
        let flow = Ident::new(
            flow.unwrap_or(PinFlow::Value).declaration_name(),
            Span::call_site(),
        );
        return quote! {
            ::std::option::Option::Some(#descriptors::PinDeclaration {
                pin_type: #descriptors::DeclaredPinType::SetByInput {
                    input_name: ::std::string::String::from(#setting_name),
                },
                flow: #descriptors::PinFlow::#flow,
                annotations: ::std::default::Default::default(),
                former_names: ::std::vec![#(::std::string::String::from(#former_names)),*],
                may_be_unset: #may_be_unset,
            })
        };
    }
    let Some(pin_type) = pin_type else {
        return quote! { ::std::option::Option::None };
    };
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
    if let Some(annotations) = stream_annotation_tokens(annotations) {
        stated.push(quote! { annotations: #annotations, });
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

/// Emit an expression that builds the stream annotations of `annotations` a Rust port states:
/// its frame rate, sample rate, sample type and residency; `None` when it states none.
fn stream_annotation_tokens(annotations: &PinAnnotations) -> Option<TokenStream> {
    let descriptors = quote! { __streamlib_sdk::descriptors };
    let optional = |tokens: Option<TokenStream>| match tokens {
        Some(tokens) => quote! { ::std::option::Option::Some(#tokens) },
        None => quote! { ::std::option::Option::None },
    };
    let from_setting = |setting: &str| quote! { setting: ::std::string::String::from(#setting) };
    let frame_rate = annotations.frame_rate.as_ref().map(|frame_rate| match frame_rate {
        ImageOutputPortFrameRate::Stated(frame_rate) => {
            let numerator = frame_rate.numerator;
            let denominator = frame_rate.denominator;
            quote! {
                #descriptors::ImageOutputPortFrameRate::Stated(#descriptors::ImageStreamFrameRate {
                    numerator: #numerator,
                    denominator: #denominator,
                })
            }
        }
        ImageOutputPortFrameRate::FromSetting { setting } => {
            let setting = from_setting(setting);
            quote! { #descriptors::ImageOutputPortFrameRate::FromSetting { #setting } }
        }
    });
    let sample_rate = annotations.sample_rate.as_ref().map(|rate| match rate {
        ChannelsOutputPortSampleRate::Stated(rate) => {
            quote! { #descriptors::ChannelsOutputPortSampleRate::Stated(#rate) }
        }
        ChannelsOutputPortSampleRate::FromSetting { setting } => {
            let setting = from_setting(setting);
            quote! { #descriptors::ChannelsOutputPortSampleRate::FromSetting { #setting } }
        }
    });
    let sample_type = annotations
        .sample_type
        .map(|sample_type| match sample_type {
            ChannelsOutputPortSampleType::F32 => {
                quote! { #descriptors::ChannelsOutputPortSampleType::F32 }
            }
            ChannelsOutputPortSampleType::F64 => {
                quote! { #descriptors::ChannelsOutputPortSampleType::F64 }
            }
        });
    let residency = annotations.residency.map(|residency| match residency {
        ArrayOutputPortResidency::Gpu => quote! { #descriptors::ArrayOutputPortResidency::Gpu },
        ArrayOutputPortResidency::Cpu => quote! { #descriptors::ArrayOutputPortResidency::Cpu },
    });
    if frame_rate.is_none() && sample_rate.is_none() && sample_type.is_none() && residency.is_none()
    {
        return None;
    }
    let (frame_rate, sample_rate, sample_type, residency) = (
        optional(frame_rate),
        optional(sample_rate),
        optional(sample_type),
        optional(residency),
    );
    Some(quote! {
        #descriptors::PinAnnotations {
            frame_rate: #frame_rate,
            sample_rate: #sample_rate,
            sample_type: #sample_type,
            residency: #residency,
            ..::std::default::Default::default()
        }
    })
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
        PinType::String {} => quote! { #descriptors::PinType::String {} },
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
        PinType::Trigger {} => quote! { #descriptors::PinType::Trigger {} },
        other => {
            let refusal = format!(
                "`pin_type = {:?}` is not a pin type a Rust port declares",
                other.built_in_name()
            );
            quote! { ::std::compile_error!(#refusal) }
        }
    }
}
