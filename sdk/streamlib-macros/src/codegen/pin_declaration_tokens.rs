// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream;
use quote::quote;
use streamlib_node_schema::pin_type::PinType;

/// Emit the `Option<PinDeclaration>` a port declaring `pin_type` carries: exactly that type, in
/// its default flow. The type travels as the JSON it serializes to, so every variant is emitted
/// the one way and none needs its own tokens.
pub(super) fn pin_declaration_tokens(pin_type: Option<&PinType>) -> TokenStream {
    let Some(pin_type) = pin_type else {
        return quote! { ::std::option::Option::None };
    };
    let pin_type_json =
        serde_json::to_string(pin_type).expect("a pin type always serializes to JSON");
    quote! {
        ::std::option::Option::Some(__streamlib_sdk::descriptors::PinDeclaration::exactly(
            __streamlib_sdk::serde_json::from_str::<__streamlib_sdk::descriptors::PinType>(
                #pin_type_json,
            )
            .expect("the node macro wrote this pin type as the JSON it reads"),
        ))
    }
}
