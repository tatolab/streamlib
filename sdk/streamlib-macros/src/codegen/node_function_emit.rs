// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `#[node(…)]` on a function: a node written as one function. Its body runs once, when the
//! node is set up, declares the node's computed values and returns its per-frame function; its
//! parameters are its value inputs as handles, its ports and the runtime context, each told by
//! its type. The node is emitted as the struct form's is, a module named after the node holding
//! its `Node`, whose scope the engine hosts.

use proc_macro2::{Ident, Span, TokenStream};
use quote::{quote, quote_spanned};
use streamlib_node_schema::NodeSchemaExecution;
use syn::spanned::Spanned;
use syn::{FnArg, GenericArgument, ItemFn, Pat, PathArguments, ReturnType, Type, TypeParamBound};

use super::author_visibility::{
    GENERATED_MODULE_MEMBER_DEPTH, GENERATED_PORT_MARKER_DEPTH,
    author_visibility_written_from_a_nested_module,
};
use super::node_scaffold_emit::{
    generated_node_members_read_from_the_schema, ipc_mailbox_fields, ipc_mailbox_inits,
    node_inherent_impl, node_module,
};
use super::port_marker_module::generate_port_marker_module;
use super::{NodeDeclaredMetaAndPortDescriptions, generate_descriptor_from_schema};
use crate::grammar::{
    ParsedNodeAttr, ParsedPort, ParsedValueInput, PinTypeChosenBy, PortDirection,
    ValueInputChosenFrom, declared_description_str_expression,
};

/// The name of the node a function `function_ident` declares: its identifier in UpperCamelCase.
/// Refused when that is no identifier, as
/// for `_1`.
pub(crate) fn node_name_of_a_node_function(function_ident: &Ident) -> syn::Result<Ident> {
    let name = streamlib_node_schema::to_pascal_case(&function_ident.to_string());
    let Ok(node_name) = syn::parse_str::<Ident>(&name) else {
        return Err(syn::Error::new(
            function_ident.span(),
            format!(
                "node function `{function_ident}` names no node: its name in UpperCamelCase, \
                 `{name}`, is not an identifier — name the function in snake_case starting with \
                 a letter"
            ),
        ));
    };
    Ok(Ident::new(&node_name.to_string(), function_ident.span()))
}

/// The refusal of `#[node(...)]` on anything but a function, showing the one form a node takes.
const NODE_WRITTEN_AS_ONE_FUNCTION: &str = "#[node(...)] declares a node written as one \
function; a node written as a struct, with a trait impl holding its lifecycle, is retired. \
Write the node as a function whose body runs once, declares its computed values and returns \
its per-frame function:

    #[node(
        execution = reactive,
        input(\"video\", pin_type = Texture),
        input(\"gain\", value = f32, default = 1.0),
        output(\"video_out\", pin_type = Texture),
    )]
    pub fn gain_node(
        gain: Input<f32>,
        inputs: InputMailboxes,
        outputs: OutputWriter,
    ) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let kernel = computed(move |ctx| build_the_kernel(ctx, *gain.get()));
        move |ctx| run_a_frame(ctx, &inputs, &outputs, &kernel.get())
    }

What `setup` opened is a `computed(...)` value, built again when an input it reads changes; \
`teardown` is the drop of what the body made; a manual node returns nothing and its computed \
values publish on their own.";

/// The refusal of `#[node(...)]` written at `span` on a struct or any other item that is not a
/// function, showing the node written as one function.
pub(crate) fn refusal_of_a_node_not_written_as_a_function(span: proc_macro2::Span) -> syn::Error {
    syn::Error::new(span, NODE_WRITTEN_AS_ONE_FUNCTION)
}

