// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Expr, Fields, FnArg, Ident, ItemFn, Pat, Type};

use crate::grammar::{
    DeclaredNodeMeta, is_the_bare_type_named, refuse_a_signature_that_is_not_a_plain_function,
    take_the_pin_default, the_one_type_argument_of, the_output_type,
};

enum FunctionNodeParameterRole {
    InputPin { input_index: usize },
    StreamTime,
    State,
    Config,
}

struct FunctionNodeInputPinDeclared {
    port: String,
    value_type: Type,
    pin_default: Option<Expr>,
}

pub(crate) fn expand_function_node(
    mut function: ItemFn,
    node_meta: DeclaredNodeMeta,
) -> syn::Result<TokenStream2> {
    refuse_a_signature_that_is_not_a_plain_function(&function)?;
    let description = node_meta.description.unwrap_or_else(|| quote! { "" });
    let category = match node_meta.category {
        Some(category) => quote! { ::std::option::Option::Some(#category) },
        None => quote! { ::std::option::Option::None },
    };
    let function_name = function.sig.ident.clone();

    let mut parameter_roles: Vec<FunctionNodeParameterRole> = Vec::new();
    let mut input_pins: Vec<FunctionNodeInputPinDeclared> = Vec::new();
    let mut stream_time_parameter: Option<String> = None;
    let mut state_parameter: Option<String> = None;
    let mut config_parameter: Option<String> = None;
    let mut state_record: Option<Type> = None;
    let mut config_record: Option<Type> = None;

    for parameter in function.sig.inputs.iter_mut() {
        let FnArg::Typed(typed_parameter) = parameter else {
            return Err(syn::Error::new(
                parameter.span(),
                "a function node is a free function; it takes no `self`",
            ));
        };
        let Pat::Ident(parameter_pattern) = typed_parameter.pat.as_ref() else {
            return Err(syn::Error::new(
                typed_parameter.pat.span(),
                "each parameter of a function node is named, since its name is its pin's",
            ));
        };
        let parameter_name = parameter_pattern.ident.unraw().to_string();
        let pin_default = take_the_pin_default(&mut typed_parameter.attrs)?;
        let parameter_type = typed_parameter.ty.as_ref().clone();

        let role = if is_the_bare_type_named(&parameter_type, "Time") {
            if stream_time_parameter.is_some() {
                return Err(syn::Error::new(
                    parameter_type.span(),
                    "a function node takes `Time` once",
                ));
            }
            stream_time_parameter = Some(parameter_name.clone());
            FunctionNodeParameterRole::StreamTime
        } else if let Some(record) = the_one_type_argument_of(&parameter_type, "State") {
            if state_record.is_some() {
                return Err(syn::Error::new(
                    parameter_type.span(),
                    "a function node has one state record; put every field on one record",
                ));
            }
            state_record = Some(record);
            state_parameter = Some(parameter_name.clone());
            FunctionNodeParameterRole::State
        } else if parameter_name == "config" {
            config_record = Some(parameter_type.clone());
            config_parameter = Some(parameter_name.clone());
            FunctionNodeParameterRole::Config
        } else {
            input_pins.push(FunctionNodeInputPinDeclared {
                port: parameter_name,
                value_type: parameter_type,
                pin_default: pin_default.clone(),
            });
            FunctionNodeParameterRole::InputPin {
                input_index: input_pins.len() - 1,
            }
        };
        if pin_default.is_some() && !matches!(role, FunctionNodeParameterRole::InputPin { .. }) {
            return Err(syn::Error::new(
                typed_parameter.span(),
                "only an input pin takes `#[pin_default(...)]`",
            ));
        }
        parameter_roles.push(role);
    }

    let output_type = the_output_type(&function, state_record.as_ref())?;
    let declaration_function_name = Ident::new(
        &format!("{}_function_node_declaration", function_name.unraw()),
        function_name.span(),
    );
    let short_name = upper_camel_case(&function_name.unraw().to_string());
    let visibility = &function.vis;
    let input_count = input_pins.len();

    let call_arguments = parameter_roles.iter().map(|role| match role {
        FunctionNodeParameterRole::InputPin { input_index } => {
            let value_type = &input_pins[*input_index].value_type;
            quote! {
            <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeInputPinValue>::from_function_node_input_f64(
                tatolab_function_node_input_values[#input_index],
            )
            }
        }
        FunctionNodeParameterRole::StreamTime => quote! { tatolab_function_node_now_ns },
        FunctionNodeParameterRole::State => {
            let record = state_record.as_ref().map(ToTokens::to_token_stream);
            quote! {
                <#record as ::tatolab_module_sdk::function_node::FunctionNodeRecord>::from_function_node_values(
                    &*tatolab_function_node_state_values,
                )
            }
        }
        FunctionNodeParameterRole::Config => {
            let record = config_record.as_ref().map(ToTokens::to_token_stream);
            quote! {
                <#record as ::tatolab_module_sdk::function_node::FunctionNodeRecord>::from_function_node_values(
                    tatolab_function_node_config_values,
                )
            }
        }
    });
    let config_values_handed = config_record.as_ref().map(|record| {
        quote! {
            // SAFETY: the read seam hands one f64 per config field.
            let tatolab_function_node_config_values = unsafe {
                ::tatolab_module_sdk::function_node::function_node_values_handed_to_the_evaluate_entry(
                    tatolab_function_node_config_values,
                    <#record as ::tatolab_module_sdk::function_node::FunctionNodeRecord>::FIELDS.len(),
                )
            };
        }
    });
    let state_values_handed = state_record.as_ref().map(|record| {
        quote! {
            // SAFETY: the read seam hands one f64 per state field, reached through nothing else for the call.
            let tatolab_function_node_state_values = unsafe {
                ::tatolab_module_sdk::function_node::function_node_values_the_evaluate_entry_writes(
                    tatolab_function_node_state_values,
                    <#record as ::tatolab_module_sdk::function_node::FunctionNodeRecord>::FIELDS.len(),
                )
            };
        }
    });
    let output_slot_count = quote! {
        ::tatolab_module_sdk::function_node::function_node_output_slot_count(
            <#output_type as ::tatolab_module_sdk::function_node::FunctionNodeOutput>::OUTPUT_PORTS,
        )
    };
    let output_values_staged = quote! {
        let mut tatolab_function_node_staged_output_values = [0.0_f64; #output_slot_count];
    };
    let write_what_the_call_returned = match &state_record {
        Some(record) => quote! {
            let (tatolab_function_node_output, tatolab_function_node_next_state) = tatolab_function_node_returned;
            #output_values_staged
            ::tatolab_module_sdk::function_node::FunctionNodeOutput::write_function_node_output_values(
                &tatolab_function_node_output,
                &mut tatolab_function_node_staged_output_values,
            );
            let mut tatolab_function_node_staged_state_values = [0.0_f64;
                <#record as ::tatolab_module_sdk::function_node::FunctionNodeRecord>::FIELDS.len()];
            ::tatolab_module_sdk::function_node::FunctionNodeRecord::write_function_node_values(
                &tatolab_function_node_next_state,
                &mut tatolab_function_node_staged_state_values,
            );
            tatolab_function_node_output_values.copy_from_slice(&tatolab_function_node_staged_output_values);
            tatolab_function_node_state_values.copy_from_slice(&tatolab_function_node_staged_state_values);
        },
        None => quote! {
            #output_values_staged
            ::tatolab_module_sdk::function_node::FunctionNodeOutput::write_function_node_output_values(
                &tatolab_function_node_returned,
                &mut tatolab_function_node_staged_output_values,
            );
            tatolab_function_node_output_values.copy_from_slice(&tatolab_function_node_staged_output_values);
        },
    };
    let input_pin_declarations = input_pins.iter().map(|input_pin| {
        let port = &input_pin.port;
        let value_type = &input_pin.value_type;
        let pin_default = match &input_pin.pin_default {
            Some(pin_default) => quote! {
                ::core::option::Option::Some(
                    <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeValue>::to_function_node_f64(
                        #pin_default,
                    ),
                )
            },
            None => quote! { ::core::option::Option::None },
        };
        quote! {
            ::tatolab_module_sdk::function_node::FunctionNodeInputPin {
                port: #port,
                value_type:
                    <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeInputPinValue>::VALUE_TYPE,
                pin_default: #pin_default,
                takes_none:
                    <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeInputPinValue>::TAKES_NONE,
            }
        }
    });
    let record_fields_with_their_defaults = |record: &Option<Type>| match record {
        Some(record) => quote! {
            ::tatolab_module_sdk::function_node::function_node_record_fields_with_their_defaults::<#record>()
        },
        None => quote! { ::std::vec::Vec::new() },
    };
    let optional_parameter_name = |parameter_name: Option<String>| match parameter_name {
        Some(parameter_name) => quote! { ::core::option::Option::Some(#parameter_name) },
        None => quote! { ::core::option::Option::None },
    };
    let input_pin_value_types = input_pins.iter().map(|input_pin| {
        let value_type = &input_pin.value_type;
        quote! {
            <#value_type as ::tatolab_module_sdk::function_node::FunctionNodeInputPinValue>::VALUE_TYPE
        }
    });
    let stream_time_parameter = optional_parameter_name(stream_time_parameter);
    let state_parameter = optional_parameter_name(state_parameter);
    let config_parameter = optional_parameter_name(config_parameter);
    let config_fields = record_fields_with_their_defaults(&config_record);
    let state_fields = record_fields_with_their_defaults(&state_record);

    Ok(quote! {
        #function

        #[doc = concat!("The function node `", #short_name, "` declares, its evaluate entry beside it.")]
        #visibility fn #declaration_function_name() -> ::tatolab_module_sdk::function_node::FunctionNodeDeclaration {
            const _: () =
                ::tatolab_module_sdk::function_node::refuse_an_unread_message_output_following_no_one_input(
                    &[#(#input_pin_value_types),*],
                    <#output_type as ::tatolab_module_sdk::function_node::FunctionNodeOutput>::OUTPUT_PORTS,
                );

            unsafe extern "C" fn evaluate_entry(
                tatolab_function_node_input_values: *const f64,
                tatolab_function_node_config_values: *const f64,
                tatolab_function_node_state_values: *mut f64,
                tatolab_function_node_output_values: *mut f64,
                tatolab_function_node_now_ns: i64,
            ) -> ::tatolab_module_sdk::module_host_table_abi::ModuleHostTableStatus {
                let _ = (tatolab_function_node_config_values, tatolab_function_node_state_values, tatolab_function_node_now_ns);
                ::tatolab_module_sdk::function_node::evaluate_one_function_node_call(|| {
                    // SAFETY: the read seam hands one f64 per input pin.
                    let tatolab_function_node_input_values = unsafe {
                        ::tatolab_module_sdk::function_node::function_node_values_handed_to_the_evaluate_entry(
                            tatolab_function_node_input_values,
                            #input_count,
                        )
                    };
                    #config_values_handed
                    #state_values_handed
                    // SAFETY: the read seam hands one f64 per output port and per handed-out flag, reached through nothing else for the call.
                    let tatolab_function_node_output_values = unsafe {
                        ::tatolab_module_sdk::function_node::function_node_values_the_evaluate_entry_writes(
                            tatolab_function_node_output_values,
                            #output_slot_count,
                        )
                    };
                    let _ = &tatolab_function_node_input_values;
                    let tatolab_function_node_returned = #function_name(#(#call_arguments),*);
                    #write_what_the_call_returned
                })
            }

            ::tatolab_module_sdk::function_node::FunctionNodeDeclaration {
                short_name: #short_name,
                description: #description,
                category: #category,
                input_pins: ::std::vec![#(#input_pin_declarations),*],
                output_ports:
                    <#output_type as ::tatolab_module_sdk::function_node::FunctionNodeOutput>::OUTPUT_PORTS.to_vec(),
                config_fields: #config_fields,
                state_fields: #state_fields,
                stream_time_parameter: #stream_time_parameter,
                state_parameter: #state_parameter,
                config_parameter: #config_parameter,
                evaluate_entry,
            }
        }
    })
}

fn upper_camel_case(snake_case_name: &str) -> String {
    snake_case_name
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

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
