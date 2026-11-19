// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A registered node type's inputs and outputs as `describe_node_types` answers them: each port
//! with its pin type, flow, mode, default, lifetime and the JSON Schema of a value it takes, and
//! each setting of a node type not yet typed as an input that takes no wire.

use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeTypeInputDescribed, NodeTypeInputMode, NodeTypePortDescribed, NodeTypePortPin,
};

use crate::core::descriptors::{
    NodeDescriptor, NodeInputMode, NodeValueLifetime, PinDeclaration, PortDescriptor,
    SettingReadTime,
};
use crate::core::graph::legacy_config_of_a_node::{
    ConfigFieldDescribedAsASetting, the_config_fields_a_node_of_the_type_can_expose,
    the_config_fields_described_as_settings,
};

/// A registered node type's inputs and outputs, and the settings `set_exposed_config_fields`
/// still takes on it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeTypePortsDescribedOnTheWire {
    /// Each input port, then each setting.
    pub inputs: Vec<NodeTypeInputDescribed>,
    /// Each output port.
    pub outputs: Vec<NodeTypePortDescribed>,
    /// The settings of a node type not yet typed that `set_exposed_config_fields` takes.
    pub exposable_config_fields: Vec<String>,
}

impl NodeTypePortsDescribedOnTheWire {
    /// The inputs and outputs `descriptor` states.
    pub fn of(descriptor: &NodeDescriptor) -> Self {
        Self {
            inputs: descriptor
                .inputs
                .iter()
                .map(an_input_port_described)
                .chain(
                    the_config_fields_described_as_settings(descriptor)
                        .into_iter()
                        .map(a_setting_described),
                )
                .collect(),
            outputs: descriptor.outputs.iter().map(a_port_described).collect(),
            exposable_config_fields: the_config_fields_a_node_of_the_type_can_expose(
                &descriptor.node_class_import_path,
                descriptor.config_schema.as_ref(),
            )
            .into_iter()
            .map(|(field, _)| field)
            .collect(),
        }
    }
}

/// `port` as an output, or as the port part of an input: typed where it declares a pin, else by
/// its family alone.
fn a_port_described(port: &PortDescriptor) -> NodeTypePortDescribed {
    let declared = port.declared_pin();
    NodeTypePortDescribed {
        name: port.name.clone(),
        documentation: port.description.clone(),
        pin: declared.as_ref().map(|declared| NodeTypePortPin {
            pin_type: declared.pin_type.clone(),
            flow: declared.flow,
        }),
        annotations: declared
            .as_ref()
            .map(|declared| declared.annotations.clone())
            .unwrap_or_default(),
        family: declared
            .is_none()
            .then(|| serde_json::to_value(&port.family).ok())
            .flatten(),
        annotation: None,
    }
}

/// `port` as an input: its mode, the one it declares, else a link and a value where its type
/// holds a stored value and a link alone where it does not.
fn an_input_port_described(port: &PortDescriptor) -> NodeTypeInputDescribed {
    let declared = port.declared_pin();
    let mode = port
        .input_mode
        .unwrap_or_else(|| the_mode_of_an_input_declaring_none(declared.as_ref()));
    let (mode, read) = NodeTypeInputMode::of(mode);
    NodeTypeInputDescribed {
        port: a_port_described(port),
        mode: Some(mode),
        read,
        default_value: port.pin_default.clone(),
        lifetime: Some(port.lifetime),
        value_json_schema: declared
            .as_ref()
            .and_then(PinDeclaration::value_json_schema),
    }
}

/// The mode of an input port that declares none: a link only where its pin type holds no stored
/// value, else a link or a value.
fn the_mode_of_an_input_declaring_none(declared: Option<&PinDeclaration>) -> NodeInputMode {
    let holds_no_stored_value = declared
        .and_then(|declared| declared.clone().into_resolved())
        .is_some_and(|resolved| !resolved.pin_type.holds_a_stored_value());
    if holds_no_stored_value {
        NodeInputMode::WireOnly
    } else {
        NodeInputMode::WireOrValue
    }
}

/// A config field of a node type not yet typed, as an input that takes no wire, read when the
/// node is set up.
fn a_setting_described(setting: ConfigFieldDescribedAsASetting) -> NodeTypeInputDescribed {
    let declared = setting.pin_type.map(|pin_type| PinDeclaration {
        annotations: setting.annotations.clone(),
        ..PinDeclaration::exactly(pin_type)
    });
    let (mode, read) = NodeTypeInputMode::of(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    });
    NodeTypeInputDescribed {
        port: NodeTypePortDescribed {
            name: setting.name,
            documentation: setting.description,
            pin: declared.as_ref().map(|declared| NodeTypePortPin {
                pin_type: declared.pin_type.clone(),
                flow: declared.flow,
            }),
            annotations: setting.annotations,
            family: None,
            annotation: None,
        },
        mode: Some(mode),
        read,
        default_value: setting.default_value,
        lifetime: Some(NodeValueLifetime::Kept),
        value_json_schema: declared
            .as_ref()
            .and_then(PinDeclaration::value_json_schema),
    }
}
