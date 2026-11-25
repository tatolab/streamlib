// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What one port of a graph's interface meets and renders: the ports linked to it judged by its
//! type, and the interface node rendered with its settings and its `value`.

use crate::core::descriptors::{
    NodeClassImportPath, PinAnnotations, PinLinkEnd, PinLinkInputEnd, PinType, ResolvedPin,
    pin_link_verdict,
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
    /// Refuse `linked_port` of `linked_node`, holding `linked_node_input_values`, unless it is a
    /// port of the right direction the link verdict lets meet a port of this one's type: a
    /// `GraphOutput`'s source must write it, and each reader of a `GraphInput` must take it.
    pub(super) fn refuse_a_linked_port_that_does_not_meet_it(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        linked_node: &GraphSnapshotNode,
        linked_port: &LinkPortRefOutput,
        linked_node_input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        refuse_a_port_the_node_does_not_have(
            node_types,
            &linked_port.node,
            linked_node,
            &linked_port.port,
            self.direction.into(),
        )?;
        let port_cast = cast_of(&linked_port.port)?;
        let Some((inputs, outputs)) = the_ports_of_a_graph_document_node(node_types, linked_node)
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
        let linked = linked.typed_by_its_setting_in(&inputs, linked_node_input_values);
        match self.refusal_of_a_linked_port_by_its_type(&linked, &linked_node.node_type, linked_port)
        {
            Some((carried, refusal)) => Err(Error::GraphError(format!(
                "`{}` node `{}` carries {carried} for `{}`, and the link rules refuse it: \
                 {refusal}",
                self.direction.interface_node_kind(),
                self.interface_node,
                self.port
            ))),
            None => Ok(()),
        }
    }

    /// The pin type this port carries: the one its `pin_type` setting chooses, else the one the
    /// first port linked to it carries; `None` while neither states one.
    pub fn the_pin_type_it_carries(&self) -> Option<PinType> {
        self.pin_type.clone()
    }

    /// The type this port carries, and why the link rules refuse `linked_port`, whose port is
    /// `linked` on a node of `linked_node_type`, against it; `None` when it carries none yet, the
    /// linked port states no one type exactly, or the link connects as it is.
    pub(super) fn refusal_of_a_linked_port_by_its_type<'port>(
        &'port self,
        linked: &PortInfo,
        linked_node_type: &NodeClassImportPath,
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
            meaning: linked.meaning(),
            flow: Some(linked_pin.flow),
        };
        let interface_end = PinLinkEnd {
            node_name: &self.interface_node,
            port_name: GRAPH_INTERFACE_VALUE_PORT_NAME,
            member_name: None,
            pin_type: chosen,
            meaning: None,
            flow: None,
        };
        let verdict = match self.direction {
            GraphInterfacePortDirection::Output => pin_link_verdict(
                linked_end,
                PinLinkInputEnd {
                    end: interface_end,
                    node_type: &NodeClassImportPath::graph_output(),
                    input_mode: None,
                },
                &[],
            ),
            GraphInterfacePortDirection::Input => pin_link_verdict(
                interface_end,
                PinLinkInputEnd {
                    end: linked_end,
                    node_type: linked_node_type,
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

    /// A port named `port_name` of the type this port carries, as `graph` renders a port.
    pub fn rendered_as_a_port_named(&self, port_name: &str) -> GraphNodePortOnTheWire {
        let pin = self.pin_type.as_ref().map(|pin_type| ResolvedPin {
            pin_type: pin_type.clone(),
            flow: pin_type.default_flow(),
        });
        GraphNodePortOnTheWire {
            name: port_name.to_string(),
            description: String::new(),
            port_kind: PortKindOnTheWire::default(),
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            stream_facts: None,
            values_checked: None,
            pin_default: None,
            pin,
            annotations: PinAnnotations::default(),
            takes_a_wire: None,
            value_read: None,
            pin_group_slot: None,
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
            pin_type: self.the_pin_type_it_carries(),
            current_value: None,
        }
    }
}
