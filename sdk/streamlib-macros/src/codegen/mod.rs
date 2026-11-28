// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream;
use quote::quote;
use streamlib_node_schema::NodeSchema;

use crate::grammar::{
    DeclaredDescription, ParsedNodeAttr, ParsedPort, declared_description_str_expression,
};

mod author_visibility;
mod node_function_emit;
mod node_scaffold_emit;
mod pin_declaration_tokens;
mod port_marker_module;

use pin_declaration_tokens::pin_declaration_tokens;

pub(crate) use node_function_emit::{
    generate_from_a_node_function, node_name_of_a_node_function,
    refusal_of_a_node_not_written_as_a_function,
};

/// The expression minting a node type's identity: a built-in's class in
/// `tatolab.stream`, else the module path captured where the macro expands.
fn node_type_minted(built_in_node: bool, node_class_short_name: &str) -> TokenStream {
    if built_in_node {
        quote! {
            __streamlib_sdk::descriptors::NodeClassImportPath::of_built_in_node(
                #node_class_short_name,
            )
        }
    } else {
        quote! {
            __streamlib_sdk::descriptors::NodeClassImportPath::new(::core::module_path!())
                .expect("module_path! always names the enclosing module")
        }
    }
}

/// The node's declared category, description and conversion and each port's
/// description, beside the schema the descriptor is emitted from: the schema
/// carries no category or conversion, and a `const` description has no text it
/// could carry.
struct NodeDeclaredMetaAndPortDescriptions<'parsed> {
    node_description: Option<&'parsed DeclaredDescription>,
    category: Option<&'parsed str>,
    conversion: bool,
    inputs: Vec<Option<&'parsed DeclaredDescription>>,
    outputs: Vec<Option<&'parsed DeclaredDescription>>,
}

impl<'parsed> NodeDeclaredMetaAndPortDescriptions<'parsed> {
    fn of(parsed: &'parsed ParsedNodeAttr) -> Self {
        let of_ports = |ports: &'parsed [ParsedPort]| {
            ports.iter().map(|port| port.description.as_ref()).collect()
        };
        Self {
            node_description: parsed.description.as_ref(),
            category: parsed.category.as_deref(),
            conversion: parsed.conversion,
            inputs: of_ports(&parsed.inputs),
            outputs: of_ports(&parsed.outputs),
        }
    }
}

