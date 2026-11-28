// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The settings a view's manifest declares, as inputs of a node of that view, which `graph`
//! renders reading the node's config. Each is the input the node catalog already lists for it, as
//! for any config field of a node type not yet typed: read when the node is set up, taking no
//! wire, of exactly the pin type its JSON Schema states where it states one. Each stays a field
//! of the node's config, checked by the view's `settings` schema as before, so `graph` serves the
//! same config keys.

use crate::core::descriptors::{NodeDescriptor, NodeInputMode, PinDeclaration, PortDescriptor};
use crate::core::graph::input_values::{
    TakesAWireRendered, the_settings_rendered_reading_the_values_held,
};
use crate::core::json_schema::GraphNodeOutput;
use crate::core::nodes::{
    ConfigFieldDescribedAsASetting, NodeTypesOneStreamResolves,
    the_config_fields_described_as_settings,
};

/// Each setting `descriptor`, the node type of a view, declares in its manifest's `settings`, in
/// the order the manifest lists them, as an input read each run.
pub(crate) fn the_settings_of_a_view_node(descriptor: &NodeDescriptor) -> Vec<PortDescriptor> {
    the_config_fields_described_as_settings(descriptor)
        .into_iter()
        .map(a_view_setting_declared)
        .collect()
}

/// `setting` as a setting read when the node is set up: of exactly the pin type its schema
/// states, with the annotations it states, else of no pin type, with the default its schema
/// states.
fn a_view_setting_declared(setting: ConfigFieldDescribedAsASetting) -> PortDescriptor {
    let port = match setting.pin_type {
        Some(pin_type) => PortDescriptor::new(
            setting.name,
            setting.description,
            false,
            PinDeclaration {
                annotations: setting.annotations,
                ..PinDeclaration::exactly(pin_type)
            },
        ),
        None => {
            PortDescriptor::of_a_pin_declared_or_not(setting.name, setting.description, false, None)
        }
    }
    .with_input_mode(NodeInputMode::HandedToItsNode);
    match setting.default_value {
        Some(default) => port.with_pin_default(default),
        None => port,
    }
}

/// Put the settings of each view node among `nodes`, as `graph` renders them, before its other
/// inputs, each taking no wire and reading the value its config holds, else the default its
/// schema states. A node of a type that is no view of this stream's project is left as it is.
pub(crate) fn render_the_settings_of_each_view_node_into(
    nodes: &mut [GraphNodeOutput],
    node_types: &NodeTypesOneStreamResolves,
) {
    for node in nodes {
        if !node_types.is_a_view_type_of_this_streams_project(&node.node_type) {
            continue;
        }
        let Some(descriptor) = node_types.descriptor(&node.node_type) else {
            continue;
        };
        let settings = the_settings_of_a_view_node(&descriptor);
        let rendered = the_settings_rendered_reading_the_values_held(
            &settings,
            &node.input_values,
            TakesAWireRendered::NoneOfThemTakesOne,
        );
        node.ports.inputs.splice(0..0, rendered);
    }
}
