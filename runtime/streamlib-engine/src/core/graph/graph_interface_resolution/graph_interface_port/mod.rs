// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What one port of a graph's interface meets and renders: the ports linked to it judged by its
//! chosen type or family, and the interface node rendered with its settings and its `value`.

use crate::core::descriptors::{
    PinLinkEnd, PinLinkInputEnd, PinType, PortFamilyDeclaration, PortFamilyLinkEndpoint,
    ResolvedPin, pin_link_verdict, refusal_of_a_link_between_port_families,
};
use crate::core::graph::PortInfo;
use crate::core::graph::graph_snapshot::{
    GraphSnapshotNode, refuse_a_port_the_node_does_not_have, the_ports_of_a_graph_document_node,
};
use crate::core::graph::pin_type_resolution::states_one_type_exactly;
use crate::core::json_schema::{GraphNodePortOnTheWire, LinkPortRefOutput, PortKindOnTheWire};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::{Error, Result};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphInterfacePortDirectionOnTheWire, GraphInterfacePortOnTheWire,
};

use super::{
    GRAPH_INTERFACE_VALUE_PORT_NAME, GraphInterfacePort, GraphInterfacePortDirection, cast_of,
    cast_or_as_written,
};

impl GraphInterfacePort {
    /// Refuse `linked_port` of `linked_node`, configured as `linked_node_config`,
    /// unless it is a port of the right direction the link rules let meet a
    /// port of this one's family: a `GraphOutput`'s source must write it, and
    /// each reader of a `GraphInput` must take it.
    pub(super) fn refuse_a_linked_port_that_does_not_meet_it(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        linked_node: &GraphSnapshotNode,
        linked_port: &LinkPortRefOutput,
        linked_node_config: &serde_json::Value,
    ) -> Result<()> {
        refuse_a_port_the_node_does_not_have(
            node_types,
            &linked_port.node,
            linked_node,
            &linked_port.port,
            self.direction.into(),
        )?;
        let port_cast = cast_of(&linked_port.port)?;
        let Some((inputs, outputs)) =
            the_ports_of_a_graph_document_node(node_types, &linked_port.node, linked_node)?
        else {
            return Ok(());
        };
        let Some(linked) = self
            .direction
            .linked_side(&inputs, &outputs)
            .iter()
            .find(|port| port.name == port_cast)
        else {
            return Ok(());
        };
        if let Some((chosen, refusal)) =
            self.refusal_of_a_linked_port_by_the_chosen_type(linked, linked_port)
        {
            return Err(Error::GraphError(format!(
                "`{}` node `{}` chooses {chosen} for `{}`, and the link rules refuse it: \
                 {refusal}",
                self.direction.interface_node_kind(),
                self.interface_node,
                self.port
            )));
        }
        let linked_declaration = match self.direction {
            GraphInterfacePortDirection::Output => linked.family.clone(),
            GraphInterfacePortDirection::Input => {
                the_input_without_what_only_the_graphs_producer_can_meet(linked.family.clone())
            }
        };
        let Some(family) = self.family else {
            return Ok(());
        };
        let declared_family = PortFamilyDeclaration::of_family(family);
        let interface_end = PortFamilyLinkEndpoint {
            node_name: &self.interface_node,
            port_name: GRAPH_INTERFACE_VALUE_PORT_NAME,
            declaration: &declared_family,
        };
        let linked_end = PortFamilyLinkEndpoint {
            node_name: &linked_port.node,
            port_name: &linked_port.port,
            declaration: &linked_declaration,
        };
        let refusal = match self.direction {
            GraphInterfacePortDirection::Output => refusal_of_a_link_between_port_families(
                linked_end,
                Some(linked_node_config),
                interface_end,
            ),
            GraphInterfacePortDirection::Input => {
                refusal_of_a_link_between_port_families(interface_end, None, linked_end)
            }
        };
        match refusal {
            Some(refusal) => Err(Error::GraphError(format!(
                "`{}` node `{}` declares `{}` as {}, and the link rules refuse it: {refusal}",
                self.direction.interface_node_kind(),
                self.interface_node,
                self.port,
                family
            ))),
            None => Ok(()),
        }
    }

    /// The pin type this port carries: the one its `pin_type` setting chooses, else the one its
    /// family states exactly, as an Image port is a Texture; `None` for any other port.
    pub fn the_pin_type_it_carries(&self) -> Option<PinType> {
        self.pin_type.clone().or_else(|| {
            self.family
                .map(PortFamilyDeclaration::of_family)
                .and_then(|family| family.exact_pin_type())
        })
    }

