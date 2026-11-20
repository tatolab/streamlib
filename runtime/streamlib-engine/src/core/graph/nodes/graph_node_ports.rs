// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use super::PortInfo;
use crate::core::PortDirection;
use crate::core::descriptors::PinType;

/// Container for a node's input and output ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GraphNodePorts {
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

/// An input holding a value under a name it had: its current name, the name whose value it
/// reads, and each other name it holds a value under, in the order they are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHoldingAValueUnderANameItHad {
    pub current_name: String,
    pub name_read: String,
    pub other_names_held: Vec<String>,
}

impl InputHoldingAValueUnderANameItHad {
    /// Each name the input holds a value under, in the order they are read: the one read first.
    pub fn names_held(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.name_read).chain(&self.other_names_held)
    }
}

impl GraphNodePorts {
    /// The ports of a node of the type `descriptor` declares just placed: the ones it declares,
    /// then, with one slot in each of its pin groups, each slot's.
    pub fn of_a_node_just_placed(descriptor: &crate::core::NodeDescriptor) -> Self {
        let one_each = vec![1; descriptor.pin_groups.len()];
        Self {
            inputs: descriptor
                .the_inputs_with_slots(&one_each)
                .iter()
                .map(PortInfo::from)
                .collect(),
            outputs: descriptor
                .the_outputs_with_slots(&one_each)
                .iter()
                .map(PortInfo::from)
                .collect(),
        }
    }

    /// These ports, of a node of `node_type` whose config is `config`, with what that config
    /// declares of them resolved: a `GraphInput`'s `value` input takes the family, the range and
    /// the lifetime its config names. The ports of every other node type are as it declares
    /// them.
    pub fn resolved_against_the_config(
        mut self,
        node_type: &crate::core::descriptors::NodeClassImportPath,
        config: Option<&serde_json::Value>,
    ) -> Self {
        if crate::core::graph::graph_interface_resolution::is_the_graph_input_type(node_type) {
            crate::core::graph::graph_interface_resolution::resolve_the_value_input_a_graph_input_config_declares(
                &mut self, config,
            );
        }
        self
    }

    /// The ports of `direction`.
    pub fn on(&self, direction: PortDirection) -> &[PortInfo] {
        match direction {
            PortDirection::Input => &self.inputs,
            PortDirection::Output => &self.outputs,
        }
    }

    /// The ports of `direction`, to change.
    pub fn on_mut(&mut self, direction: PortDirection) -> &mut Vec<PortInfo> {
        match direction {
            PortDirection::Input => &mut self.inputs,
            PortDirection::Output => &mut self.outputs,
        }
    }

    /// The port of `direction` that `name` finds: the port so named, else the one that lists
    /// `name` as a name it had. Every lookup of a port by a name a caller, a document or an older
    /// edit gives goes through here, so a renamed port is found under each name it had.
    pub fn the_one_a_name_finds(&self, direction: PortDirection, name: &str) -> Option<&PortInfo> {
        let ports = self.on(direction);
        ports.iter().find(|port| port.name == name).or_else(|| {
            ports
                .iter()
                .find(|port| port.former_names.iter().any(|former| former == name))
        })
    }

    /// The current name of the port of `direction` that `name` finds.
    pub fn current_name(&self, direction: PortDirection, name: &str) -> Option<&str> {
        self.the_one_a_name_finds(direction, name)
            .map(|port| port.name.as_str())
    }

    /// The input `name` finds: the input so named, else the one that had that name.
    pub fn input_named(&self, name: &str) -> Option<&PortInfo> {
        self.the_one_a_name_finds(PortDirection::Input, name)
    }

