// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The bundles a stream's node types declare, which a choice among them lists.

use std::collections::BTreeMap;

use super::NodeTypesOneStreamResolves;
use crate::core::compiler::compiler_ops::function_node_pool::is_the_function_node_pool_type;
use crate::core::descriptors::{DeclaredPinType, NodeDescriptor, PinType, PinTypesAChoiceTakes};
use crate::core::graph::PortInfo;

/// Where a bundle is offered among the others: by its name, then each member's name and pin
/// type in words, in order.
type OrderOfAnOfferedBundle = (Option<String>, Vec<(String, String)>);

impl NodeTypesOneStreamResolves {
    /// List, on every port of `ports` that chooses among the bundles its stream declares, each
    /// bundle [`Self::the_bundles_its_node_types_declare`] finds, in place of any it listed.
    pub(crate) fn list_the_bundles_its_node_types_declare_on(&self, ports: &mut [PortInfo]) {
        if !ports
            .iter()
            .any(chooses_among_the_bundles_its_stream_declares)
        {
            return;
        }
        let bundles = self.the_bundles_its_node_types_declare();
        for port in ports {
            for among in the_choices_among_the_bundles_its_stream_declares_on(port) {
                among.clone_from(&bundles);
            }
        }
    }

    /// List in `pin_type`, when it chooses among the bundles its stream declares, each bundle
    /// [`Self::the_bundles_its_node_types_declare`] finds, in place of any it listed.
    pub(crate) fn list_the_bundles_its_node_types_declare_in(&self, pin_type: &mut PinType) {
        if let Some(among) = the_choice_among_the_bundles_its_stream_declares_in(pin_type) {
            *among = self.the_bundles_its_node_types_declare();
        }
    }

    /// Every bundle a pin of a node type its project brings or its interpreter described
    /// declares, a List's element among them, each once by
    /// [`PinType::is_the_same_bundle_as`], in the order of their names.
    fn the_bundles_its_node_types_declare(&self) -> Vec<PinType> {
        let mut bundles_in_order = BTreeMap::<OrderOfAnOfferedBundle, Vec<PinType>>::new();
        let mut offer_each_bundle_of = |descriptor: &NodeDescriptor| {
            for (bundle, order) in descriptor
                .inputs
                .iter()
                .chain(&descriptor.outputs)
                .filter_map(|port| port.pin_declaration.as_ref())
                .flat_map(|declared| declared.pin_type.the_pin_types_it_names())
                .filter_map(the_bundle_a_pin_type_holds)
            {
                let offered = bundles_in_order.entry(order).or_default();
                if !offered
                    .iter()
                    .any(|held| held.is_the_same_bundle_as(bundle))
                {
                    offered.push(bundle.clone());
                }
            }
        };
        for view_type in self.view_types_this_streams_project_brings.read().values() {
            offer_each_bundle_of(&view_type.descriptor);
        }
        for described in self
            .described_in_this_streams_node_interpreter
            .read()
            .values()
        {
            if !is_the_function_node_pool_type(&described.descriptor.node_class_import_path) {
                offer_each_bundle_of(&described.descriptor);
            }
        }
        bundles_in_order.into_values().flatten().collect()
    }
}

/// Whether `port` declares, or resolves to, a choice among the bundles its stream declares.
pub(crate) fn chooses_among_the_bundles_its_stream_declares(port: &PortInfo) -> bool {
    let declares_one = matches!(
        port.declared_pin_type.as_ref().map(|declared| &declared.pin_type),
        Some(DeclaredPinType::Is { pin_type }) if is_a_choice_among_the_bundles_its_stream_declares(pin_type)
    );
    declares_one
        || port
            .pin
            .as_ref()
            .is_some_and(|pin| is_a_choice_among_the_bundles_its_stream_declares(&pin.pin_type))
}

fn is_a_choice_among_the_bundles_its_stream_declares(pin_type: &PinType) -> bool {
    matches!(
        pin_type.the_pin_types_a_choice_takes(),
        Some(PinTypesAChoiceTakes::OneOfTheBundlesItsStreamDeclares(_))
    )
}

/// The list of each choice among the bundles its stream declares that `port` declares or
/// resolves to.
fn the_choices_among_the_bundles_its_stream_declares_on(
    port: &mut PortInfo,
) -> impl Iterator<Item = &mut Vec<PinType>> {
    let declared =
        port.declared_pin_type
            .as_mut()
            .and_then(|declared| match &mut declared.pin_type {
                DeclaredPinType::Is { pin_type } => {
                    the_choice_among_the_bundles_its_stream_declares_in(pin_type)
                }
                _ => None,
            });
    let resolved = port
        .pin
        .as_mut()
        .and_then(|pin| the_choice_among_the_bundles_its_stream_declares_in(&mut pin.pin_type));
    declared.into_iter().chain(resolved)
}

/// The list of `pin_type` when it chooses among the bundles its stream declares.
fn the_choice_among_the_bundles_its_stream_declares_in(
    pin_type: &mut PinType,
) -> Option<&mut Vec<PinType>> {
    match pin_type {
        PinType::PinTypeChoice {
            among,
            among_the_bundles_its_stream_declares: true,
        } => Some(among),
        _ => None,
    }
}

/// `pin_type` when it is a bundle, or the bundle a List of them, at any depth, holds, with
/// where it is offered among the others.
fn the_bundle_a_pin_type_holds(pin_type: &PinType) -> Option<(&PinType, OrderOfAnOfferedBundle)> {
    match pin_type {
        PinType::Bundle { name, members } => Some((
            pin_type,
            (
                name.clone(),
                members
                    .iter()
                    .map(|member| (member.name.clone(), member.member_type.to_string()))
                    .collect(),
            ),
        )),
        PinType::ListOf { element_type } => the_bundle_a_pin_type_holds(element_type),
        _ => None,
    }
}
