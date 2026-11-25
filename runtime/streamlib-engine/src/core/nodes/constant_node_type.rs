// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a `Constant` declares: its `pin_type` setting, a choice among the small types, and its
//! one `value` input and one `value` output, both of the type that setting chooses. A Constant
//! runs as the value source a top-level `GraphInput` runs as, without declaring an Input of its
//! stream.

use std::sync::LazyLock;

use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode, PinType,
    PortDescriptor, SettingReadTime, VectorComponentCount,
};
use crate::core::error::{Error, Result};
use crate::core::graph::graph_interface_resolution::GRAPH_INTERFACE_PIN_TYPE_SETTING;

use super::graph_interface_node_types::{setting, value_port};

/// The category a `Constant` is filed under.
const CONSTANT_NODE_TYPE_CATEGORY: &str = "Math";

/// The former name of a `Constant`'s `value` output, which a saved link may still name.
const THE_FORMER_NAME_OF_A_CONSTANT_OUTPUT: &str = "result";

/// The import path of `Constant`.
pub(crate) static THE_CONSTANT_NODE_TYPE: LazyLock<NodeClassImportPath> = LazyLock::new(|| {
    NodeClassImportPath::new(streamlib_node_schema::CONSTANT_NODE_TYPE)
        .expect("the Constant node type's import path names a class")
});

/// The types a `Constant`'s `pin_type` offers, in the order its menu lists them: every small
/// type a person sets by hand and wires into many inputs.
pub(crate) fn the_pin_types_a_constant_offers() -> Vec<PinType> {
    vec![
        PinType::Float { exact_size: None },
        PinType::Integer { exact_size: None },
        PinType::Boolean {},
        PinType::String {},
        PinType::Vector {
            component_count: VectorComponentCount::Two,
        },
        PinType::Vector {
            component_count: VectorComponentCount::Three,
        },
        PinType::Vector {
            component_count: VectorComponentCount::Four,
        },
        PinType::Color {},
    ]
}

/// The type a `Constant` holds while its `pin_type` holds none: a Float.
pub fn the_pin_type_a_constant_holds_by_default() -> PinType {
    PinType::Float { exact_size: None }
}

/// The descriptor of the `Constant` node type: its `pin_type` setting, read at setup, choosing
/// among [`the_pin_types_a_constant_offers`] and a Float until one is chosen; its `value` input,
/// a setting read each run that holds the value a person sets; and its `value` output, carrying
/// that value as a value, which keeps its last message for a reader that connects later. Both
/// `value` ports are of the type `pin_type` chooses.
fn the_node_type_of_a_constant() -> Result<NodeDescriptor> {
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Constant".to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        THE_CONSTANT_NODE_TYPE.clone(),
        "A value you set once and wire to many inputs: a Float, an Integer, a Boolean, a String, \
         a Vector or a Color, as its `pin_type` chooses."
            .to_string(),
    )
    .with_category(CONSTANT_NODE_TYPE_CATEGORY);
    descriptor.inputs = vec![
        setting(
            GRAPH_INTERFACE_PIN_TYPE_SETTING,
            "The type of the value the Constant holds.",
            PinType::PinTypeChoice {
                among: the_pin_types_a_constant_offers(),
                among_the_bundles_its_stream_declares: false,
            },
            false,
            Some(
                serde_json::to_value(the_pin_type_a_constant_holds_by_default())
                    .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
            ),
        ),
        the_value_port_of_a_constant(
            "The value the Constant holds; it takes no wire.",
            Vec::new(),
        )
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun,
        }),
    ];
    descriptor.outputs = vec![the_value_port_of_a_constant(
        "The value the Constant holds.",
        vec![THE_FORMER_NAME_OF_A_CONSTANT_OUTPUT.to_string()],
    )];
    Ok(descriptor)
}

/// The descriptor of `Constant`, built once.
pub(crate) static THE_DESCRIPTOR_OF_A_CONSTANT: LazyLock<NodeDescriptor> = LazyLock::new(|| {
    the_node_type_of_a_constant().expect("the Constant node type's names are valid")
});

/// The descriptor of `Constant`, a node type the runtime runs itself beside those its registry
/// builds.
pub fn the_descriptor_of_a_constant() -> NodeDescriptor {
    THE_DESCRIPTOR_OF_A_CONSTANT.clone()
}

/// A `value` port of a `Constant` stating `description`, carried as a value of the type its
/// `pin_type` chooses; one a person may clear, and a link naming any of `former_names` resolves
/// to.
fn the_value_port_of_a_constant(description: &str, former_names: Vec<String>) -> PortDescriptor {
    value_port(description, former_names, true)
}