    /// The port of `direction` `name` finds among `ports`, the ports of a node type: the port
    /// so named, else, for a port of a slot of a pin group past the first and within
    /// [`crate::core::descriptors::PIN_GROUP_MOST_SLOTS`], the same pin of the first slot, which
    /// a type's ports hold and which declares what every slot's does.
    pub fn the_port_of_a_type_a_name_finds<'ports>(
        ports: &'ports [PortInfo],
        name: &str,
    ) -> Option<&'ports PortInfo> {
        ports.iter().find(|port| port.name == name).or_else(|| {
            let slot_port = crate::core::descriptors::PinGroupSlotPort::named(name)
                .filter(|port| port.slot < crate::core::descriptors::PIN_GROUP_MOST_SLOTS)?;
            let in_the_first_slot = slot_port.named_in_the_first_slot();
            ports.iter().find(|port| port.name == in_the_first_slot)
        })
    }

    /// The input `name` finds among the ports of a node type: the input so named or that had
    /// that name, else the same pin of a pin group's first slot, as
    /// [`Self::the_port_of_a_type_a_name_finds`] finds it.
    pub fn the_input_of_the_type_a_name_finds(&self, name: &str) -> Option<&PortInfo> {
        self.input_named(name)
            .or_else(|| Self::the_port_of_a_type_a_name_finds(&self.inputs, name))
    }

    /// The output `name` finds: the output so named, else the one that had that name.
    pub fn output_named(&self, name: &str) -> Option<&PortInfo> {
        self.the_one_a_name_finds(PortDirection::Output, name)
    }

    /// Each input that `holds` says holds a value under a name it had, with the names it holds a
    /// value under in the order they are read: its current name first, then the names it had in
    /// the order it declares them. The value read is the one under the first; this is the one
    /// order every move of a value to its input's current name follows.
    pub fn inputs_holding_a_value_under_a_name_they_had(
        &self,
        holds: impl Fn(&str) -> bool,
    ) -> Vec<InputHoldingAValueUnderANameItHad> {
        self.inputs
            .iter()
            .filter(|input| input.former_names.iter().any(|former| holds(former)))
            .filter_map(|input| {
                let mut names_held = std::iter::once(&input.name)
                    .chain(&input.former_names)
                    .filter(|name| holds(name))
                    .cloned();
                Some(InputHoldingAValueUnderANameItHad {
                    current_name: input.name.clone(),
                    name_read: names_held.next()?,
                    other_names_held: names_held.collect(),
                })
            })
            .collect()
    }

    /// `values`, each value by input name, with each input holding a value under a name it had
    /// holding, under its current name, the value
    /// [`Self::inputs_holding_a_value_under_a_name_they_had`] reads and no other, and each value
    /// saved in a form its input's type held before held in the form it holds now; a key no
    /// input names is kept as it is.
    pub fn input_values_under_the_current_names(
        &self,
        mut values: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        for moved in
            self.inputs_holding_a_value_under_a_name_they_had(|name| values.contains_key(name))
        {
            let read = values.remove(&moved.name_read);
            for other in &moved.other_names_held {
                values.remove(other);
            }
            if let Some(read) = read {
                values.insert(moved.current_name, read);
            }
        }
        for input in &self.inputs {
            if let Some(value) = values.remove(&input.name)
                && let Some(value) = self.input_value_in_the_form_it_takes_now(&input.name, value)
            {
                values.insert(input.name.clone(), value);
            }
        }
        values
    }

    /// `value`, saved for the input named `input`, in the form its type takes now; `None` when
    /// the form it was saved in held no value. An Asset was saved by a config class as its bare
    /// UUID string, and as the empty string for no asset; it takes the object
    /// `{"asset": "<uuid>"}`. Every other value is as saved.
    pub fn input_value_in_the_form_it_takes_now(
        &self,
        input: &str,
        value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        let holds_an_asset = self.input_named(input).is_some_and(|input| {
            input
                .pin
                .as_ref()
                .is_some_and(|pin| matches!(pin.pin_type, PinType::Asset { .. }))
        });
        match value {
            serde_json::Value::String(uuid) if holds_an_asset && uuid.is_empty() => None,
            serde_json::Value::String(uuid) if holds_an_asset => {
                Some(serde_json::json!({ "asset": uuid }))
            }
            value => Some(value),
        }
    }
}
