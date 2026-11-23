// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The settings a view's manifest declares, as inputs of a node of that view, which `graph`
//! renders reading the node's config. Each is the input the node catalog already lists for it, as
//! for any config field of a node type not yet typed: read when the node is set up, taking no
//! wire, of exactly the pin type its JSON Schema states where it states one. Each stays a field
//! of the node's config, checked by the view's `settings` schema as before, so `graph` serves the
//! same config keys.

use crate::core::descriptors::{
    NodeDescriptor, NodeInputMode, PinDeclaration, PortDescriptor, PortFamily,
    PortFamilyDeclaration, SettingReadTime,
};
use crate::core::graph::input_values::{
    TakesAWireRendered, the_settings_rendered_reading_the_values_held,
};
use crate::core::graph::legacy_config_of_a_node::{
    ConfigFieldDescribedAsASetting, the_config_fields_described_as_settings,
};
use crate::core::json_schema::GraphNodeOutput;
use crate::core::nodes::NodeTypesOneStreamResolves;

/// Each setting `descriptor`, the node type of a view, declares in its manifest's `settings`, in
/// the order the manifest lists them, as an input that takes no wire.
pub(crate) fn the_settings_of_a_view_node(descriptor: &NodeDescriptor) -> Vec<PortDescriptor> {
    the_config_fields_described_as_settings(descriptor)
        .into_iter()
        .map(a_view_setting_declared)
        .collect()
}

/// `setting` as a setting read when the node is set up: of exactly the pin type its schema
/// states, with the annotations it states, else of no pin type and on Records, the family that
/// carries any value, with the default its schema states.
fn a_view_setting_declared(setting: ConfigFieldDescribedAsASetting) -> PortDescriptor {
    let family = setting
        .pin_type
        .as_ref()
        .and_then(PortFamily::carrying)
        .unwrap_or(PortFamily::Records);
    let mut port = PortDescriptor::new(
        setting.name,
        setting.description,
        false,
        PortFamilyDeclaration::of_family(family),
    )
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    });
    if let Some(pin_type) = setting.pin_type {
        port = port.with_pin_declaration(PinDeclaration {
            annotations: setting.annotations,
            ..PinDeclaration::exactly(pin_type)
        });
    }
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
        let config_held = node
            .config
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let rendered = the_settings_rendered_reading_the_values_held(
            &settings,
            &config_held,
            TakesAWireRendered::NoneOfThemTakesOne,
        );
        node.ports.inputs.splice(0..0, rendered);
    }
}