    /// The type this port chooses, and why the link rules refuse `linked_port`, whose port is
    /// `linked`, against it; `None` when it chooses none, the linked port states no one type
    /// exactly, or the link connects as it is.
    pub(super) fn refusal_of_a_linked_port_by_the_chosen_type<'port>(
        &'port self,
        linked: &PortInfo,
        linked_port: &LinkPortRefOutput,
    ) -> Option<(&'port PinType, String)> {
        let chosen = self.pin_type.as_ref()?;
        if !states_one_type_exactly(linked) {
            return None;
        }
        let linked_pin = linked.pin.as_ref()?;
        let linked_end = PinLinkEnd {
            node_name: &linked_port.node,
            port_name: &linked_port.port,
            member_name: linked_port.member.as_deref(),
            pin_type: &linked_pin.pin_type,
            meaning: linked.meaning.as_deref(),
        };
        let interface_end = PinLinkEnd {
            node_name: &self.interface_node,
            port_name: GRAPH_INTERFACE_VALUE_PORT_NAME,
            member_name: None,
            pin_type: chosen,
            meaning: None,
        };
        let verdict = match self.direction {
            GraphInterfacePortDirection::Output => pin_link_verdict(
                linked_end,
                PinLinkInputEnd {
                    end: interface_end,
                    input_mode: None,
                },
                &[],
            ),
            GraphInterfacePortDirection::Input => pin_link_verdict(
                interface_end,
                PinLinkInputEnd {
                    end: linked_end,
                    input_mode: linked.input_mode,
                },
                &[],
            ),
        };
        verdict
            .refusal_of_the_link_made_as_it_is()
            .map(|refusal| (chosen, refusal.to_string()))
    }

    /// Why `output` is not this port's own output, the `value` of a
    /// `GraphInput` and the one output an interface node has; `None` when it is.
    pub fn refusal_of_an_output_it_does_not_have(
        &self,
        output: &LinkPortRefOutput,
    ) -> Option<String> {
        let is_the_value_output = self.direction == GraphInterfacePortDirection::Input
            && cast_of(&output.port).is_ok_and(|port| port == GRAPH_INTERFACE_VALUE_PORT_NAME);
        (!is_the_value_output).then(|| {
            format!(
                "`{}/{}` is no output of `{}`, and an interface node's one output is a \
                 `GraphInput`'s `{GRAPH_INTERFACE_VALUE_PORT_NAME}`",
                output.node, output.port, self.interface_node
            )
        })
    }

    /// A port named `port_name` of the family this port declares, as `graph` renders a port:
    /// typed as a node's port of that family is.
    pub fn rendered_as_a_port_named(&self, port_name: &str) -> GraphNodePortOnTheWire {
        let family = self.family.map(PortFamilyDeclaration::of_family);
        let pin = match &self.pin_type {
            Some(pin_type) => Some(ResolvedPin {
                pin_type: pin_type.clone(),
                flow: pin_type.default_flow(),
            }),
            None => family.clone().and_then(|family| {
                PortInfo::declaring_only_a_family(port_name, String::new(), family).pin
            }),
        };
        GraphNodePortOnTheWire {
            name: port_name.to_string(),
            description: String::new(),
            port_kind: PortKindOnTheWire::default(),
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            stream_facts: None,
            values_checked: None,
            family,
            pin_default: None,
            pin,
            value_read: None,
        }
    }

    /// The port as `graph` renders it under `graph_interface` on its node and each link with an
    /// end on it.
    pub(super) fn as_component(&self) -> GraphInterfacePortOnTheWire {
        GraphInterfacePortOnTheWire {
            direction: match self.direction {
                GraphInterfacePortDirection::Input => GraphInterfacePortDirectionOnTheWire::Input,
                GraphInterfacePortDirection::Output => GraphInterfacePortDirectionOnTheWire::Output,
            },
            port: cast_or_as_written(&self.port),
            family: self.family.map(|family| family.wire_name().to_string()),
            pin_type: self.the_pin_type_it_carries(),
            current_value: None,
        }
    }
}

/// `input_declaration` with no required names or codec: a `GraphInput` states
/// only a family, and the producer whose names and codec would meet them is
/// outside the graph.
fn the_input_without_what_only_the_graphs_producer_can_meet(
    input_declaration: PortFamilyDeclaration,
) -> PortFamilyDeclaration {
    PortFamilyDeclaration {
        requires: Vec::new(),
        requires_codec: Vec::new(),
        ..input_declaration
    }
}