pub(crate) fn generate_from_a_node_function(
    mut item: ItemFn,
    parsed: &ParsedNodeAttr,
    sdk_root: TokenStream,
) -> TokenStream {
    let read_and_named = refuse_a_form_the_function_does_not_take(&item, parsed)
        .and_then(|()| node_name_of_a_node_function(&item.sig.ident))
        .and_then(|module_name| Ok((read_the_node_function(&mut item, parsed)?, module_name)));
    let (read, module_name) = match read_and_named {
        Ok(read_and_named) => read_and_named,
        Err(refusal) => {
            let refusal = refusal.to_compile_error();
            return quote! { #item #refusal };
        }
    };
    let function_ident = &item.sig.ident;
    let module_visibility = &item.vis;
    let member_visibility =
        author_visibility_written_from_a_nested_module(&item.vis, GENERATED_MODULE_MEMBER_DEPTH);
    let port_marker_visibility =
        author_visibility_written_from_a_nested_module(&item.vis, GENERATED_PORT_MARKER_DEPTH);
    let schema = parsed.to_node_schema();

    let input_link_module = generate_port_marker_module(
        PortDirection::Input,
        &parsed.inputs,
        &member_visibility,
        &port_marker_visibility,
    );
    let output_link_module = generate_port_marker_module(
        PortDirection::Output,
        &parsed.outputs,
        &member_visibility,
        &port_marker_visibility,
    );
    let value_inputs = ValueInputsEmitted {
        value_inputs: &parsed.value_inputs,
    };
    let descriptor_impl = generate_descriptor_from_schema(
        &schema,
        &NodeDeclaredMetaAndPortDescriptions::of(parsed),
        &value_inputs.descriptor_inputs(),
    );
    let node_inherent_impl = node_inherent_impl(&schema, parsed.built_in_node, &member_visibility);
    let members_read_from_the_schema =
        generated_node_members_read_from_the_schema(&schema, &descriptor_impl);
    let ipc_mailbox_fields = ipc_mailbox_fields(&schema, &member_visibility);
    let ipc_mailbox_inits = ipc_mailbox_inits(&schema);

    let value_input_fields = value_inputs.fields();
    let value_input_handle_fields = value_inputs.handle_fields();
    let value_inputs_declared_by_default = value_inputs.declared_by_default();
    let value_inputs_handed = value_inputs.handed();
    let value_inputs_declared_into_the_scope = value_inputs.declared_into_the_scope();
    let value_inputs_written = value_inputs.written();

    let arguments = read.arguments();
    let argument_bindings = read.argument_bindings();
    let private = quote! { __streamlib_sdk::nodes::__generated_private };
    let call = quote! { super::#function_ident(#(#arguments),*) };
    let call = if read.returned.in_a_result {
        quote! { #call? }
    } else {
        call
    };
    let per_frame_function_taken = if read.returned.a_per_frame_function {
        quote! {
            Ok(::std::option::Option::Some(
                ::std::boxed::Box::new(#call) as #private::PerFrameFunction,
            ))
        }
    } else {
        quote! {
            #call;
            Ok(::std::option::Option::None)
        }
    };
    let nothing_to_start_or_stop_when_pacing_itself =
        matches!(parsed.execution, NodeSchemaExecution::Manual).then(|| {
            quote! {
                /// Nothing to start: the body ran at setup, and its computed values publish on
                /// their own from their first build.
                fn start(
                    &mut self,
                    _ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>,
                ) -> __streamlib_sdk::error::Result<()> {
                    Ok(())
                }

                /// Nothing to stop: teardown drops every value the body made.
                fn stop(
                    &mut self,
                    _ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>,
                ) -> __streamlib_sdk::error::Result<()> {
                    Ok(())
                }
            }
        });

    let node_module = node_module(
        module_visibility,
        &module_name,
        &sdk_root,
        &member_visibility,
        quote! {
            /// The node the function declares, which runs its body once, when it is set up.
            #member_visibility struct Node {
                #ipc_mailbox_fields
                /// The value each value input holds, the latest it was handed.
                value_inputs: ValueInputs,
                /// The scope, view and per-frame function the body made, and the handle of each
                /// value input in the scope; `None` until the body runs.
                running: ::std::option::Option<(#private::NodeFunctionRunning, ValueInputHandles)>,
            }

            /// The value each value input of the node holds.
            struct ValueInputs {
                #(#value_input_fields)*
            }

            /// The handle of each value input in the node's scope.
            #[derive(Clone, Copy)]
            struct ValueInputHandles {
                #(#value_input_handle_fields)*
            }

            impl ValueInputs {
                /// Each value input holding its declared default.
                fn declared_by_default() -> Self {
                    #value_inputs_declared_by_default
                }

                /// The values `input_values` hold, each value input they hold none for holding its
                /// declared default; refused, naming the input, when one does not take its value.
                #[allow(unused_variables, reason = "a node of no value inputs reads none of them")]
                fn handed(
                    node: &str,
                    input_values: &__streamlib_sdk::serde_json::Map<
                        ::std::string::String,
                        __streamlib_sdk::serde_json::Value,
                    >,
                ) -> __streamlib_sdk::error::Result<Self> {
                    #value_inputs_handed
                }

                /// Declare each value input in `declaring`, holding the value it holds here.
                #[allow(unused_variables, reason = "a node of no value inputs declares none")]
                fn declared_into(
                    &self,
                    declaring: &mut #private::ScopeBeingDeclared,
                ) -> ValueInputHandles {
                    #value_inputs_declared_into_the_scope
                }

                /// Write the value each value input holds here to its handle in `handles`.
                #[allow(unused_variables, reason = "a node of no value inputs writes none")]
                fn written(&self, handles: &ValueInputHandles, writes: &mut #private::Writes) {
                    #value_inputs_written
                }
            }

            #input_link_module

            #output_link_module

            #node_inherent_impl

            impl #private::GeneratedNode for Node {
                #members_read_from_the_schema

                fn from_config(_config: Self::Config) -> __streamlib_sdk::error::Result<Self> {
                    Ok(Self {
                        #ipc_mailbox_inits
                        value_inputs: ValueInputs::declared_by_default(),
                        running: ::std::option::Option::None,
                    })
                }

                #nothing_to_start_or_stop_when_pacing_itself

                /// Run a frame once the body has run and every computed value has its first
                /// value; nothing runs before then.
                fn process(
                    &mut self,
                    ctx: &__streamlib_sdk::context::RuntimeContextLimitedAccess<'_>,
                ) -> __streamlib_sdk::error::Result<()> {
                    match &mut self.running {
                        ::std::option::Option::Some((running, _)) => running.run_a_frame(ctx),
                        ::std::option::Option::None => Ok(()),
                    }
                }

                fn write_the_value_inputs_the_input_values_hold(
                    &mut self,
                    input_values: &__streamlib_sdk::serde_json::Map<
                        ::std::string::String,
                        __streamlib_sdk::serde_json::Value,
                    >,
                ) -> __streamlib_sdk::error::Result<()> {
                    let handed = ValueInputs::handed(Self::NAME, input_values)?;
                    if let ::std::option::Option::Some((running, handles)) = &self.running {
                        running.write(|writes| handed.written(handles, writes));
                    }
                    self.value_inputs = handed;
                    Ok(())
                }

                /// Run the body once, declaring the node's scope, and keep the per-frame
                /// function it returns.
                fn __generated_setup(
                    &mut self,
                    ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>,
                ) -> __streamlib_sdk::error::Result<()> {
                    let mut declaring = #private::ScopeBeingDeclared::new();
                    let handles = self.value_inputs.declared_into(&mut declaring);
                    #(#argument_bindings)*
                    let running = #private::NodeFunctionRunning::run_the_body_once(
                        Self::NAME,
                        ctx,
                        declaring,
                        || { #per_frame_function_taken },
                    )?;
                    self.running = ::std::option::Option::Some((running, handles));
                    Ok(())
                }

                /// Drop everything the body made: the per-frame function, then every computed
                /// value, with no build after.
                fn __generated_teardown(
                    &mut self,
                    _ctx: &__streamlib_sdk::context::RuntimeContextFullAccess<'_>,
                ) -> __streamlib_sdk::error::Result<()> {
                    self.running = ::std::option::Option::None;
                    Ok(())
                }
            }
        },
    );

    quote! {
        #item
        #node_module
    }
}

/// What a node function's value inputs emit.
struct ValueInputsEmitted<'parsed> {
    value_inputs: &'parsed [ParsedValueInput],
}

impl ValueInputsEmitted<'_> {
    /// The value its default states, as the type it holds.
    fn default_value(value_input: &ParsedValueInput) -> TokenStream {
        let value_type = &value_input.value_type;
        let default = &value_input.default;
        quote_spanned! { default.span() =>
            <_ as __streamlib_sdk::nodes::__generated_private::TheDefaultOfAValueInput<#value_type>>
                ::into_the_default(#default)
        }
    }

    fn fields(&self) -> Vec<TokenStream> {
        self.value_inputs
            .iter()
            .map(|value_input| {
                let field = &value_input.declared_ident;
                let value_type = &value_input.value_type;
                quote! { #field: #value_type, }
            })
            .collect()
    }

    fn handle_fields(&self) -> Vec<TokenStream> {
        self.value_inputs
            .iter()
            .map(|value_input| {
                let field = &value_input.declared_ident;
                let value_type = &value_input.value_type;
                quote! { #field: __streamlib_sdk::nodes::Input<#value_type>, }
            })
            .collect()
    }

    fn declared_by_default(&self) -> TokenStream {
        let fields = self.value_inputs.iter().map(|value_input| {
            let field = &value_input.declared_ident;
            let default = Self::default_value(value_input);
            quote! { #field: #default, }
        });
        quote! { Self { #(#fields)* } }
    }

    fn handed(&self) -> TokenStream {
        let fields = self.value_inputs.iter().map(|value_input| {
            let field = &value_input.declared_ident;
            let value_type = &value_input.value_type;
            let name = &value_input.cast_name;
            let default = Self::default_value(value_input);
            quote! {
                #field: match input_values.get(#name).filter(|value| !value.is_null()) {
                    ::std::option::Option::Some(value) => {
                        __streamlib_sdk::serde_json::from_value::<#value_type>(
                            ::std::clone::Clone::clone(value),
                        )
                        .map_err(|why| __streamlib_sdk::error::Error::InputValueNotTaken {
                            node: ::std::borrow::ToOwned::to_owned(node),
                            input: ::std::borrow::ToOwned::to_owned(#name),
                            why: ::std::string::ToString::to_string(&why),
                        })?
                    }
                    ::std::option::Option::None => #default,
                },
            }
        });
        quote! { Ok(Self { #(#fields)* }) }
    }

    fn declared_into_the_scope(&self) -> TokenStream {
        let fields = self.value_inputs.iter().map(|value_input| {
            let field = &value_input.declared_ident;
            quote! { #field: declaring.input(::std::clone::Clone::clone(&self.#field)), }
        });
        quote! { ValueInputHandles { #(#fields)* } }
    }

    fn written(&self) -> TokenStream {
        let writes = self.value_inputs.iter().map(|value_input| {
            let field = &value_input.declared_ident;
            quote! { writes.write(handles.#field, ::std::clone::Clone::clone(&self.#field)); }
        });
        quote! { #(#writes)* }
    }

    /// Each value input's `.with_input(…)` on the node's descriptor.
    fn descriptor_inputs(&self) -> TokenStream {
        let inputs = self.value_inputs.iter().map(|value_input| {
            let value_type = &value_input.value_type;
            let name = &value_input.cast_name;
            let description = declared_description_str_expression(value_input.description.as_ref());
            let default = Self::default_value(value_input);
            let pin_type = match &value_input.chosen_from {
                ValueInputChosenFrom::AnyValueOfItsType => quote! {
                    <#value_type as __streamlib_sdk::descriptors::SettingPinType>::setting_pin_type()
                },
                ValueInputChosenFrom::Options(option_names) => quote! {
                    __streamlib_sdk::descriptors::the_menu_of_a_setting::<#value_type>(
                        &[#(#option_names),*],
                    )
                },
                ValueInputChosenFrom::TheBundlesItsStreamDeclares => quote! {
                    __streamlib_sdk::descriptors::the_choice_among_the_bundles_its_stream_declares::<
                        #value_type,
                    >()
                },
            };
            quote! {
                .with_input(__streamlib_sdk::descriptors::the_input_taking_no_wire(
                    #name,
                    #description,
                    #pin_type,
                    __streamlib_sdk::serde_json::to_value(#default)
                        .expect("a value input's default serializes, as every value an input holds does"),
                    __streamlib_sdk::descriptors::SettingReadTime::EachRun,
                ))
            }
        });
        quote! { #(#inputs)* }
    }
}

/// What a parameter of a node function is, told by its type.
#[derive(PartialEq, Eq)]
enum ParameterRole {
    /// `&RuntimeContextFullAccess<'_>`: the runtime, for the body only.
    RuntimeContext,
    /// `InputMailboxes`: the node's stream inputs.
    InputMailboxes,
    /// `OutputWriter`: the node's outputs.
    OutputWriter,
    /// `Input<T>`: the value input the parameter is named after.
    ValueInput(Ident),
}

/// What a node function returns: its per-frame function or nothing, in a `Result` or not.
struct WhatTheBodyReturns {
    /// The per-frame function; nothing for a node that paces itself, whose computed values
    /// publish on their own.
    a_per_frame_function: bool,
    /// In a `Result`, when the body can fail.
    in_a_result: bool,
}

/// A node function, read: each parameter's role, in order, and how it returns its per-frame
/// function.
struct NodeFunctionRead {
    parameters: Vec<ParameterRole>,
    returned: WhatTheBodyReturns,
}

impl NodeFunctionRead {
    /// The `let` each argument the body is called with is bound by before the call.
    fn argument_bindings(&self) -> Vec<TokenStream> {
        self.parameters
            .iter()
            .filter_map(|role| match role {
                ParameterRole::InputMailboxes => Some(quote! {
                    let inputs = ::std::clone::Clone::clone(&self.inputs);
                }),
                ParameterRole::OutputWriter => Some(quote! {
                    let outputs = ::std::clone::Clone::clone(&self.outputs);
                }),
                ParameterRole::RuntimeContext | ParameterRole::ValueInput(_) => None,
            })
            .collect()
    }

    /// The arguments the body is called with, in the order of its parameters.
    fn arguments(&self) -> Vec<TokenStream> {
        self.parameters
            .iter()
            .map(|role| match role {
                ParameterRole::RuntimeContext => quote! { ctx },
                ParameterRole::InputMailboxes => quote! { inputs },
                ParameterRole::OutputWriter => quote! { outputs },
                ParameterRole::ValueInput(parameter) => quote! { handles.#parameter },
            })
            .collect()
    }
}

/// Refuse a declaration the one-function form does not take.
fn refuse_a_form_the_function_does_not_take(
    item: &ItemFn,
    parsed: &ParsedNodeAttr,
) -> syn::Result<()> {
    for port in parsed.inputs.iter().chain(&parsed.outputs) {
        refuse_a_pin_type_no_value_input_chooses(port, &parsed.value_inputs)?;
    }
    if !item.sig.generics.params.is_empty() || item.sig.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            item.sig.generics.span(),
            "a node function takes no generic parameters: it declares one node type",
        ));
    }
    if let Some(asyncness) = &item.sig.asyncness {
        return Err(syn::Error::new(
            asyncness.span(),
            "a node function is not `async`: its body runs once, when the node is set up",
        ));
    }
    Ok(())
}

/// Read each parameter's role and how the function returns its per-frame function, adding
/// `+ Send + 'static` to the per-frame function's type where the author left them out.
fn read_the_node_function(
    item: &mut ItemFn,
    parsed: &ParsedNodeAttr,
) -> syn::Result<NodeFunctionRead> {
    let mut parameters = Vec::with_capacity(item.sig.inputs.len());
    for parameter in &item.sig.inputs {
        let FnArg::Typed(parameter) = parameter else {
            return Err(syn::Error::new(
                parameter.span(),
                "a node function takes no `self`: it is a free function",
            ));
        };
        let Pat::Ident(pattern) = parameter.pat.as_ref() else {
            return Err(syn::Error::new(
                parameter.pat.span(),
                "each parameter of a node function is a plain name, which a value input's \
                 parameter shares with its input",
            ));
        };
        let role = role_of_a_parameter(&pattern.ident, &parameter.ty)?;
        if parameters.contains(&role) {
            return Err(syn::Error::new(
                parameter.span(),
                "a node function takes each of its context, `InputMailboxes`, `OutputWriter` \
                 and value inputs once",
            ));
        }
        refuse_a_parameter_the_node_does_not_declare(&role, parameter.span(), parsed)?;
        parameters.push(role);
    }
    if let Some(value_input) = parsed.value_inputs.iter().find(|value_input| {
        !parameters.iter().any(|role| {
            matches!(role, ParameterRole::ValueInput(parameter)
                if *parameter == value_input.declared_ident)
        })
    }) {
        return Err(syn::Error::new(
            value_input.declared_ident.span(),
            format!(
                "value input `{0}` is read by no parameter — add `{0}: Input<…>` to the node \
                 function",
                value_input.declared_ident
            ),
        ));
    }
    let returned = if matches!(parsed.execution, NodeSchemaExecution::Manual) {
        nothing_returned_by_a_node_pacing_itself(&mut item.sig.output)?
    } else {
        per_frame_function_returned(&mut item.sig.output)?
    };
    Ok(NodeFunctionRead {
        parameters,
        returned,
    })
}

/// Refuse `port` of a node function when the value input its `from_input(…)` names is not one
/// of its `value_inputs`, or holds no pin type to choose.
fn refuse_a_pin_type_no_value_input_chooses(
    port: &ParsedPort,
    value_inputs: &[ParsedValueInput],
) -> syn::Result<()> {
    let port_ident = &port.declared_marker_ident;
    match &port.pin_type_chosen_by {
        None => Ok(()),
        Some(PinTypeChosenBy { named, cast_name }) => {
            let Some(value_input) = value_inputs
                .iter()
                .find(|value_input| &value_input.cast_name == cast_name)
            else {
                return Err(syn::Error::new(
                    named.span(),
                    format!(
                        "port `{port_ident}` takes its pin type `from_input({named})`, and the \
                         node declares no value input `{named}`: declare it, `input(\"{named}\", \
                         value = Option<PinType>, default = None)`"
                    ),
                ));
            };
            if holds_a_pin_type(&value_input.value_type) {
                return Ok(());
            }
            Err(syn::Error::new(
                value_input.value_type.span(),
                format!(
                    "value input `{named}` chooses the pin type of port `{port_ident}`, so it \
                     holds the choice of one: `value = Option<PinType>`"
                ),
            ))
        }
    }
}

/// Whether `ty` names an optional pin type, the choice of a type a value input holds.
fn holds_a_pin_type(ty: &Type) -> bool {
    let Some(segment) = last_path_segment(ty).filter(|segment| segment.ident == "Option") else {
        return false;
    };
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    matches!(
        arguments.args.first(),
        Some(GenericArgument::Type(held)) if last_path_segment(held).is_some_and(|held| held.ident == "PinType")
    )
}

/// The last segment of the path `ty` names, with its arguments; `None` for any other type.
fn last_path_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(path) if path.qself.is_none() => path.path.segments.last(),
        _ => None,
    }
}

fn role_of_a_parameter(parameter: &Ident, ty: &Type) -> syn::Result<ParameterRole> {
    if let Type::Reference(reference) = ty
        && reference.mutability.is_none()
        && last_path_segment(&reference.elem)
            .is_some_and(|segment| segment.ident == "RuntimeContextFullAccess")
    {
        return Ok(ParameterRole::RuntimeContext);
    }
    let refusal = || {
        syn::Error::new(
            ty.span(),
            format!(
                "parameter `{parameter}` of a node function is not one a node function takes — \
                 each is `&RuntimeContextFullAccess<'_>`, `InputMailboxes`, `OutputWriter`, or \
                 `Input<T>` for the value input it is named after; a frame takes \
                 `&RuntimeContextLimitedAccess<'_>` as the per-frame function's argument"
            ),
        )
    };
    let Some(segment) = last_path_segment(ty) else {
        return Err(refusal());
    };
    match segment.ident.to_string().as_str() {
        "InputMailboxes" => Ok(ParameterRole::InputMailboxes),
        "OutputWriter" => Ok(ParameterRole::OutputWriter),
        "Input" if matches!(segment.arguments, PathArguments::AngleBracketed(_)) => {
            Ok(ParameterRole::ValueInput(parameter.clone()))
        }
        _ => Err(refusal()),
    }
}

fn refuse_a_parameter_the_node_does_not_declare(
    role: &ParameterRole,
    span: Span,
    parsed: &ParsedNodeAttr,
) -> syn::Result<()> {
    match role {
        ParameterRole::InputMailboxes if parsed.inputs.is_empty() => Err(syn::Error::new(
            span,
            "the node function takes `InputMailboxes`, and its node declares no stream input — \
             declare one with `input(\"…\", pin_type = …)`",
        )),
        ParameterRole::OutputWriter if parsed.outputs.is_empty() => Err(syn::Error::new(
            span,
            "the node function takes `OutputWriter`, and its node declares no output — declare \
             one with `output(\"…\", pin_type = …)`",
        )),
        ParameterRole::ValueInput(parameter)
            if !parsed
                .value_inputs
                .iter()
                .any(|value_input| value_input.declared_ident == *parameter) =>
        {
            Err(syn::Error::new(
                span,
                format!(
                    "parameter `{parameter}` reads a value input its node does not declare — \
                     declare it with `input(\"{parameter}\", value = <type>, default = …)`"
                ),
            ))
        }
        _ => Ok(()),
    }
}

/// The first type argument of `ty` when it names a `Result<…>`; `None` for any other type.
fn the_ok_type_of_a_result(ty: &mut Type) -> Option<&mut Type> {
    let Type::Path(result) = ty else {
        return None;
    };
    let segment = result.path.segments.last_mut()?;
    if segment.ident != "Result" {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &mut segment.arguments else {
        return None;
    };
    match arguments.args.first_mut()? {
        GenericArgument::Type(ok_type) => Some(ok_type),
        _ => None,
    }
}

/// How the body of a node that paces itself (`execution = manual`) returns: nothing, or
/// `Result<()>` when it can fail. It has no per-frame function: its computed values publish on
/// their own, and it takes each value set on it at once.
fn nothing_returned_by_a_node_pacing_itself(
    output: &mut ReturnType,
) -> syn::Result<WhatTheBodyReturns> {
    let ReturnType::Type(_, returned) = output else {
        return Ok(WhatTheBodyReturns {
            a_per_frame_function: false,
            in_a_result: false,
        });
    };
    if matches!(&**returned, Type::Tuple(unit) if unit.elems.is_empty()) {
        return Ok(WhatTheBodyReturns {
            a_per_frame_function: false,
            in_a_result: false,
        });
    }
    let returned_span = returned.span();
    match the_ok_type_of_a_result(returned) {
        Some(Type::Tuple(unit)) if unit.elems.is_empty() => Ok(WhatTheBodyReturns {
            a_per_frame_function: false,
            in_a_result: true,
        }),
        _ => Err(syn::Error::new(
            returned_span,
            "a node that paces itself (`execution = manual`) returns no per-frame function: its \
             computed values publish on their own, and it takes each value set on it at once — \
             return nothing, or `Result<()>` when its body can fail",
        )),
    }
}

/// How `output` returns the per-frame function, with `+ Send + 'static` added to its type
/// where the author left them out.
fn per_frame_function_returned(output: &mut ReturnType) -> syn::Result<WhatTheBodyReturns> {
    let refusal = |span: Span| {
        syn::Error::new(
            span,
            "a node function returns its per-frame function: `-> impl \
             FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>`, or `-> Result<impl \
             FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>>` when its body can fail",
        )
    };
    let ReturnType::Type(_, returned) = output else {
        return Err(refusal(output.span()));
    };
    let returned_span = returned.span();
    if let Type::ImplTrait(per_frame) = returned.as_mut() {
        add_send_and_static(&mut per_frame.bounds);
        return Ok(WhatTheBodyReturns {
            a_per_frame_function: true,
            in_a_result: false,
        });
    }
    match the_ok_type_of_a_result(returned) {
        Some(Type::ImplTrait(per_frame)) => {
            add_send_and_static(&mut per_frame.bounds);
            Ok(WhatTheBodyReturns {
                a_per_frame_function: true,
                in_a_result: true,
            })
        }
        _ => Err(refusal(returned_span)),
    }
}

fn add_send_and_static(bounds: &mut syn::punctuated::Punctuated<TypeParamBound, syn::Token![+]>) {
    let states_send = bounds.iter().any(|bound| {
        matches!(bound, TypeParamBound::Trait(bound)
            if bound.path.segments.last().is_some_and(|segment| segment.ident == "Send"))
    });
    let states_static = bounds.iter().any(
        |bound| matches!(bound, TypeParamBound::Lifetime(lifetime) if lifetime.ident == "static"),
    );
    if !states_send {
        bounds.push(syn::parse_quote! { ::core::marker::Send });
    }
    if !states_static {
        bounds.push(syn::parse_quote! { 'static });
    }
}
