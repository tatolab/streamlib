// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A pin group: a slot of inputs and outputs a node type declares once, which repeats on each
//! node of the type. A group whose slot holds an input that takes a wire grows with its wires:
//! one empty slot after the last wired one. A group whose slot holds only outputs, or inputs
//! that are settings, has as many slots as the node's input named after the group says.

use serde::{Deserialize, Serialize};

use crate::descriptors::PortDescriptor;
use crate::pin_type::{
    DeclaredPinType, NodeInputMode, PinAnnotations, PinDeclaration, PinNumberRange, PinType,
};

/// The separator between a pin group's name, a slot's index and a pin's name in the name of a
/// slot's port, as `layers.0.layer`.
pub const PIN_GROUP_SLOT_PORT_NAME_SEPARATOR: char = '.';

/// The most slots a pin group holds on one node.
pub const PIN_GROUP_MOST_SLOTS: usize = 64;

/// A slot of inputs and outputs a node type declares once, which repeats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinGroupDescriptor {
    /// The group's name; each slot's port is named `<group>.<index>.<pin>`.
    pub name: String,
    /// What the group is for.
    #[serde(default)]
    pub description: String,
    /// The inputs each slot holds, each named as its pin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<PortDescriptor>,
    /// The outputs each slot holds, each named as its pin; one that follows an input's type
    /// names an input of its own slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PortDescriptor>,
}

/// How many slots a pin group has on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinGroupSlotCount {
    /// One empty slot after the last wired one, never stored.
    FollowsItsWires,
    /// The value of the node's input named after the group, which a person sets.
    SetByItsInput,
}

/// A port of one slot of a pin group: the group's name, the slot's index from 0, and the pin's
/// name in the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinGroupSlotPort<'name> {
    /// The group's name.
    pub group: &'name str,
    /// The slot's index, from 0.
    pub slot: usize,
    /// The pin's name in the slot.
    pub pin: &'name str,
}

impl<'name> PinGroupSlotPort<'name> {
    /// The name of this pin of the group's first slot, as `layers.0.layer`, which declares what
    /// every slot's does.
    pub fn named_in_the_first_slot(&self) -> String {
        format!(
            "{}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}0{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{}",
            self.group, self.pin
        )
    }

    /// The slot port `port_name` names, as `layers.0.layer`; `None` for any other name.
    pub fn named(port_name: &'name str) -> Option<Self> {
        let mut parts = port_name.splitn(3, PIN_GROUP_SLOT_PORT_NAME_SEPARATOR);
        let (group, slot, pin) = (parts.next()?, parts.next()?, parts.next()?);
        if group.is_empty() || pin.is_empty() || (slot.len() > 1 && slot.starts_with('0')) {
            return None;
        }
        Some(Self {
            group,
            slot: slot.parse().ok()?,
            pin,
        })
    }
}

