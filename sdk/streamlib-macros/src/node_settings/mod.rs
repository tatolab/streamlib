// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `#[node_settings]`: a struct whose every field is a setting a node reads when it is set up,
//! each described by `#[description = …]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Fields, ItemStruct};

use crate::field_attribute::take_the_declared_description;

pub(crate) fn expand_node_settings(
    attribute: TokenStream,
    mut item: ItemStruct,
    sdk_root: TokenStream,
) -> syn::Result<TokenStream> {
    if !attribute.is_empty() {
        return Err(syn::Error::new(
            attribute.span(),
            "#[node_settings] takes no arguments; describe each setting with \
             `#[description = \"…\"]` on its field",
        ));
    }
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(
            item.generics.span(),
            "#[node_settings] declares the settings of one node type, so the struct takes no \
             generic parameters",
        ));
    }
    let Fields::Named(named_fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.fields.span(),
            "#[node_settings] declares the settings of a struct with named fields, each \
             setting named by its field",
        ));
    };

    let mut setting_inputs = Vec::with_capacity(named_fields.named.len());
    for field in named_fields.named.iter_mut() {
        let field_ident = field
            .ident
            .clone()
            .expect("a named field carries its identifier");
        let setting_name = field_ident.to_string();
        let setting_name = setting_name.trim_start_matches("r#");
        let Some(description) = take_the_declared_description(&mut field.attrs)? else {
            return Err(syn::Error::new(
                field_ident.span(),
                format!(
                    "setting `{setting_name}` is described to whoever sets it: add \
                     `#[description = \"…\"]` to its field"
                ),
            ));
        };
        let field_type = &field.ty;
        let pin_type = quote! {
            <#field_type as #sdk_root::descriptors::SettingPinType>::setting_pin_type()
        };
        setting_inputs.push(quote! {
            #sdk_root::descriptors::the_input_taking_no_wire(
                #setting_name,
                #description,
                #pin_type,
                #sdk_root::descriptors::the_default_of_a_setting(&defaults, #setting_name),
                #sdk_root::descriptors::SettingReadTime::AtSetup,
            )
        });
    }

    let struct_name = &item.ident;
    Ok(quote! {
        #item

        impl #sdk_root::descriptors::NodeSettings for #struct_name {
            fn setting_inputs() -> ::std::vec::Vec<#sdk_root::descriptors::PortDescriptor> {
                let defaults = #sdk_root::serde_json::to_value(
                    <Self as ::std::default::Default>::default(),
                )
                .expect("a node's settings serialize, as every field a setting holds does");
                ::std::vec![#(#setting_inputs),*]
            }
        }
    })
}

#[cfg(test)]
mod tests;
