// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin type a port of a placed node carries, resolved against its graph from what its node
//! type declares: the type itself, one of several, any type, the type linked into another of its
//! inputs, or the type one of its settings chooses.

use std::collections::HashSet;

use super::input_values::the_pin_values_with_the_live_ones_over_them;
use super::{
    Graph, GraphEdgeWithComponents, GraphNode, NodeRunId, PendingDeletionComponent, PortInfo,
};
use crate::core::PortDirection;
use crate::core::descriptors::{DeclaredPinType, PinConversion, PinType, ResolvedPin};

/// What a port of a placed node carries, resolved against its graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PinTypeInItsGraph {
    /// Exactly this type.
    Is(PinType),
    /// Any one of these types: a link connects as the best of them does.
    OneOf(Vec<PinType>),
    /// Every type: an input that takes a link from any typed output.
    Any,
    /// No type its graph settles: a port its node type declares no pin for, an output that
    /// declares any type, one following an input nothing typed is linked into, or one whose type
    /// a setting chooses while the setting holds none. Its family judges its links.
    Untyped,
}

/// The pin `port`, a port of `node` at `direction`, resolves to in `graph`, as `graph` serves
/// it: the pin it declares exactly, a choice among the bundles its stream declares listing
/// them, else its one type and its flow; `None` while it is one of several, any or untyped.
pub(crate) fn the_pin_a_port_resolves_to(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    direction: PortDirection,
) -> Option<ResolvedPin> {
    if declares_its_type_exactly(port) {
        return port.pin.clone().map(|mut pin| {
            graph
                .node_types_this_stream_resolves()
                .list_the_bundles_its_node_types_declare_in(&mut pin.pin_type);
            pin
        });
    }
    let PinTypeInItsGraph::Is(pin_type) =
        the_pin_type_of_a_port_in_its_graph(graph, node, port, direction)
    else {
        return None;
    };
    let flow = port
        .declared_pin_type
        .as_ref()
        .map(|declared| declared.flow)
        .or_else(|| port.pin.as_ref().map(|pin| pin.flow))
        .unwrap_or_else(|| pin_type.default_flow());
    Some(ResolvedPin { pin_type, flow })
}

/// What `port`, a port of `node` at `direction`, carries in `graph`.
pub(crate) fn the_pin_type_of_a_port_in_its_graph(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    direction: PortDirection,
) -> PinTypeInItsGraph {
    the_pin_type_followed_upstream(graph, node, port, direction, &mut HashSet::new())
}

/// Whether `port` declares its type exactly, or declares none: what it carries is its own pin,
/// whatever it is linked to.
pub(crate) fn declares_its_type_exactly(port: &PortInfo) -> bool {
    matches!(
        port.declared_pin_type
            .as_ref()
            .map(|declared| &declared.pin_type),
        Some(DeclaredPinType::Is { .. }) | None
    )
}

/// Whether `port` carries exactly the one type it declares: it declares its type exactly, and
/// neither a config key nor another input sets its family.
pub(crate) fn states_one_type_exactly(port: &PortInfo) -> bool {
    declares_its_type_exactly(port)
        && matches!(the_pin_type_it_states(port), PinTypeInItsGraph::Is(_))
}

/// What `port` carries, each input of a node a port following one is followed through kept in
/// `inputs_followed`: an input followed twice closes a loop, and a port on it is untyped.
fn the_pin_type_followed_upstream<'graph>(
    graph: &'graph Graph,
    node: &'graph GraphNode,
    port: &PortInfo,
    direction: PortDirection,
    inputs_followed: &mut HashSet<(&'graph NodeRunId, &'graph str)>,
) -> PinTypeInItsGraph {
    match port
        .declared_pin_type
        .as_ref()
        .map(|declared| &declared.pin_type)
    {
        Some(DeclaredPinType::OneOf { pin_types }) => PinTypeInItsGraph::OneOf(pin_types.clone()),
        Some(DeclaredPinType::Any) => match direction {
            PortDirection::Input => PinTypeInItsGraph::Any,
            PortDirection::Output => PinTypeInItsGraph::Untyped,
        },
        Some(DeclaredPinType::SameAsInput { input_port_name }) => {
            the_pin_type_linked_into(graph, node, input_port_name, inputs_followed)
        }
        Some(DeclaredPinType::SetByInput { input_name }) => {
            the_pin_type_a_setting_chooses(node, input_name)
                .map_or(PinTypeInItsGraph::Untyped, PinTypeInItsGraph::Is)
        }
        Some(DeclaredPinType::Is { .. }) | None => match direction {
            PortDirection::Output => the_type_a_convert_node_gives_under(graph, node, port, None)
                .map_or_else(|| the_pin_type_it_states(port), PinTypeInItsGraph::Is),
            PortDirection::Input => the_pin_type_it_states(port),
        },
    }
}

