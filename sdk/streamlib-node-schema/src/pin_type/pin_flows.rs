// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::{DeclaredPinType, PinDeclaration, PinFlow, PinType, built_in_pin_type};

impl PinFlow {
    /// Its name as a declaration and a client write it: `value`, `stream` or `trigger`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Stream => "stream",
            Self::Trigger => "trigger",
        }
    }
}

impl PinType {
    /// The flows a pin of this type may state, its default first. A List or a bundle is one
    /// message, so one holding a member that streams takes the flows of the member that allows
    /// least: a member that never travels as a value, else one that streams by default.
    pub fn flows(&self) -> &'static [PinFlow] {
        let own_flows = built_in_pin_type(self.built_in_name()).flows;
        let streaming_member_flows = match self {
            Self::ListOf { element_type } => vec![element_type.flows()],
            Self::Bundle { members } => members
                .iter()
                .map(|member| member.member_type.flows())
                .collect(),
            _ => return own_flows,
        }
        .into_iter()
        .filter(|member_flows| member_flows[0] != PinFlow::Value);
        streaming_member_flows
            .min_by_key(|member_flows| member_flows.contains(&PinFlow::Value))
            .unwrap_or(own_flows)
    }

    /// The flow a pin of this type has when its author states none.
    pub fn default_flow(&self) -> PinFlow {
        self.flows()[0]
    }

    /// The delivery profile a Stream input of this type reads by when its author states none:
    /// `newest` for a picture or a tensor, and for a List or a bundle whose streaming members
    /// all are, where a stale message has no use once a fresher one exists; `ordered` for every
    /// other type, whose messages each carry something.
    pub fn default_stream_delivery_profile(&self) -> &'static str {
        let read_newest = match self {
            Self::Texture { .. } | Self::Tensor {} => true,
            Self::ListOf { .. } | Self::Bundle { .. } => {
                self.flows() == [PinFlow::Stream, PinFlow::Value]
            }
            _ => false,
        };
        if read_newest { "newest" } else { "ordered" }
    }
}

impl PinDeclaration {
    /// The delivery profile an input of this declaration reads by when its author states none:
    /// `newest` for a Value, which reads the newest message it has received, `ordered` for a
    /// Trigger, and its type's own for a Stream. `None` for a Stream input whose type is not one
    /// exact type, which states its delivery.
    pub fn default_input_delivery_profile(&self) -> Option<&'static str> {
        match self.flow {
            PinFlow::Value => Some("newest"),
            PinFlow::Trigger => Some("ordered"),
            PinFlow::Stream => match &self.pin_type {
                DeclaredPinType::Is { pin_type } => {
                    Some(pin_type.default_stream_delivery_profile())
                }
                DeclaredPinType::OneOf { .. }
                | DeclaredPinType::Any
                | DeclaredPinType::SameAsInput { .. }
                | DeclaredPinType::SetByInput { .. } => None,
            },
        }
    }

    /// Why port `port_name` may not be declared so: it states a flow a type it may carry does
    /// not allow. `None` when every type it may carry allows its flow.
    pub fn refusal_of_its_flow(&self, port_name: &str) -> Option<String> {
        let pin_types_it_may_carry: &[PinType] = match &self.pin_type {
            DeclaredPinType::Is { pin_type } => std::slice::from_ref(pin_type),
            DeclaredPinType::OneOf { pin_types } => pin_types,
            DeclaredPinType::Any
            | DeclaredPinType::SameAsInput { .. }
            | DeclaredPinType::SetByInput { .. } => &[],
        };
        let refusing_type = pin_types_it_may_carry
            .iter()
            .find(|pin_type| !pin_type.flows().contains(&self.flow))?;
        let flows_it_allows = refusing_type
            .flows()
            .iter()
            .map(|flow| format!("`{}`", flow.name()))
            .collect::<Vec<_>>()
            .join(" or ");
        Some(format!(
            "port `{port_name}` states the flow `{}`, which the {} type does not allow; it flows \
             as {flows_it_allows}",
            self.flow.name(),
            built_in_pin_type(refusing_type.built_in_name()).label,
        ))
    }
}
