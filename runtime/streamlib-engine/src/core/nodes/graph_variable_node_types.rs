// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a `SetVariable` and a `GetVariable` declare: each of their settings as an input, and
//! their one `value` port, which carries any type: the type of the port linked into the setter.

use std::sync::LazyLock;

use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, NodeDescriptor, PinAnnotations, PinDeclaration, PinFlow,
    PinType, PortDescriptor,
};
use crate::core::error::Result;
use crate::core::graph::graph_variable_resolution::{
    GET_VARIABLE_NODE_TYPE, GRAPH_VARIABLE_NAME_CONFIG_KEY, GRAPH_VARIABLE_VALUE_PORT_NAME,
    SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY, SET_VARIABLE_NODE_TYPE,
};

use super::graph_interface_node_types::setting;
use super::node_types_of_the_graph_itself::node_type_of_the_graph_itself;

/// What a `SetVariable`'s `value` input states.
const SET_VARIABLE_DESCRIPTION_OF_VALUE: &str =
    "The port whose output the variable holds, of that port's type.";

/// What a `GetVariable`'s `value` output states.
const GET_VARIABLE_DESCRIPTION_OF_VALUE: &str =
    "The port's output the variable holds, of that port's type.";

/// What a `GetVariable`'s `variable` setting states.
const GET_VARIABLE_DESCRIPTION_OF_VARIABLE: &str =
    "The variable it reads, set on its level or an enclosing one.";

/// The settings of the `SetVariable` node type, built once.
static THE_SETTINGS_OF_A_SET_VARIABLE: LazyLock<Vec<PortDescriptor>> =
    LazyLock::new(|| the_settings_of(the_node_type_of_a_set_variable()));

/// The settings of the `GetVariable` node type, built once.
static THE_SETTINGS_OF_A_GET_VARIABLE: LazyLock<Vec<PortDescriptor>> =
    LazyLock::new(|| the_settings_of(the_node_type_of_a_get_variable()));

/// The settings a node of `node_type` takes when it is a `SetVariable` or a `GetVariable`, as
/// its type declares them, each read at setup: `variable`, and on a `SetVariable`
/// `node_reference`; `None` for any other type.
pub(crate) fn the_settings_of_a_graph_variable_node(
    node_type: &NodeClassImportPath,
) -> Option<&'static [PortDescriptor]> {
    match node_type.as_str() {
        SET_VARIABLE_NODE_TYPE => Some(&THE_SETTINGS_OF_A_SET_VARIABLE),
        GET_VARIABLE_NODE_TYPE => Some(&THE_SETTINGS_OF_A_GET_VARIABLE),
        _ => None,
    }
}

/// The settings of a `GetVariable` in a stream whose setters on its level and the enclosing
/// ones set `variables`: its `variable`, a menu of them.
pub(crate) fn the_settings_of_a_get_variable_offering(variables: &[String]) -> Vec<PortDescriptor> {
    vec![setting(
        GRAPH_VARIABLE_NAME_CONFIG_KEY,
        GET_VARIABLE_DESCRIPTION_OF_VARIABLE,
        PinType::Menu {
            option_names: variables.to_vec(),
        },
        false,
        None,
    )]
}

/// The `value` port of a variable node of `node_type`, as its type declares it: the input of
/// a `SetVariable`, the output of a `GetVariable`; `None` for any other type.
pub(crate) fn the_value_port_of_a_graph_variable_node(
    node_type: &NodeClassImportPath,
) -> Option<PortDescriptor> {
    match node_type.as_str() {
        SET_VARIABLE_NODE_TYPE => Some(value_port(SET_VARIABLE_DESCRIPTION_OF_VALUE)),
        GET_VARIABLE_NODE_TYPE => Some(value_port(GET_VARIABLE_DESCRIPTION_OF_VALUE)),
        _ => None,
    }
}

/// The descriptor of the `SetVariable` node type: its settings `variable` and
/// `node_reference`, each read at setup, and its `value` input, the port linked into it, of
/// any type.
pub(crate) fn the_node_type_of_a_set_variable() -> Result<NodeDescriptor> {
    let mut descriptor = node_type_of_the_graph_itself(
        "SetVariable",
        SET_VARIABLE_NODE_TYPE,
        "Names one value under `variable`: the port's output linked into `value`, or the node \
         in `node_reference`. Every `GetVariable` of the name on its level, or on a level \
         inside it, reads that value with no wire drawn.",
    )?;
    descriptor.inputs = vec![
        variable_setting("The variable's name, which every `GetVariable` of it reads."),
        setting(
            SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY,
            "The node the variable holds, of any type; unset, the variable holds the port \
             linked into `value`.",
            PinType::NodeReference {
                referenced_node_type: None,
                one_referrer_per_referenced_node: false,
            },
            true,
            None,
        ),
        value_port(SET_VARIABLE_DESCRIPTION_OF_VALUE),
    ];
    Ok(descriptor)
}

/// The descriptor of the `GetVariable` node type: its setting `variable`, read at setup, and
/// its `value` output, carrying what the variable's setter holds, of the type of the port
/// linked into that setter.
pub(crate) fn the_node_type_of_a_get_variable() -> Result<NodeDescriptor> {
    let mut descriptor = node_type_of_the_graph_itself(
        "GetVariable",
        GET_VARIABLE_NODE_TYPE,
        "Reads the variable `variable` from the setter on its own level, or else from the \
         nearest enclosing level's: a port's output by linking `value`, a node by naming this \
         node in a node reference.",
    )?;
    descriptor.inputs = vec![variable_setting(GET_VARIABLE_DESCRIPTION_OF_VARIABLE)];
    descriptor.outputs = vec![value_port(GET_VARIABLE_DESCRIPTION_OF_VALUE)];
    Ok(descriptor)
}

/// The inputs of the variable node type `descriptor` other than its `value`: the settings it
/// reads at setup.
fn the_settings_of(descriptor: Result<NodeDescriptor>) -> Vec<PortDescriptor> {
    descriptor
        .expect("a variable node type's names are valid")
        .inputs
        .into_iter()
        .filter(|input| input.name != GRAPH_VARIABLE_VALUE_PORT_NAME)
        .collect()
}

/// The `variable` setting: the variable's name, a String every variable node holds.
fn variable_setting(description: &str) -> PortDescriptor {
    setting(
        GRAPH_VARIABLE_NAME_CONFIG_KEY,
        description,
        PinType::String {},
        false,
        None,
    )
}

/// A `value` port stating `description`, carried as a value of any type: the type of the port
/// linked into the setter.
fn value_port(description: &str) -> PortDescriptor {
    PortDescriptor::new(
        GRAPH_VARIABLE_VALUE_PORT_NAME,
        description,
        false,
        PinDeclaration {
            pin_type: DeclaredPinType::Any,
            flow: PinFlow::Value,
            annotations: PinAnnotations::default(),
            former_names: Vec::new(),
            may_be_unset: false,
        },
    )
}