/// The descriptor a node's schema and its declared meta and port descriptions build; `value_inputs`, the `.with_input(…)` calls of a node function's value inputs, follow
/// its port inputs.
fn generate_descriptor_from_schema(
    schema: &NodeSchema,
    declared_meta_and_port_descriptions: &NodeDeclaredMetaAndPortDescriptions<'_>,
    value_inputs: &TokenStream,
) -> TokenStream {
    let description =
        declared_description_str_expression(declared_meta_and_port_descriptions.node_description);
    let repository = "https://github.com/tatolab/streamlib";

    // iceoryx2-based input ports
    let ipc_input_ports: Vec<TokenStream> = schema
        .inputs
        .iter()
        .zip(&declared_meta_and_port_descriptions.inputs)
        .map(|(p, declared_description)| {
            let port_name = &p.name;
            let port_desc = declared_description_str_expression(*declared_description);
            let delivery_profile_tokens = match p.delivery_profile.as_deref() {
                Some(value) => quote! { ::std::option::Option::Some(#value.to_string()) },
                None => quote! { ::std::option::Option::None },
            };
            let audio_window_tokens = audio_window_contract_tokens(p.audio_window.as_ref());
            let pin_declaration_tokens = pin_declaration_tokens(
                p.pin_type.as_ref(),
                p.pin_type_from_input.as_deref(),
                p.flow,
                &p.former_names,
                p.may_be_unset,
                &p.annotations,
            );
            let borrow_quota_tokens = match p.borrow_quota {
                Some(quota) => quote! { ::std::option::Option::Some(#quota) },
                None => quote! { ::std::option::Option::None },
            };
            let lifetime_tokens = node_value_lifetime_tokens(p.lifetime);
            let input_mode_tokens = if p.takes_a_value {
                quote! {
                    ::std::option::Option::Some(
                        __streamlib_sdk::descriptors::NodeInputMode::WireOrValue
                    )
                }
            } else {
                quote! { ::std::option::Option::None }
            };
            quote! {
                .with_input(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    required: true,
                    delivery_profile: #delivery_profile_tokens,
                    borrow_quota: #borrow_quota_tokens,
                    audio_window: #audio_window_tokens,
                    pin_default: ::std::option::Option::None,
                    pin_declaration: #pin_declaration_tokens,
                    lifetime: #lifetime_tokens,
                    input_mode: #input_mode_tokens,
                })
            }
        })
        .collect();

    // iceoryx2-based output ports
    let no_audio_window_tokens = audio_window_contract_tokens(None);
    let ipc_output_ports: Vec<TokenStream> = schema
        .outputs
        .iter()
        .zip(&declared_meta_and_port_descriptions.outputs)
        .map(|(p, declared_description)| {
            let port_name = &p.name;
            let port_desc = declared_description_str_expression(*declared_description);
            let pin_declaration_tokens = pin_declaration_tokens(
                p.pin_type.as_ref(),
                p.pin_type_from_input.as_deref(),
                p.flow,
                &p.former_names,
                p.may_be_unset,
                &p.annotations,
            );
            quote! {
                .with_output(__streamlib_sdk::descriptors::PortDescriptor {
                    name: #port_name.to_string(),
                    description: #port_desc.to_string(),
                    required: true,
                    delivery_profile: ::std::option::Option::None,
                    borrow_quota: ::std::option::Option::None,
                    audio_window: #no_audio_window_tokens,
                    pin_default: ::std::option::Option::None,
                    pin_declaration: #pin_declaration_tokens,
                    lifetime: __streamlib_sdk::descriptors::NodeValueLifetime::Kept,
                    input_mode: ::std::option::Option::None,
                })
            }
        })
        .collect();

    // Reached through the bound-carrying trait rather than `schemars` directly, so a config
    // type missing the derive fails on a diagnostic that names the fix.
    let config_schema = quote! {
        .with_config_schema(
            <__streamlib_sdk::nodes::EmptyConfig as __streamlib_sdk::descriptors::NodeConfigJsonSchema>
                ::node_config_schema_document()
        )
    };

    let scheduling = schema.scheduling.as_ref().map(|s| {
        let priority_tokens = thread_priority_tokens(s.priority);
        quote! {
            .with_scheduling(__streamlib_sdk::descriptors::NodeScheduling {
                priority: #priority_tokens,
            })
        }
    });

    let paced_by_real_time = schema
        .paced_by_real_time
        .then(|| quote! { .paced_by_real_time() });

    let category = declared_meta_and_port_descriptions
        .category
        .map(|category| quote! { .with_category(#category) });

    let conversion = declared_meta_and_port_descriptions
        .conversion
        .then(|| quote! { .stating_a_conversion() });

    // `module_path!()` resolves here to `<author's module>::<node name>`, because this
    // expansion lands inside the module the macro names after the author's node function: the
    // generated module's own path *is* the node type's path.
    quote! {
        fn descriptor() -> Option<__streamlib_sdk::descriptors::NodeDescriptor> {
            Some(
                __streamlib_sdk::descriptors::NodeDescriptor::new(
                    __streamlib_sdk::descriptors::NodeClassShortName::new(Node::NAME)
                        .expect("a node function's name is never blank"),
                    Node::node_class_import_path(),
                    #description,
                )
                    .with_repository(#repository)
                    #config_schema
                    #scheduling
                    #paced_by_real_time
                    #category
                    #conversion
                    #(#ipc_input_ports)*
                    #value_inputs
                    #(#ipc_output_ports)*
            )
        }
    }
}

fn thread_priority_tokens(priority: streamlib_node_schema::ThreadPriority) -> TokenStream {
    use streamlib_node_schema::ThreadPriority;
    match priority {
        ThreadPriority::RealTime => quote! { __streamlib_sdk::execution::ThreadPriority::RealTime },
        ThreadPriority::High => quote! { __streamlib_sdk::execution::ThreadPriority::High },
        ThreadPriority::Normal => quote! { __streamlib_sdk::execution::ThreadPriority::Normal },
    }
}

/// Generate iceoryx2 accessor methods from schema.
fn generate_iceoryx2_accessors_from_schema(schema: &NodeSchema) -> TokenStream {
    let has_iceoryx2_outputs = !schema.outputs.is_empty();
    let has_iceoryx2_inputs = !schema.inputs.is_empty();

    if !has_iceoryx2_outputs && !has_iceoryx2_inputs {
        return quote! {};
    }

    let has_outputs_impl = if has_iceoryx2_outputs {
        quote! {
            fn has_iceoryx2_outputs(&self) -> bool {
                true
            }
        }
    } else {
        quote! {}
    };

    let has_inputs_impl = if has_iceoryx2_inputs {
        quote! {
            fn has_iceoryx2_inputs(&self) -> bool {
                true
            }
        }
    } else {
        quote! {}
    };

    // Emit `set_iceoryx2_resources` to receive host-allocated handles +
    // the `iceoryx2_output_writer_inner` / `iceoryx2_input_mailboxes_inner`
    // accessors so the host's wiring path can mutate the inner Arc
    // directly. The host owns per-port registration
    // (`InputMailboxesInner::add_port`) at wire time, where it reads the
    // destination input port's declared delivery profile — the macro
    // registers none.
    let assign_outputs = if has_iceoryx2_outputs {
        quote! {
            if let ::std::option::Option::Some(ow) = output_writer {
                self.outputs = ow;
            }
        }
    } else {
        quote! {}
    };

    let assign_inputs = if has_iceoryx2_inputs {
        quote! {
            if let ::std::option::Option::Some(im) = input_mailboxes {
                self.inputs = im;
            }
        }
    } else {
        quote! {
            let _ = input_mailboxes;
        }
    };

    let outputs_inner_impl = if has_iceoryx2_outputs {
        quote! {
            fn iceoryx2_output_writer_inner(
                &self,
            ) -> ::std::option::Option<::std::sync::Arc<__streamlib_sdk::iceoryx2::OutputWriterInner>> {
                self.outputs.inner_arc()
            }
        }
    } else {
        quote! {}
    };

    let inputs_inner_impl = if has_iceoryx2_inputs {
        quote! {
            fn iceoryx2_input_mailboxes_inner(
                &self,
            ) -> ::std::option::Option<::std::sync::Arc<__streamlib_sdk::iceoryx2::InputMailboxesInner>> {
                self.inputs.inner_arc()
            }
        }
    } else {
        quote! {}
    };

    let set_resources_impl = quote! {
        fn set_iceoryx2_resources(
            &mut self,
            output_writer: ::std::option::Option<__streamlib_sdk::iceoryx2::OutputWriter>,
            input_mailboxes: ::std::option::Option<__streamlib_sdk::iceoryx2::InputMailboxes>,
        ) -> __streamlib_sdk::error::Result<()> {
            #assign_outputs
            #assign_inputs
            ::std::result::Result::Ok(())
        }
    };

    quote! {
        #has_outputs_impl
        #has_inputs_impl
        #set_resources_impl
        #outputs_inner_impl
        #inputs_inner_impl
    }
}

/// Emit the `lifetime` an input declared.
fn node_value_lifetime_tokens(lifetime: streamlib_node_schema::NodeValueLifetime) -> TokenStream {
    use streamlib_node_schema::NodeValueLifetime;

    match lifetime {
        NodeValueLifetime::Kept => quote! { __streamlib_sdk::descriptors::NodeValueLifetime::Kept },
        NodeValueLifetime::Reset => {
            quote! { __streamlib_sdk::descriptors::NodeValueLifetime::Reset }
        }
    }
}

/// Emit the `audio_window` a port declared, reconstructing the contract in the
/// generated descriptor.
///
/// The values are already validated by the grammar, so this only has to render
/// them — a contract that gets this far is one the read-side stage can honour.
fn audio_window_contract_tokens(
    audio_window: Option<&streamlib_node_schema::AudioWindowContract>,
) -> TokenStream {
    use streamlib_node_schema::AudioWindowContract;

    match audio_window {
        None => quote! { ::std::option::Option::None },
        // Rendering-only: `graph` uses it to say five values came from a device
        // rather than from an author, and the grammar this codegen reads has no
        // spelling that produces one. A `compile_error!` rather than a panic
        // because the only way to reach it is a future grammar change, and that
        // change should fail the build that introduced it.
        Some(AudioWindowContract::Device(_)) => quote! {
            compile_error!(
                "`audio_window` resolved from a device is a rendering, not a declaration \
                 — a port states its own values or `match_device`"
            )
        },
        Some(AudioWindowContract::MatchDevice {}) => quote! {
            ::std::option::Option::Some(
                __streamlib_sdk::descriptors::AudioWindowContract::MatchDevice {},
            )
        },
        Some(AudioWindowContract::Declaration(values)) => {
            let sample_rate = values.sample_rate;
            let channels = match values.channels {
                Some(channels) => quote! { ::std::option::Option::Some(#channels) },
                None => quote! { ::std::option::Option::None },
            };
            let dtype = &values.dtype;
            let window_size = values.window_size;
            let hop = values.hop;
            quote! {
                ::std::option::Option::Some(
                    __streamlib_sdk::descriptors::AudioWindowContract::Declaration(
                        __streamlib_sdk::descriptors::AudioWindowContractDeclaredValues {
                            sample_rate: #sample_rate,
                            channels: #channels,
                            dtype: #dtype.to_string(),
                            window_size: #window_size,
                            hop: #hop,
                        },
                    ),
                )
            }
        }
    }
}

#[cfg(test)]
mod pin_declaration_tokens_tests;

#[cfg(test)]
mod node_function_emit_tests;
