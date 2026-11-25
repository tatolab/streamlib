// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the `value` port of a `GraphInput`, a `GraphOutput` or a `Constant` carries, read from
//! its settings: the pin type `pin_type` chooses; and the one helper that writes it onto every
//! `value` port.

use streamlib_node_schema::the_pin_type_a_choice_holds;

use super::{GRAPH_INTERFACE_PIN_TYPE_SETTING, GRAPH_INTERFACE_VALUE_PORT_NAME};
use crate::core::descriptors::{DeclaredPinType, PinType, PortDescriptor, ResolvedPin};
use crate::core::graph::PortInfo;

/// The pin type the interface node holding `settings` chooses with its `pin_type` setting;
/// `None` while it holds none, so the first port linked to it decides it. Refused when it holds
/// no pin type, or one only a setting takes, which no link carries.
pub(crate) fn what_the_value_port_of_an_interface_node_carries(
    settings: Option<&serde_json::Value>,
) -> std::result::Result<Option<PinType>, String> {
    let Some(held) = settings
        .and_then(serde_json::Value::as_object)
        .and_then(|settings| settings.get(GRAPH_INTERFACE_PIN_TYPE_SETTING))
        .filter(|held| !held.is_null())
    else {
        return Ok(None);
    };
    the_pin_type_a_choice_holds(GRAPH_INTERFACE_PIN_TYPE_SETTING, held).map(Some)
}

/// What the `value` ports of the `Constant` holding `settings` carry: the pin type its `pin_type`
/// setting chooses, else the Float a Constant holds while it chooses none. Refused for a pin type
/// no link carries.
pub(crate) fn what_the_value_ports_of_a_constant_carry(
    settings: Option<&serde_json::Value>,
) -> std::result::Result<PinType, String> {
    Ok(what_the_value_port_of_an_interface_node_carries(settings)?
        .unwrap_or_else(crate::core::nodes::the_pin_type_a_constant_holds_by_default))
}

/// Whether the interface node holding `settings` states what its `value` port carries, a pin type
/// its `pin_type` chooses, so no link decides it.
pub(crate) fn an_interface_node_states_what_its_value_port_carries(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    settings
        .get(GRAPH_INTERFACE_PIN_TYPE_SETTING)
        .is_some_and(|held| !held.is_null())
}

/// A port of an interface node that the pin type its `value` carries is written onto: a port its
/// node type declares, or one of its node's resolved ports.
pub(crate) trait APortAnInterfaceNodeOffers {
    /// The port's name.
    fn its_name(&self) -> &str;
    /// Carry `pin_type`, in its type's default flow; while none is chosen, carry what its node
    /// type declares.
    fn carry(&mut self, pin_type: Option<&PinType>);
}

impl APortAnInterfaceNodeOffers for PortDescriptor {
    fn its_name(&self) -> &str {
        &self.name
    }

    fn carry(&mut self, pin_type: Option<&PinType>) {
        if let (Some(pin_type), Some(declaration)) = (pin_type, self.pin_declaration.as_mut()) {
            declaration.pin_type = DeclaredPinType::Is {
                pin_type: pin_type.clone(),
            };
            declaration.flow = pin_type.default_flow();
        }
    }
}

impl APortAnInterfaceNodeOffers for PortInfo {
    fn its_name(&self) -> &str {
        &self.name
    }

    fn carry(&mut self, pin_type: Option<&PinType>) {
        self.pin = pin_type.map(|pin_type| ResolvedPin {
            pin_type: pin_type.clone(),
            flow: pin_type.default_flow(),
        });
    }
}

/// Write `pin_type`, what [`what_the_value_port_of_an_interface_node_carries`] gives, onto every
/// `value` port among `ports`.
pub(crate) fn carry_on_every_value_port<'port, Port: APortAnInterfaceNodeOffers + 'port>(
    ports: impl IntoIterator<Item = &'port mut Port>,
    pin_type: Option<&PinType>,
) {
    for port in ports
        .into_iter()
        .filter(|port| port.its_name() == GRAPH_INTERFACE_VALUE_PORT_NAME)
    {
        port.carry(pin_type);
    }
}
