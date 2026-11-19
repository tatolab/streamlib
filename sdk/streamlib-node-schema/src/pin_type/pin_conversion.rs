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
    /// The settings it picks for the input its output feeds, when what its output gives
    /// follows a setting; `None` when its output gives `output_pin_type` whatever it holds.
    pub output_picked_for_the_input: Option<PinConversionOutputPick>,
}

impl PinConversion {
    /// Whether a transaction's `connect` places a node of it, a built-in's, on a link it bridges.
    pub fn may_be_inserted(&self) -> bool {
        self.node_type.names_a_built_in_node()
    }

    /// The settings a node of it holds to feed an input of `input`, and the type its output then
    /// gives; `None` when no setting makes it give a type that input takes.
    pub fn output_picked_for(&self, input: &PinType) -> Option<PinConversionOutputPicked> {
        match self.output_picked_for_the_input {
            None => Some(PinConversionOutputPicked {
                input_values: serde_json::Map::new(),
                output_pin_type: self.output_pin_type.clone(),
            }),
            Some(PinConversionOutputPick { picked_for, .. }) => picked_for(input),
        }
    }

    /// The type a node of it gives on its output while it holds `held`, each setting by its
    /// input's name; `None` when its output gives `output_pin_type` whatever it holds, or `held`
    /// names no setting it takes.
    pub fn output_given_under(
        &self,
        held: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<PinType> {
        let PinConversionOutputPick { given_under, .. } = self.output_picked_for_the_input?;
        given_under(held)
    }
}

/// The two functions a conversion whose output follows a setting supplies.
#[derive(Clone, Copy)]
pub struct PinConversionOutputPick {
    /// For the type of the input its output feeds, the settings it then holds and the type its
    /// output then gives; `None` when none of its settings gives a type that input takes.
    pub picked_for: fn(&PinType) -> Option<PinConversionOutputPicked>,
    /// For the settings a node of it holds, each by its input's name, the type its output gives;
    /// `None` when they name no setting it takes.
    pub given_under: fn(&serde_json::Map<String, serde_json::Value>) -> Option<PinType>,
}

impl std::fmt::Debug for PinConversionOutputPick {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PinConversionOutputPick(..)")
    }
}

// `fn_addr_eq` compares addresses, so one function may compare unequal across codegen units.
impl PartialEq for PinConversionOutputPick {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.picked_for, other.picked_for)
            && std::ptr::fn_addr_eq(self.given_under, other.given_under)
    }
}

/// What a conversion picks to feed one input: the settings its node holds and the type its output
/// gives under them.
#[derive(Debug, Clone, PartialEq)]
pub struct PinConversionOutputPicked {
    /// Each setting picked, by its input's name, as the node's input values hold it.
    pub input_values: serde_json::Map<String, serde_json::Value>,
    /// The type the output gives under them.
    pub output_pin_type: PinType,
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
            output_picked_for_the_input: None,
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
