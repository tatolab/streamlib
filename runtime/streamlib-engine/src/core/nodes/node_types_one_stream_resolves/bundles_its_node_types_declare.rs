// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The bundles a stream's node types declare, which a choice among them lists.

use super::NodeTypesOneStreamResolves;
use crate::core::descriptors::{DeclaredPinType, PinType};
use crate::core::graph::PortInfo;

impl NodeTypesOneStreamResolves {
    /// Every bundle a pin of a node type its project brings or its interpreter described
    /// declares, a List's element among them, each once, in the order of their names.
    pub fn the_bundles_its_node_types_declare(&self) -> Vec<PinType> {
        let mut bundles = Vec::new();
        for descriptor in self
            .descriptors_of_the_view_types_this_streams_project_brings()
            .into_iter()
            .chain(self.descriptors_described_in_this_streams_node_interpreter())
        {
            for declared in descriptor
                .inputs
                .iter()
                .chain(&descriptor.outputs)
                .filter_map(|port| port.pin_declaration.as_ref())
            {
                match &declared.pin_type {
                    DeclaredPinType::Is { pin_type } => {
                        gather_the_bundles_a_pin_type_holds(pin_type, &mut bundles);
                    }
                    DeclaredPinType::OneOf { pin_types } => {
                        for pin_type in pin_types {
                            gather_the_bundles_a_pin_type_holds(pin_type, &mut bundles);
                        }
                    }
                    DeclaredPinType::Any
                    | DeclaredPinType::SameAsInput { .. }
                    | DeclaredPinType::SetByInput { .. } => {}
                }
            }
        }
        bundles.sort_by_cached_key(|bundle| {
            let PinType::Bundle { name, .. } = bundle else {
                unreachable!("only a bundle is gathered");
            };
            (
                name.clone(),
                serde_json::to_string(bundle).unwrap_or_default(),
            )
        });
        bundles.dedup();
        bundles
    }

    /// List on `port`, when it is a choice among the bundles its stream declares, every bundle
    /// [`Self::the_bundles_its_node_types_declare`] finds, in place of any it listed.
    pub(crate) fn list_the_bundles_its_node_types_declare_on(&self, port: &mut PortInfo) {
        if let Some(declared) = port.declared_pin_type.as_mut()
            && let DeclaredPinType::Is { pin_type } = &mut declared.pin_type
        {
            self.list_the_bundles_its_node_types_declare_in(pin_type);
        }
        if let Some(pin) = port.pin.as_mut() {
            self.list_the_bundles_its_node_types_declare_in(&mut pin.pin_type);
        }
    }

    /// List in `pin_type`, when it is a choice among the bundles its stream declares, every
    /// bundle [`Self::the_bundles_its_node_types_declare`] finds, in place of any it listed.
    pub(crate) fn list_the_bundles_its_node_types_declare_in(&self, pin_type: &mut PinType) {
        if let PinType::PinTypeChoice {
            among,
            among_the_bundles_its_stream_declares: true,
        } = pin_type
        {
            *among = self.the_bundles_its_node_types_declare();
        }
    }
}

/// Push onto `bundles` `pin_type` when it is a bundle, and the element of a List of them.
fn gather_the_bundles_a_pin_type_holds(pin_type: &PinType, bundles: &mut Vec<PinType>) {
    match pin_type {
        PinType::Bundle { .. } => bundles.push(pin_type.clone()),
        PinType::ListOf { element_type } => {
            gather_the_bundles_a_pin_type_holds(element_type, bundles);
        }
        _ => {}
    }
}
