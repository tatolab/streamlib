// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use super::PortInfo;

/// Container for a node's input and output ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GraphNodePorts {
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

impl GraphNodePorts {
    /// The input `name` finds: the input so named, else the one that had that name.
    pub fn input_named(&self, name: &str) -> Option<&PortInfo> {
        PortInfo::the_one_a_name_finds(&self.inputs, name)
    }

    /// The output `name` finds: the output so named, else the one that had that name.
    pub fn output_named(&self, name: &str) -> Option<&PortInfo> {
        PortInfo::the_one_a_name_finds(&self.outputs, name)
    }

    /// `values`, each value by input name, with each one held under a name an input had moved
    /// to the input's name, as [`PortInfo::input_values_under_the_current_names`] moves them.
    pub fn input_values_under_the_current_names(
        &self,
        values: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        PortInfo::input_values_under_the_current_names(&self.inputs, values)
    }
}