/// The conversion `output_port` of `node` is the output of when `node` is a node of a
/// conversion whose output follows a setting, among those `graph`'s stream resolves.
pub(crate) fn the_conversion_whose_output_follows_a_setting(
    graph: &Graph,
    node: &GraphNode,
    output_port: &PortInfo,
) -> Option<PinConversion> {
    graph
        .node_types_this_stream_resolves()
        .pin_conversions()
        .iter()
        .find(|conversion| {
            conversion.node_type == node.node_type
                && conversion.output_port_name == output_port.name
                && conversion.output_picked_for_the_input.is_some()
        })
        .cloned()
}

/// The type `output_port` of `node` gives while `node` holds `config` with its pin values over
/// it, when `node` is a node of a conversion whose output follows a setting; `None` for any
/// other output, and for settings that give no type, which leave it carrying the type it states.
pub(crate) fn the_type_a_convert_node_gives_under(
    graph: &Graph,
    node: &GraphNode,
    output_port: &PortInfo,
    input_values: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<PinType> {
    let conversion = the_conversion_whose_output_follows_a_setting(graph, node, output_port)?;
    let mut held = input_values.cloned().unwrap_or_default();
    held.extend(the_pin_values_with_the_live_ones_over_them(graph, node));
    conversion.output_given_under(&held)
}

/// The type `port` states exactly, unless a config key or another input sets its family, which
/// keeps such a port, not yet typed, judged by its family.
pub(crate) fn the_pin_type_it_states(port: &PortInfo) -> PinTypeInItsGraph {
    let family = &port.family;
    let family_set_another_way = family.family_from_config_key.is_some()
        || family.codec_from_config_key.is_some()
        || family.family_follows_input_port.is_some();
    match &port.pin {
        Some(pin) if !family_set_another_way => PinTypeInItsGraph::Is(pin.pin_type.clone()),
        _ => PinTypeInItsGraph::Untyped,
    }
}

/// What a port following input `input_port_name` of `node` carries: what the output linked
/// into that input carries, else the input's own type while nothing is linked into it.
fn the_pin_type_linked_into<'graph>(
    graph: &'graph Graph,
    node: &'graph GraphNode,
    input_port_name: &str,
    inputs_followed: &mut HashSet<(&'graph NodeRunId, &'graph str)>,
) -> PinTypeInItsGraph {
    let Some(followed_input) = node.input_port(input_port_name) else {
        return PinTypeInItsGraph::Untyped;
    };
    if !inputs_followed.insert((&node.id, followed_input.name.as_str())) {
        return PinTypeInItsGraph::Untyped;
    }
    let link_into_it = graph.traversal().e(()).iter().find(|link| {
        link.to_port().node_run_id() == &node.id
            && link.to_port().port_name() == followed_input.name
            && !link.has::<PendingDeletionComponent>()
    });
    let Some(link_into_it) = link_into_it else {
        return match the_pin_type_followed_upstream(
            graph,
            node,
            followed_input,
            PortDirection::Input,
            inputs_followed,
        ) {
            exactly @ PinTypeInItsGraph::Is(_) => exactly,
            PinTypeInItsGraph::OneOf(_) | PinTypeInItsGraph::Any | PinTypeInItsGraph::Untyped => {
                PinTypeInItsGraph::Untyped
            }
        };
    };
    let Some(upstream_node) = graph
        .traversal()
        .v(link_into_it.source().node_run_id())
        .first()
    else {
        return PinTypeInItsGraph::Untyped;
    };
    let Some(upstream_output) = upstream_node.output_port(link_into_it.source().port_name()) else {
        return PinTypeInItsGraph::Untyped;
    };
    the_pin_type_followed_upstream(
        graph,
        upstream_node,
        upstream_output,
        PortDirection::Output,
        inputs_followed,
    )
}

/// The pin type `node`'s setting `input_name` holds: its saved value, else the default it
/// declares; `None` while it holds neither, or holds no pin type.
pub(crate) fn the_pin_type_a_setting_chooses(
    node: &GraphNode,
    input_name: &str,
) -> Option<PinType> {
    let setting = node.input_port(input_name)?;
    let held = node
        .input_values
        .get(&setting.name)
        .or(setting.pin_default.as_ref())?;
    serde_json::from_value(held.clone()).ok()
}
