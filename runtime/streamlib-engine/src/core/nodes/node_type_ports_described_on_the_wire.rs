// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A registered node type's inputs and outputs as `describe_node_types` answers them: each port
//! with its pin type, flow, mode, default, lifetime and the JSON Schema of a value it takes, and
//! each setting of a node type not yet typed as an input that takes no wire, with the schema of
//! its value.

use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeTypeInputDescribed, NodeTypeInputMode, NodeTypePinGroupDescribed, NodeTypePinGroupSlots,
    NodeTypePortDescribed, NodeTypePortPin,
};

use crate::core::descriptors::{
    NodeDescriptor, NodeInputMode, NodeKind, NodeValueLifetime, PinDeclaration, PinGroupSlotCount,
    PortDescriptor, SettingReadTime,
};
use crate::core::graph::PortInfo;
use crate::core::graph::input_values::the_input_port_takes_a_value;
use crate::core::nodes::{ConfigFieldDescribedAsASetting, the_config_fields_described_as_settings};

/// A registered node type's inputs and outputs.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeTypePortsDescribedOnTheWire {
    /// Each input port, then each setting.
    pub inputs: Vec<NodeTypeInputDescribed>,
    /// Each output port.
    pub outputs: Vec<NodeTypePortDescribed>,
    /// Each pin group, its slot's ports each named as its pin.
    pub pin_groups: Vec<NodeTypePinGroupDescribed>,
}

impl NodeTypePortsDescribedOnTheWire {
    /// The inputs and outputs `descriptor` states.
    pub fn of(descriptor: &NodeDescriptor) -> Self {
        Self {
            inputs: descriptor
                .the_inputs_with_slots(&vec![0; descriptor.pin_groups.len()])
                .iter()
                .map(|port| an_input_port_described(descriptor, port))
                .chain(
                    the_config_fields_described_as_settings(descriptor)
                        .into_iter()
                        .map(a_setting_described),
                )
                .collect(),
            outputs: descriptor.outputs.iter().map(a_port_described).collect(),
            pin_groups: descriptor
                .pin_groups
                .iter()
                .map(|group| NodeTypePinGroupDescribed {
                    name: group.name.clone(),
                    documentation: group.description.clone(),
                    slots: match group.slot_count() {
                        PinGroupSlotCount::FollowsItsWires => {
                            NodeTypePinGroupSlots::FollowTheirWires
                        }
                        PinGroupSlotCount::SetByItsInput => {
                            NodeTypePinGroupSlots::SetByTheInputOfTheGroupsName
                        }
                    },
                    inputs: group
                        .inputs
                        .iter()
                        .map(|port| an_input_port_described(descriptor, port))
                        .collect(),
                    outputs: group.outputs.iter().map(a_port_described).collect(),
                })
                .collect(),
        }
    }
}

/// `port` as an output, or as the port part of an input: typed where it declares a pin, else
/// untyped.
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
    }
}

/// `port`, an input of a node of the type `descriptor` describes, as an input: its mode, the
/// one it declares, else the one its pin type and the node's kind give it; whether a link may
/// end at it; and the JSON Schema of a value it takes, only where it takes one.
fn an_input_port_described(
    descriptor: &NodeDescriptor,
    port: &PortDescriptor,
) -> NodeTypeInputDescribed {
    let node_kind = descriptor.node_kind;
    let declared = port.declared_pin();
    let mode = port
        .input_mode
        .unwrap_or_else(|| the_mode_of_an_input_declaring_none(node_kind, port));
    let port_info = PortInfo {
        input_mode: Some(mode),
        ..PortInfo::from(port)
    };
    let takes_a_value = the_input_port_takes_a_value(Some(node_kind), &port_info);
    let (mode, read) = NodeTypeInputMode::of(mode);
    NodeTypeInputDescribed {
        port: a_port_described(port),
        mode: Some(mode),
        takes_a_wire: port_info.takes_a_wire_on_a_node_of(&descriptor.node_class_import_path),
        read,
        default_value: port.pin_default.clone(),
        lifetime: Some(port.lifetime),
        value_json_schema: declared
            .as_ref()
            .filter(|_| takes_a_value)
            .and_then(PinDeclaration::value_json_schema),
    }
}

/// The mode of `port`, an input of a node of `node_kind` that declares none: a setting read at
/// setup where its pin type never takes a wire; else a link alone where the engine sets it no
/// value, as on a type that holds none; else a link or a value.
fn the_mode_of_an_input_declaring_none(
    node_kind: NodeKind,
    port: &PortDescriptor,
) -> NodeInputMode {
    let port_info = PortInfo::from(port);
    if port_info
        .pin
        .as_ref()
        .is_some_and(|pin| !pin.pin_type.takes_a_wire())
    {
        return NodeInputMode::SettingOnly {
            read: SettingReadTime::AtSetup,
        };
    }
    if the_input_port_takes_a_value(Some(node_kind), &port_info) {
        NodeInputMode::WireOrValue
    } else {
        NodeInputMode::WireOnly
    }
}

/// A config field of a node type not yet typed, as an input that takes no wire, read when the
/// node is set up: the schema of a value it takes is its pin type's where its schema states one
/// exactly, else the field's own.
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
        },
        mode: Some(mode),
        takes_a_wire: false,
        read,
        default_value: setting.default_value,
        lifetime: Some(NodeValueLifetime::Kept),
        value_json_schema: declared
            .as_ref()
            .and_then(PinDeclaration::value_json_schema)
            .or(Some(setting.field_value_json_schema)),
    }
}
