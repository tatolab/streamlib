// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin type a port of a placed node carries, resolved against its graph from what its node
//! type declares: the type itself, one of several, any type, the type linked into another of its
//! inputs, or the type one of its settings chooses.

use super::{Graph, GraphEdgeWithComponents, GraphNode, PendingDeletionComponent, PortInfo};
use crate::core::PortDirection;
use crate::core::descriptors::{DeclaredPinType, PinType, ResolvedPin};

/// How far a port whose type follows an input is followed upstream before it counts as untyped;
/// a chain longer than this is a loop.
const MOST_INPUTS_FOLLOWED_UPSTREAM: usize = 64;

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
/// it: its one type and its flow; `None` while it is one of several, any or untyped.
pub(crate) fn the_pin_a_port_resolves_to(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    direction: PortDirection,
) -> Option<ResolvedPin> {
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
    the_pin_type_followed_upstream(graph, node, port, direction, 0)
}

fn the_pin_type_followed_upstream(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    direction: PortDirection,
    inputs_followed: usize,
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
            if inputs_followed >= MOST_INPUTS_FOLLOWED_UPSTREAM {
                return PinTypeInItsGraph::Untyped;
            }
            the_pin_type_linked_into(graph, node, input_port_name, inputs_followed + 1)
        }
        Some(DeclaredPinType::SetByInput { input_name }) => {
            the_pin_type_a_setting_chooses(node, input_name)
                .map_or(PinTypeInItsGraph::Untyped, PinTypeInItsGraph::Is)
        }
        Some(DeclaredPinType::Is { .. }) | None => the_pin_type_it_states(port),
    }
}

/// The type `port` states exactly, unless a config key or another input sets its family, which
/// keeps such a port, not yet typed, judged by its family.
fn the_pin_type_it_states(port: &PortInfo) -> PinTypeInItsGraph {
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
fn the_pin_type_linked_into(
    graph: &Graph,
    node: &GraphNode,
    input_port_name: &str,
    inputs_followed: usize,
) -> PinTypeInItsGraph {
    let Some(followed_input) = node.input_port(input_port_name) else {
        return PinTypeInItsGraph::Untyped;
    };
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

/// The pin type `node`'s setting `input_name` holds, as its saved value; `None` while it holds
/// none, or holds no pin type.
pub(crate) fn the_pin_type_a_setting_chooses(
    node: &GraphNode,
    input_name: &str,
) -> Option<PinType> {
    let setting = node.input_port(input_name)?;
    serde_json::from_value(node.pins.get(&setting.name)?.clone()).ok()
}
