// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the `value` port of a `GraphInput` or a `GraphOutput` carries, read from its settings:
//! the pin type `pin_type` chooses and the family that type travels on, else the family `family`
//! names; and the one helper that writes both onto every `value` port.

use streamlib_node_schema::{
    output_port_family_resolved_against_config, the_pin_type_a_choice_holds,
};

use super::{
    GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    GRAPH_INTERFACE_VALUE_PORT_NAME,
};
use crate::core::descriptors::{
    DeclaredPinType, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration, ResolvedPin,
};
use crate::core::graph::PortInfo;

/// The pin type the interface node holding `input_values` chooses with its `pin_type` setting;
/// `None` while it holds none. Refused when it holds no pin type, or one only a setting takes,
/// which no link carries.
pub(crate) fn the_pin_type_an_interface_node_chooses(
    input_values: Option<&serde_json::Map<String, serde_json::Value>>,
) -> std::result::Result<Option<PinType>, String> {
    let Some(held) = input_values
        .and_then(|input_values| input_values.get(GRAPH_INTERFACE_PIN_TYPE_SETTING))
        .filter(|held| !held.is_null())
    else {
        return Ok(None);
    };
    the_pin_type_a_choice_holds(GRAPH_INTERFACE_PIN_TYPE_SETTING, held).map(Some)
}

/// The family a port of the chosen `pin_type` travels on, as
/// [`PortFamily::a_chosen_pin_type_travels_on`] finds it; refused for a type no family's message
/// carries, such as a List.
pub(crate) fn the_family_a_chosen_pin_type_travels_on(
    pin_type: &PinType,
) -> std::result::Result<PortFamily, String> {
    PortFamily::a_chosen_pin_type_travels_on(pin_type).ok_or_else(|| {
        format!(
            "its `{GRAPH_INTERFACE_PIN_TYPE_SETTING}` chooses {pin_type}, which no stream's port \
             carries yet. To fix: choose a type a single message carries, such as a Float, a \
             String or a Texture"
        )
    })
}

/// What the `value` port of the interface node holding `settings` carries: the pin type its
/// `pin_type` setting chooses and the family that type travels on, else no pin type and the
/// family its `family` setting names, else Records. Refused for a pin type no link carries or no
/// family's message carries, and for a `family` naming no family.
pub(crate) fn what_the_value_port_of_an_interface_node_carries(
    settings: Option<&serde_json::Value>,
) -> std::result::Result<(Option<PinType>, PortFamily), String> {
    let chosen =
        the_pin_type_an_interface_node_chooses(settings.and_then(serde_json::Value::as_object))?;
    let family = match &chosen {
        Some(pin_type) => the_family_a_chosen_pin_type_travels_on(pin_type)?,
        None => output_port_family_resolved_against_config(
            &crate::core::nodes::the_family_a_graph_interface_value_port_declares(),
            settings,
        )?,
    };
    Ok((chosen, family))
}

/// Whether the interface node holding `settings` states what its `value` port carries, a pin type
/// its `pin_type` chooses or a family its `family` names, so no link decides it.
pub(crate) fn an_interface_node_states_what_its_value_port_carries(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    [
        GRAPH_INTERFACE_PIN_TYPE_SETTING,
        GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
    ]
    .iter()
    .any(|setting| settings.get(*setting).is_some_and(|held| !held.is_null()))
}

/// A port of an interface node that the family and pin type its `value` carries are written onto:
/// a port its node type declares, or one of its node's resolved ports.
pub(crate) trait APortAnInterfaceNodeOffers {
    /// The port's name.
    fn its_name(&self) -> &str;
    /// Carry `family`, and `pin_type` when one is chosen, in its type's default flow.
    fn carry(&mut self, family: PortFamily, pin_type: Option<&PinType>);
}

impl APortAnInterfaceNodeOffers for PortDescriptor {
    fn its_name(&self) -> &str {
        &self.name
    }

    fn carry(&mut self, family: PortFamily, pin_type: Option<&PinType>) {
        self.family = PortFamilyDeclaration::of_family(family);
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

    fn carry(&mut self, family: PortFamily, pin_type: Option<&PinType>) {
        self.family = PortFamilyDeclaration::of_family(family);
        self.pin = pin_type.map(|pin_type| ResolvedPin {
            pin_type: pin_type.clone(),
            flow: pin_type.default_flow(),
        });
    }
}

/// Write `family` and `pin_type`, what [`what_the_value_port_of_an_interface_node_carries`]
/// gives, onto every `value` port among `ports`.
pub(crate) fn carry_on_every_value_port<'port, Port: APortAnInterfaceNodeOffers + 'port>(
    ports: impl IntoIterator<Item = &'port mut Port>,
    family: PortFamily,
    pin_type: Option<&PinType>,
) {
    for port in ports
        .into_iter()
        .filter(|port| port.its_name() == GRAPH_INTERFACE_VALUE_PORT_NAME)
    {
        port.carry(family, pin_type);
    }
}
