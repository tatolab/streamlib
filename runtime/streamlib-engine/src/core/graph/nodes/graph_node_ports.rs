// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use super::PortInfo;
use crate::core::PortDirection;

/// Container for a node's input and output ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GraphNodePorts {
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

/// An input holding a value under a name it had: its current name and each name it holds a
/// value under, in the order they are read, the first being the one whose value it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHoldingAValueUnderANameItHad {
    pub current_name: String,
    pub names_in_the_order_read: Vec<String>,
}

impl GraphNodePorts {
    /// The ports of `direction`.
    pub fn on(&self, direction: PortDirection) -> &[PortInfo] {
        match direction {
            PortDirection::Input => &self.inputs,
            PortDirection::Output => &self.outputs,
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
            .map(|input| InputHoldingAValueUnderANameItHad {
                current_name: input.name.clone(),
                names_in_the_order_read: std::iter::once(&input.name)
                    .chain(&input.former_names)
                    .filter(|name| holds(name))
                    .cloned()
                    .collect(),
            })
            .collect()
    }

    /// `values`, each value by input name, with each input holding a value under a name it had
    /// holding, under its current name, the value
    /// [`Self::inputs_holding_a_value_under_a_name_they_had`] reads and no other; a key no input
    /// names is kept as it is.
    pub fn input_values_under_the_current_names(
        &self,
        mut values: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        for moved in
            self.inputs_holding_a_value_under_a_name_they_had(|name| values.contains_key(name))
        {
            let held: Vec<serde_json::Value> = moved
                .names_in_the_order_read
                .iter()
                .filter_map(|name| values.remove(name))
                .collect();
            if let Some(read) = held.into_iter().next() {
                values.insert(moved.current_name, read);
            }
        }
        values
    }
}
