// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::{DeclaredPinType, PinDeclaration, PinFlow, PinType, built_in_pin_type};
use crate::{NEWEST_DELIVERY_PROFILE, ORDERED_DELIVERY_PROFILE};

impl PinFlow {
    /// Every flow, in declaration order.
    pub const EVERY_FLOW: [Self; 3] = [Self::Value, Self::Stream, Self::Trigger];

    /// Its name as a declaration and a client write it: `value`, `stream` or `trigger`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Stream => "stream",
            Self::Trigger => "trigger",
        }
    }

    /// Its variant's name, as a Rust port declares it: `Value`, `Stream` or `Trigger`.
    pub fn declaration_name(self) -> &'static str {
        match self {
            Self::Value => "Value",
            Self::Stream => "Stream",
            Self::Trigger => "Trigger",
        }
    }
}

impl PinType {
    /// The types a List or a bundle holds: a List's element type, a bundle's member types.
    /// Empty for every other type.
    fn member_types(&self) -> impl Iterator<Item = &PinType> {
        let (element_type, members) = match self {
            Self::ListOf { element_type } => (Some(element_type.as_ref()), &[][..]),
            Self::Bundle { members } => (None, members.as_slice()),
            _ => (None, &[][..]),
        };
        element_type
            .into_iter()
            .chain(members.iter().map(|member| &member.member_type))
    }

    /// The types a List or a bundle holds that stream by default, each with its flows.
    fn streaming_members_and_flows(&self) -> impl Iterator<Item = (&PinType, &'static [PinFlow])> {
        self.member_types()
            .map(|member_type| (member_type, member_type.flows()))
            .filter(|(_, member_flows)| member_flows[0] == PinFlow::Stream)
    }

    /// The flows a pin of this type may state, its default first. A List or a bundle is one
    /// message, so one holding a member that streams by default takes the flows of the member
    /// that allows least: a member that never travels as a value, else one that may.
    pub fn flows(&self) -> &'static [PinFlow] {
        let own_flows = built_in_pin_type(self.built_in_name()).flows;
        self.streaming_members_and_flows()
            .map(|(_, member_flows)| member_flows)
            .min_by_key(|member_flows| member_flows.contains(&PinFlow::Value))
            .unwrap_or(own_flows)
    }

    /// Words for the member of a List or a bundle whose flows the whole takes, such as
    /// "member `voice`, an Audio": `None` when its own type's flows stand.
    fn the_member_its_flows_come_from(&self) -> Option<String> {
        let flows = self.flows();
        if flows == built_in_pin_type(self.built_in_name()).flows {
            return None;
        }
        let label_of = |member_type: &PinType| built_in_pin_type(member_type.built_in_name()).label;
        match self {
            Self::ListOf { element_type } => {
                Some(format!("elements of the {} type", label_of(element_type)))
            }
            Self::Bundle { members } => members
                .iter()
                .find(|member| member.member_type.flows() == flows)
                .map(|member| {
                    format!(
                        "member `{}` of the {} type",
                        member.name,
                        label_of(&member.member_type)
                    )
                }),
            _ => None,
        }
    }

    /// The flow a pin of this type has when its author states none.
    pub fn default_flow(&self) -> PinFlow {
        self.flows()[0]
    }

    /// The delivery profile a Stream input of this type reads by when its author states none:
    /// `newest` for a picture or a tensor, and for a List or a bundle holding a member that
    /// streams whose members that stream all read `newest`, where a stale message has no use
    /// once a fresher one exists; `ordered` for every other type, whose messages each carry
    /// something.
    pub fn default_stream_delivery_profile(&self) -> &'static str {
        let read_newest = match self {
            Self::Texture { .. } | Self::Tensor {} => true,
            Self::ListOf { .. } | Self::Bundle { .. } => {
                let mut streaming_members = self.streaming_members_and_flows().peekable();
                streaming_members.peek().is_some()
                    && streaming_members.all(|(member_type, _)| {
                        member_type.default_stream_delivery_profile() == NEWEST_DELIVERY_PROFILE
                    })
            }
            _ => false,
        };
        if read_newest {
            NEWEST_DELIVERY_PROFILE
        } else {
            ORDERED_DELIVERY_PROFILE
        }
    }
}

impl PinDeclaration {
    /// The delivery profile an input of this declaration reads by when its author states none:
    /// `newest` for a Value, which reads the newest message it has received, `ordered` for a
    /// Trigger, and its type's own for a Stream. `None` for a Stream input whose type is not one
    /// exact type, which states its delivery.
    pub fn default_input_delivery_profile(&self) -> Option<&'static str> {
        match self.flow {
            PinFlow::Value => Some(NEWEST_DELIVERY_PROFILE),
            PinFlow::Trigger => Some(ORDERED_DELIVERY_PROFILE),
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

    /// Why input `port_name` may not read by `delivery_profile`, which its author states: a
    /// Trigger reads each event, so it reads `ordered` and nothing else. `None` for every
    /// other flow, and for a Trigger stating `ordered`.
    pub fn refusal_of_its_stated_delivery_profile(
        &self,
        port_name: &str,
        delivery_profile: &str,
    ) -> Option<String> {
        (self.flow == PinFlow::Trigger && delivery_profile != ORDERED_DELIVERY_PROFILE).then(|| {
            format!(
                "input port `{port_name}` is a Trigger and states the delivery profile \
                 `{delivery_profile}`; a Trigger reads every event, so it reads \
                 `{ORDERED_DELIVERY_PROFILE}` — drop `delivery_profile`"
            )
        })
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
        let (refusing_type, flows_it_allows) = pin_types_it_may_carry
            .iter()
            .map(|pin_type| (pin_type, pin_type.flows()))
            .find(|(_, flows)| !flows.contains(&self.flow))?;
        let flows_it_allows = flows_it_allows
            .iter()
            .map(|flow| format!("`{}`", flow.name()))
            .collect::<Vec<_>>()
            .join(" or ");
        let held_by = refusing_type
            .the_member_its_flows_come_from()
            .map(|words| format!(", as it holds {words}"))
            .unwrap_or_default();
        Some(format!(
            "port `{port_name}` states the flow `{}`, which the {} type does not allow{held_by}; \
             it flows as {flows_it_allows}",
            self.flow.name(),
            built_in_pin_type(refusing_type.built_in_name()).label,
        ))
    }
}
