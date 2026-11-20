// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a `SetVariable` and a `GetVariable` declare: each of their settings as an input. Their
//! one `value` port is of the type of the port linked into the setter, so no setting chooses it.

use std::sync::LazyLock;

use crate::core::descriptors::{NodeClassImportPath, PinType, PortDescriptor};
use crate::core::graph::graph_variable_resolution::{
    GET_VARIABLE_NODE_TYPE, GRAPH_VARIABLE_NAME_CONFIG_KEY, SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY,
    SET_VARIABLE_NODE_TYPE,
};

use super::graph_interface_node_types::setting;

/// The settings of the `SetVariable` node type, built once.
static THE_SETTINGS_OF_A_SET_VARIABLE: LazyLock<Vec<PortDescriptor>> = LazyLock::new(|| {
    vec![
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
    ]
});

/// The settings of the `GetVariable` node type, built once.
static THE_SETTINGS_OF_A_GET_VARIABLE: LazyLock<Vec<PortDescriptor>> = LazyLock::new(|| {
    vec![variable_setting(
        "The variable it reads, set on its level or an enclosing one.",
    )]
});

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
