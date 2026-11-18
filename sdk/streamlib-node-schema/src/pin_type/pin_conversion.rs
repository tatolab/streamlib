// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::PinType;
use crate::NodeClassImportPath;
use crate::descriptors::{NodeDescriptor, PortDescriptor};

/// A node type's statement that it converts its one wired input's type into its output's.
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
    /// Whether the verdict may answer a convert node of it, a built-in's; `connect` refuses it yet.
    pub fn may_be_inserted(&self) -> bool {
        self.node_type.names_a_built_in_node()
    }
}

impl NodeDescriptor {
    /// The conversion this node type states, refused on any shape but the one a conversion has.
    pub fn pin_conversion(&self) -> Result<Option<PinConversion>, PinConversionRefusal> {
        if !self.conversion {
            return Ok(None);
        }
        let node_type = &self.node_class_import_path;
        let refused = |why: String| {
            Err(PinConversionRefusal {
                node_type: node_type.clone(),
                why,
            })
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
        for (direction, port, pin_type) in [
            ("input", *wired_input, &input_pin_type),
            ("output", output, &output_pin_type),
        ] {
            if !pin_type.takes_a_wire() {
                return refused(format!(
                    "its {direction} `{}` is a {pin_type}, which takes no wire",
                    port.name
                ));
            }
        }
        Ok(Some(PinConversion {
            node_type: node_type.clone(),
            input_port_name: wired_input.name.clone(),
            input_pin_type,
            output_port_name: output.name.clone(),
            output_pin_type,
        }))
    }
}

/// Why a node type's statement that it is a conversion is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinConversionRefusal {
    /// The node type.
    pub node_type: NodeClassImportPath,
    /// What its shape has that a conversion's does not.
    pub why: String,
}

impl std::fmt::Display for PinConversionRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "`{}` states that it is a conversion, and {}; a conversion has exactly one input \
             that takes a wire, exactly one output and any number of setting inputs, each wired \
             pin stating one pin type",
            self.node_type, self.why
        )
    }
}

impl std::error::Error for PinConversionRefusal {}
