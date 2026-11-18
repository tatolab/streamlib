// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::PinType;
use crate::NodeClassImportPath;
use crate::descriptors::{NodeDescriptor, PortDescriptor};

/// A node type that states it converts one pin type to another, as its own pins say: what its
/// one wired input takes and what its one output gives. The link verdict tests each side of it
/// with its own rules, so no table of conversions exists apart from the node types.
#[derive(Debug, Clone, PartialEq)]
pub struct PinConversion {
    /// The node type that converts.
    pub node_type: NodeClassImportPath,
    /// Its one input that takes a wire.
    pub input_port_name: String,
    /// The type that input takes.
    pub input_pin_type: PinType,
    /// Its one output.
    pub output_port_name: String,
    /// The type that output gives.
    pub output_pin_type: PinType,
}

impl PinConversion {
    /// Whether `connect` may place a node of it between two ends by itself: only a built-in
    /// conversion is placed; any other is named for a person or a client to place.
    pub fn may_be_inserted(&self) -> bool {
        self.node_type.names_a_built_in_node()
    }
}

impl NodeDescriptor {
    /// The conversion this node type states, `None` when it states none. Refuses, naming the
    /// node type, a conversion of any shape but exactly one input that takes a wire, exactly one
    /// output, and any number of setting inputs, and one whose wired input or output does not
    /// state exactly one pin type.
    pub fn pin_conversion(&self) -> Result<Option<PinConversion>, String> {
        if !self.conversion {
            return Ok(None);
        }
        let node_type = &self.node_class_import_path;
        let refused = |why: String| {
            Err(format!(
                "`{node_type}` states that it is a conversion, and {why}; a conversion has \
                 exactly one input that takes a wire, exactly one output and any number of \
                 setting inputs, each wired pin stating one pin type"
            ))
        };
        let wired_inputs: Vec<&PortDescriptor> = self
            .inputs
            .iter()
            .filter(|input| input.input_mode.is_none_or(|mode| mode.takes_a_wire()))
            .collect();
        let ([wired_input], [output]) = (wired_inputs.as_slice(), self.outputs.as_slice()) else {
            return refused(format!(
                "it has {} inputs that take a wire and {} outputs",
                wired_inputs.len(),
                self.outputs.len()
            ));
        };
        let exact_pin_type_of = |port: &PortDescriptor| {
            port.declared_pin()
                .and_then(|declared| declared.into_resolved())
                .map(|resolved| resolved.pin_type)
        };
        let Some(input_pin_type) = exact_pin_type_of(wired_input) else {
            return refused(format!(
                "its input `{}` states no one pin type",
                wired_input.name
            ));
        };
        let Some(output_pin_type) = exact_pin_type_of(output) else {
            return refused(format!(
                "its output `{}` states no one pin type",
                output.name
            ));
        };
        Ok(Some(PinConversion {
            node_type: node_type.clone(),
            input_port_name: wired_input.name.clone(),
            input_pin_type,
            output_port_name: output.name.clone(),
            output_pin_type,
        }))
    }
}
