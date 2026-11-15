// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, FnArg, GenericArgument, Ident, ItemFn, Pat, PathArguments, ReturnType, Type,
};

use crate::declared_node_meta::DeclaredNodeMeta;

const PIN_DEFAULT_ATTRIBUTE: &str = "pin_default";

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

pub(crate) fn refuse_an_argument_to_function_node(attribute: TokenStream2) -> syn::Result<()> {
    if attribute.is_empty() {
        return Ok(());
    }
    Err(syn::Error::new(
        attribute.span(),
        "#[function_node] takes no argument: the node's name is its function's, in \
         UpperCamelCase, and what describes it goes in the `#[meta(...)]` under it, \
         `#[meta(category = \"Effects/Stylize\", description = \"…\")]`",
    ))
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

fn refuse_a_signature_that_is_not_a_plain_function(function: &ItemFn) -> syn::Result<()> {
    let signature = &function.sig;
    if let Some(asyncness) = signature.asyncness {
        return Err(syn::Error::new(
            asyncness.span(),
            "a function node computes its outputs and returns; it is not `async`",
        ));
    }
    if let Some(constness) = signature.constness {
        return Err(syn::Error::new(
            constness.span(),
            "a function node is called through its evaluate entry, so it is not `const`",
        ));
    }
    if let Some(unsafety) = signature.unsafety {
        return Err(syn::Error::new(
            unsafety.span(),
            "a function node's body is safe Rust; it is not `unsafe`",
        ));
    }
    if let Some(abi) = &signature.abi {
        return Err(syn::Error::new(
            abi.span(),
            "#[function_node] writes the C entry itself; the function takes no ABI",
        ));
    }
    if !signature.generics.params.is_empty() || signature.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            signature.generics.span(),
            "a function node is one concrete function; it takes no generics",
        ));
    }
    if let Some(variadic) = &signature.variadic {
        return Err(syn::Error::new(
            variadic.span(),
            "every input of a function node is a named pin",
        ));
    }
    Ok(())
}

fn the_output_type(function: &ItemFn, state_record: Option<&Type>) -> syn::Result<Type> {
    let ReturnType::Type(_, returned_type) = &function.sig.output else {
        return Err(syn::Error::new(
            function.sig.span(),
            "a function node returns its output: `-> f64`, `-> Option<f64>` when it may hand \
             out nothing, or a `FunctionNodeRecord` for several outputs",
        ));
    };
    match (stateful_output_and_state(returned_type), state_record) {
        (Some((output_type, returned_state_record)), Some(state_record)) => {
            if returned_state_record.to_token_stream().to_string()
                != state_record.to_token_stream().to_string()
            {
                return Err(syn::Error::new(
                    returned_state_record.span(),
                    "the state a function node returns is the one its next call takes; name \
                     one record in `State<...>` and `Stateful<..., ...>`",
                ));
            }
            Ok(output_type)
        }
        (Some(_), None) => Err(syn::Error::new(
            returned_type.span(),
            "a function node returning `Stateful<...>` takes `previous: State<Record>`, or the \
             state it returns never reaches its next call",
        )),
        (None, Some(_)) => Err(syn::Error::new(
            returned_type.span(),
            "a function node taking `State<Record>` returns `Stateful<Output, Record>`, or its \
             next call never sees a new state",
        )),
        (None, None) => Ok(returned_type.as_ref().clone()),
    }
}

fn stateful_output_and_state(returned_type: &Type) -> Option<(Type, Type)> {
    let segment = the_last_path_segment(returned_type, "Stateful")?;
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let mut type_arguments = arguments.args.iter().filter_map(|argument| match argument {
        GenericArgument::Type(type_argument) => Some(type_argument.clone()),
        _ => None,
    });
    let output_type = type_arguments.next()?;
    let state_record = type_arguments.next()?;
    Some((output_type, state_record))
}

fn the_last_path_segment<'parsed>(
    parsed_type: &'parsed Type,
    name: &str,
) -> Option<&'parsed syn::PathSegment> {
    let Type::Path(type_path) = parsed_type else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    (segment.ident == name).then_some(segment)
}

pub(crate) fn is_the_bare_type_named(parsed_type: &Type, name: &str) -> bool {
    the_last_path_segment(parsed_type, name)
        .is_some_and(|segment| matches!(segment.arguments, PathArguments::None))
}

pub(crate) fn the_one_type_argument_of(parsed_type: &Type, name: &str) -> Option<Type> {
    let segment = the_last_path_segment(parsed_type, name)?;
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    if arguments.args.len() != 1 {
        return None;
    }
    match arguments.args.first()? {
        GenericArgument::Type(type_argument) => Some(type_argument.clone()),
        _ => None,
    }
}

fn take_the_pin_default(attributes: &mut Vec<Attribute>) -> syn::Result<Option<Expr>> {
    let mut pin_default: Option<Expr> = None;
    let mut kept_attributes = Vec::with_capacity(attributes.len());
    for attribute in attributes.drain(..) {
        if attribute.path().is_ident(PIN_DEFAULT_ATTRIBUTE) {
            if pin_default.is_some() {
                return Err(syn::Error::new(
                    attribute.span(),
                    "an input pin takes one `#[pin_default(...)]`",
                ));
            }
            pin_default = Some(attribute.parse_args::<Expr>()?);
        } else {
            kept_attributes.push(attribute);
        }
    }
    *attributes = kept_attributes;
    Ok(pin_default)
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
