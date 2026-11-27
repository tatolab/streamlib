// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a node emits whether it is written as a struct or as one function: the module holding
//! it, its port mailboxes, its inherent `impl Node` and the members of `GeneratedNode` that read
//! only its schema.

use proc_macro2::{Ident, TokenStream};
use quote::quote;
use streamlib_node_schema::{NodeSchema, NodeSchemaExecution};

use super::node_settings_emit::NodeSettingsEmitted;
use super::{generate_iceoryx2_accessors_from_schema, node_type_minted};

/// The module a node is emitted in, named after the author's node, holding `body`.
pub(super) fn node_module(
    module_visibility: &syn::Visibility,
    module_name: &Ident,
    sdk_root: &TokenStream,
    member_visibility: &TokenStream,
    settings: &NodeSettingsEmitted<'_>,
    body: TokenStream,
) -> TokenStream {
    let module_alias_and_constructor = settings.module_alias_and_constructor(member_visibility);
    quote! {
        #[allow(non_snake_case, reason = "the generated module is named after the author's node type")]
        #module_visibility mod #module_name {
            use super::*;

            // Alias the consumer's real SDK crate (streamlib-plugin-sdk for
            // plugins, the streamlib facade for hosts) so the emitted paths
            // below resolve without any `streamlib` aliasing in the consumer.
            #[allow(unused_imports, reason = "the generated module may name nothing from the SDK root")]
            use #sdk_root as __streamlib_sdk;

            #module_alias_and_constructor

            /// This node type's identity: its class in `tatolab.stream` for a
            /// built-in, else the module path captured where the macro expanded.
            #[allow(dead_code, reason = "the identity function is emitted for every node, and not every crate calls it")]
            #member_visibility fn node_class_import_path()
                -> __streamlib_sdk::descriptors::NodeClassImportPath
            {
                Node::node_class_import_path()
            }

            #body
        }
    }
}

/// The `ProcessExecution` a node's declared execution names.
pub(super) fn execution_variant(execution: &NodeSchemaExecution) -> TokenStream {
    match execution {
        NodeSchemaExecution::Reactive => {
            quote! { __streamlib_sdk::execution::ProcessExecution::Reactive }
        }
        NodeSchemaExecution::Manual => {
            quote! { __streamlib_sdk::execution::ProcessExecution::Manual }
        }
        NodeSchemaExecution::Continuous { interval_ms } => quote! {
            __streamlib_sdk::execution::ProcessExecution::Continuous { interval_ms: #interval_ms }
        },
    }
}

/// The fields of `Node` holding its stream ports' mailboxes, for the ports it declares.
pub(super) fn ipc_mailbox_fields(
    schema: &NodeSchema,
    member_visibility: &TokenStream,
) -> TokenStream {
    let input_field = (!schema.inputs.is_empty()).then(|| {
        quote! { #member_visibility inputs: __streamlib_sdk::iceoryx2::InputMailboxes, }
    });
    let output_field = (!schema.outputs.is_empty()).then(|| {
        quote! { #member_visibility outputs: __streamlib_sdk::iceoryx2::OutputWriter, }
    });
    quote! { #input_field #output_field }
}

/// The initializers of `ipc_mailbox_fields`, each mailbox empty until the engine wires it.
pub(super) fn ipc_mailbox_inits(schema: &NodeSchema) -> TokenStream {
    let input_init = (!schema.inputs.is_empty())
        .then(|| quote! { inputs: __streamlib_sdk::iceoryx2::InputMailboxes::empty(), });
    let output_init = (!schema.outputs.is_empty())
        .then(|| quote! { outputs: __streamlib_sdk::iceoryx2::OutputWriter::empty(), });
    quote! { #input_init #output_init }
}

/// `impl Node`: its name, identity, `NodeSpec` constructor and execution mode.
pub(super) fn node_inherent_impl(
    schema: &NodeSchema,
    built_in_node: bool,
    member_visibility: &TokenStream,
    settings: &NodeSettingsEmitted<'_>,
) -> TokenStream {
    let node_class_short_name = &schema.name;
    let node_type_minted = node_type_minted(built_in_node, node_class_short_name);
    let node_spec_constructor = settings.node_spec_constructor(member_visibility);
    let execution_variant = execution_variant(&schema.execution);
    let execution_description = match schema.execution {
        NodeSchemaExecution::Reactive => "Reactive",
        NodeSchemaExecution::Manual => "Manual",
        NodeSchemaExecution::Continuous { .. } => "Continuous",
    };
    quote! {
        impl Node {
            #member_visibility const NAME: &'static str = #node_class_short_name;

            /// This node type's identity: its class in `tatolab.stream` for a
            /// built-in, else the module path captured where the macro expanded.
            #member_visibility fn node_class_import_path()
                -> __streamlib_sdk::descriptors::NodeClassImportPath
            {
                #node_type_minted
            }

            #node_spec_constructor

            #member_visibility fn execution_mode(&self) -> __streamlib_sdk::execution::ProcessExecution {
                #execution_variant
            }

            /// Returns a human-readable description of the execution mode.
            #member_visibility fn execution_mode_description(&self) -> &'static str {
                #execution_description
            }
        }
    }
}

/// The members of `GeneratedNode` read only from the node's schema and its descriptor: its
/// config, name, execution config, descriptor and port accessors.
pub(super) fn generated_node_members_read_from_the_schema(
    schema: &NodeSchema,
    descriptor_impl: &TokenStream,
) -> TokenStream {
    let execution_variant = execution_variant(&schema.execution);
    let iceoryx2_accessors = generate_iceoryx2_accessors_from_schema(schema);
    quote! {
        type Config = __streamlib_sdk::nodes::EmptyConfig;

        fn name(&self) -> &str {
            Self::NAME
        }

        fn execution_config(&self) -> __streamlib_sdk::execution::ExecutionConfig {
            __streamlib_sdk::execution::ExecutionConfig {
                execution: #execution_variant,
            }
        }

        #descriptor_impl
        #iceoryx2_accessors
    }
}
