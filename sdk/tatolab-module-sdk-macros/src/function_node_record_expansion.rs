// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Ident, Type};

use crate::function_node_expansion::{is_the_bare_type_named, the_one_type_argument_of};

pub(crate) fn expand_function_node_record(record: DeriveInput) -> syn::Result<TokenStream2> {
    if !record.generics.params.is_empty() {
        return Err(syn::Error::new(
            record.generics.span(),
            "a function node record is one concrete struct; it takes no generics",
        ));
    }
    let Data::Struct(record_struct) = &record.data else {
        return Err(syn::Error::new(
            record.ident.span(),
            "a function node record is a struct of `f64`, `i64` and `bool` fields, or of outputs \
             that may also be `Option`s and `UnreadMessage`s",
        ));
    };
    let Fields::Named(named_fields) = &record_struct.fields else {
        return Err(syn::Error::new(
            record.ident.span(),
            "a function node record names its fields, since each name is a field or an output port",
        ));
    };
    if named_fields.named.is_empty() {
        return Err(syn::Error::new(
            record.ident.span(),
            "a function node record carries at least one field",
        ));
    }
    let record_name = &record.ident;
    let fields: Vec<(&Ident, &Type)> = named_fields
        .named
        .iter()
        .filter_map(|field| field.ident.as_ref().map(|name| (name, &field.ty)))
        .collect();
    let output_port_declarations = fields.iter().map(|(name, value_type)| {
        let port_name = name.unraw().to_string();
        quote! {
            ::tatolab_module_sdk::function_node::FunctionNodeOutputPort::handing_out::<#value_type>(#port_name)
        }
    });
    let output_port_writes = fields.iter().enumerate().map(|(index, (name, _))| {
        quote! {
            ::tatolab_module_sdk::function_node::write_one_function_node_output_value(
                Self::OUTPUT_PORTS,
                #index,
                self.#name,
                output_values,
            );
        }
    });
    let function_node_output_implementation = quote! {
        impl ::tatolab_module_sdk::function_node::FunctionNodeOutput for #record_name {
            const OUTPUT_PORTS: &'static [::tatolab_module_sdk::function_node::FunctionNodeOutputPort] =
                &[#(#output_port_declarations),*];

            fn write_function_node_output_values(&self, output_values: &mut [f64]) {
                #(#output_port_writes)*
            }
        }
    };
    let is_a_record_of_outputs_only = fields
        .iter()
        .any(|(_, value_type)| hands_out_nothing_or_an_unread_message(value_type));
    if is_a_record_of_outputs_only {
        return Ok(function_node_output_implementation);
    }

    let field_declarations = fields.iter().map(|(name, value_type)| {
        let field_name = name.unraw().to_string();
        quote! {
            ::tatolab_module_sdk::function_node::FunctionNodeNamedValue {
                name: #field_name,
                value_type:
                    <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeValue>::VALUE_TYPE,
            }
        }
    });
    let field_writes = fields.iter().enumerate().map(|(index, (name, _))| {
        quote! {
            values[#index] =
                ::tatolab_module_sdk::function_node::FunctionNodeValue::to_function_node_f64(self.#name);
        }
    });
    let field_reads = fields.iter().enumerate().map(|(index, (name, value_type))| {
        quote! {
            #name: <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeValue>::from_function_node_f64(
                values[#index],
            )
        }
    });
    Ok(quote! {
        impl ::tatolab_module_sdk::function_node::FunctionNodeRecord for #record_name {
            const FIELDS: &'static [::tatolab_module_sdk::function_node::FunctionNodeNamedValue] =
                &[#(#field_declarations),*];

            fn write_function_node_values(&self, values: &mut [f64]) {
                #(#field_writes)*
            }

            fn from_function_node_values(values: &[f64]) -> Self {
                Self { #(#field_reads),* }
            }
        }

        #function_node_output_implementation
    })
}

/// Whether a field is typed `Option<...>` or `UnreadMessage`, which only a record of outputs carries.
fn hands_out_nothing_or_an_unread_message(field_type: &Type) -> bool {
    the_one_type_argument_of(field_type, "Option").is_some()
        || is_the_bare_type_named(field_type, "UnreadMessage")
}
