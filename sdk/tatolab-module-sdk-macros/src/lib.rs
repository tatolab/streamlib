// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[function_node]` and `#[derive(FunctionNodeRecord)]`, re-exported by
//! `tatolab-module-sdk`'s `function-node` feature, whose `function_node` module
//! carries the grammar.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, FnArg, GenericArgument, Ident, ItemFn, Pat,
    PathArguments, ReturnType, Type, parse_macro_input,
};

const PIN_DEFAULT_ATTRIBUTE: &str = "pin_default";

/// Export a function as a function node: its evaluate entry and a
/// `<function>_function_node_declaration()` its module's Python half hands
/// `node.rust_function(...)`.
#[proc_macro_attribute]
pub fn function_node(attribute: TokenStream, item: TokenStream) -> TokenStream {
    if !attribute.is_empty() {
        return syn::Error::new(
            TokenStream2::from(attribute).span(),
            "#[function_node] takes no arguments: the node's name is its function's, in \
             UpperCamelCase, and its description its doc comment",
        )
        .to_compile_error()
        .into();
    }
    let function = parse_macro_input!(item as ItemFn);
    match expand_function_node(function) {
        Ok(expanded) => expanded.into(),
        Err(refusal) => refusal.to_compile_error().into(),
    }
}

/// Implement `FunctionNodeRecord` and `FunctionNodeOutput` for a struct of
/// `f64`, `i64` and `bool` fields, or only `FunctionNodeOutput` when a field is
/// an `Option` or an `UnreadMessage`.
#[proc_macro_derive(FunctionNodeRecord)]
pub fn derive_function_node_record(item: TokenStream) -> TokenStream {
    let record = parse_macro_input!(item as DeriveInput);
    match expand_function_node_record(record) {
        Ok(expanded) => expanded.into(),
        Err(refusal) => refusal.to_compile_error().into(),
    }
}

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

fn expand_function_node(mut function: ItemFn) -> syn::Result<TokenStream2> {
    refuse_a_signature_that_is_not_a_plain_function(&function)?;
    let function_name = function.sig.ident.clone();
    let description = doc_comment_of(&function.attrs);

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

fn is_the_bare_type_named(parsed_type: &Type, name: &str) -> bool {
    the_last_path_segment(parsed_type, name)
        .is_some_and(|segment| matches!(segment.arguments, PathArguments::None))
}

fn the_one_type_argument_of(parsed_type: &Type, name: &str) -> Option<Type> {
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

fn doc_comment_of(attributes: &[Attribute]) -> String {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("doc"))
        .filter_map(|attribute| match &attribute.meta {
            syn::Meta::NameValue(name_value) => match &name_value.value {
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(line),
                    ..
                }) => Some(line.value().trim().to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
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

fn expand_function_node_record(record: DeriveInput) -> syn::Result<TokenStream2> {
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
