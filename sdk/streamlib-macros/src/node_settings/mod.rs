// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `#[node_settings]`: a struct whose every field is a setting a node reads when it is set up,
//! each described by `#[description = …]`; a String chosen from a list is marked
//! `#[options("…", …)]`, and an `Option<PinType>` choosing one of the bundles its stream's node
//! types declare `#[among_the_bundles_its_stream_declares]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Fields, ItemStruct, LitStr, Meta, Token};

use crate::field_attribute::take_the_declared_description;
use crate::field_attribute::take_the_one_attribute_named;

const OPTIONS_ATTRIBUTE: &str = "options";
const AMONG_THE_BUNDLES_ITS_STREAM_DECLARES_ATTRIBUTE: &str =
    "among_the_bundles_its_stream_declares";

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
        let chooses_among_the_bundles_its_stream_declares =
            take_the_marker_of_a_choice_among_the_bundles_its_stream_declares(&mut field.attrs)?;
        let pin_type = match take_the_declared_options(&mut field.attrs)? {
            Some(_) if let Some(marker) = chooses_among_the_bundles_its_stream_declares => {
                return Err(syn::Error::new(
                    marker,
                    format!(
                        "setting `{setting_name}` is chosen from `#[options(…)]` or among the \
                         bundles its stream declares, not both"
                    ),
                ));
            }
            Some(option_names) => quote! {
                #sdk_root::descriptors::the_menu_of_a_setting::<#field_type>(&[#(#option_names),*])
            },
            None if chooses_among_the_bundles_its_stream_declares.is_some() => quote! {
                #sdk_root::descriptors::the_choice_among_the_bundles_its_stream_declares::<#field_type>()
            },
            None => quote! {
                <#field_type as #sdk_root::descriptors::SettingPinType>::setting_pin_type()
            },
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

/// Removes the field's `#[options("…", …)]` and returns the names it lists; `None` when the
/// field declares none.
fn take_the_declared_options(attributes: &mut Vec<Attribute>) -> syn::Result<Option<Vec<LitStr>>> {
    let Some(attribute) = take_the_one_attribute_named(
        attributes,
        OPTIONS_ATTRIBUTE,
        "a setting declares one `#[options(…)]`, listing every option",
    )?
    else {
        return Ok(None);
    };
    let Meta::List(listed) = &attribute.meta else {
        return Err(options_spelling_refusal(attribute.span()));
    };
    let names: Vec<LitStr> = listed
        .parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)
        .map_err(|_| options_spelling_refusal(listed.span()))?
        .into_iter()
        .collect();
    if names.is_empty() {
        return Err(syn::Error::new(
            listed.span(),
            "`#[options(…)]` lists the names a setting is chosen from, and lists none",
        ));
    }
    Ok(Some(names))
}

/// Removes the field's `#[among_the_bundles_its_stream_declares]` and returns where it was
/// written; `None` when the field declares none.
fn take_the_marker_of_a_choice_among_the_bundles_its_stream_declares(
    attributes: &mut Vec<Attribute>,
) -> syn::Result<Option<proc_macro2::Span>> {
    const MARKED_ONCE_BARE: &str =
        "a setting is marked once with a bare `#[among_the_bundles_its_stream_declares]`";
    let Some(marker) = take_the_one_attribute_named(
        attributes,
        AMONG_THE_BUNDLES_ITS_STREAM_DECLARES_ATTRIBUTE,
        MARKED_ONCE_BARE,
    )?
    else {
        return Ok(None);
    };
    if !matches!(marker.meta, Meta::Path(_)) {
        return Err(syn::Error::new(marker.span(), MARKED_ONCE_BARE));
    }
    Ok(Some(marker.span()))
}

fn options_spelling_refusal(span: proc_macro2::Span) -> syn::Error {
    syn::Error::new(
        span,
        "`#[options(…)]` lists the names a setting is chosen from as string literals — \
         `#[options(\"fast\", \"fine\")]`",
    )
}

#[cfg(test)]
mod tests;