impl PinGroupDescriptor {
    /// A group named `name` holding `inputs` and `outputs` in each slot.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        inputs: Vec<PortDescriptor>,
        outputs: Vec<PortDescriptor>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            inputs,
            outputs,
        }
    }

    /// How many slots the group has on a node: following its wires when a slot holds an input
    /// that is no setting, else set by the node's input named after the group.
    pub fn slot_count(&self) -> PinGroupSlotCount {
        if self.inputs.iter().any(|input| {
            !input
                .input_mode
                .is_some_and(NodeInputMode::is_handed_to_its_node)
        }) {
            PinGroupSlotCount::FollowsItsWires
        } else {
            PinGroupSlotCount::SetByItsInput
        }
    }

    /// The name of pin `pin` of slot `slot`, as `layers.0.layer`.
    pub fn slot_port_name(&self, slot: usize, pin: &str) -> String {
        format!(
            "{}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{slot}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{pin}",
            self.name
        )
    }

    /// Whether `port_name` names a pin of a slot of this group, of any index.
    pub fn holds_the_slot_port(&self, port_name: &str) -> bool {
        PinGroupSlotPort::named(port_name).is_some_and(|port| {
            port.group == self.name
                && self
                    .inputs
                    .iter()
                    .chain(&self.outputs)
                    .any(|declared| declared.name == port.pin)
        })
    }

    /// The inputs and the outputs of slot `slot`, each named `<group>.<slot>.<pin>`, a pin
    /// following the type of an input of its own slot following that slot's input.
    pub fn the_ports_of_slot(&self, slot: usize) -> (Vec<PortDescriptor>, Vec<PortDescriptor>) {
        let of_this_slot = |port: &PortDescriptor| {
            let mut port = port.clone();
            port.name = self.slot_port_name(slot, &port.name);
            if let Some(PinDeclaration {
                pin_type: DeclaredPinType::SameAsInput { input_port_name },
                ..
            }) = port.pin_declaration.as_mut()
                && self
                    .inputs
                    .iter()
                    .any(|input| input.name == *input_port_name)
            {
                *input_port_name = self.slot_port_name(slot, input_port_name);
            }
            port
        };
        (
            self.inputs.iter().map(of_this_slot).collect(),
            self.outputs.iter().map(of_this_slot).collect(),
        )
    }

    /// The input named after a group whose count a person sets: an Integer from 0 to
    /// [`PIN_GROUP_MOST_SLOTS`], a
    /// setting read at setup, holding 1 until it is set; `None` for a group that follows its
    /// wires.
    pub fn the_input_setting_its_slot_count(&self) -> Option<PortDescriptor> {
        if self.slot_count() != PinGroupSlotCount::SetByItsInput {
            return None;
        }
        let pin_type = PinType::Integer { exact_size: None };
        Some(
            PortDescriptor::new(
                self.name.clone(),
                format!("How many slots `{}` has", self.name),
                false,
                PinDeclaration {
                    annotations: PinAnnotations {
                        range: Some(PinNumberRange {
                            minimum: Some(0.0),
                            maximum: Some(PIN_GROUP_MOST_SLOTS as f64),
                        }),
                        ..PinAnnotations::default()
                    },
                    ..PinDeclaration::exactly(pin_type)
                },
            )
            .with_input_mode(NodeInputMode::HandedToItsNode)
            .with_pin_default(serde_json::json!(1)),
        )
    }

    /// Why the group is not one a node type may declare, naming it: no pin, a pin named twice
    /// or holding the separator, a pin following an input of one slot by its slot's name;
    /// `None` when it may.
    pub fn refusal_of_its_declaration(&self) -> Option<String> {
        let group = &self.name;
        if group.is_empty() || group.contains(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR) {
            return Some(format!(
                "pin group {group:?} is named with no character or with \
                 `{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}`, which separates a slot's index"
            ));
        }
        if self.inputs.is_empty() && self.outputs.is_empty() {
            return Some(format!("pin group `{group}` holds no pin in its slot"));
        }
        let mut names = std::collections::HashSet::new();
        for pin in self.inputs.iter().chain(&self.outputs) {
            if pin.name.contains(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR) {
                return Some(format!(
                    "pin `{}` of pin group `{group}` holds \
                     `{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}`, which separates a slot's index",
                    pin.name
                ));
            }
            let followed = match &pin.pin_declaration {
                Some(PinDeclaration {
                    pin_type: DeclaredPinType::SameAsInput { input_port_name },
                    ..
                }) => Some(input_port_name),
                _ => None,
            };
            if let Some(followed) = followed.filter(|followed| {
                PinGroupSlotPort::named(followed).is_some_and(|slot| slot.group == group)
            }) {
                return Some(format!(
                    "pin `{}` of pin group `{group}` follows `{followed}`, an input of one slot; \
                     a pin follows an input of its own slot by that input's name alone",
                    pin.name
                ));
            }
            if !names.insert(pin.name.as_str()) {
                return Some(format!(
                    "pin group `{group}` names pin `{}` twice in its slot",
                    pin.name
                ));
            }
        }
        None
    }
}
