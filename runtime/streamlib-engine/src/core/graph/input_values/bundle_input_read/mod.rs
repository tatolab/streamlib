// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a bundle input reads while unlinked: each member resolved alone, its live value over its
//! saved one over the input's default over the member's own, so one member set live leaves every
//! other reading what it read.

use serde_json::{Map, Value};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    BundleMemberValueReadOnTheWire, InputValueComesFromOnTheWire,
};

use crate::core::descriptors::{BundleMember, PinType};
use crate::core::graph::{GraphNode, PortInfo, WhatTheLinksIntoAnInputFeed};

/// The members of the bundle input `port` carries; `None` for an input carrying another type.
pub(crate) fn the_bundle_members_of(port: &PortInfo) -> Option<&[BundleMember]> {
    match &port.pin.as_ref()?.pin_type {
        PinType::Bundle { members, .. } => Some(members),
        _ => None,
    }
}

/// The values one bundle input holds, each an object keyed by member when it holds one: the
/// value set live, the saved one and the input's default.
#[derive(Clone, Copy)]
pub(crate) struct ValuesABundleInputHolds<'held> {
    pub(crate) live: Option<&'held Value>,
    pub(crate) saved: Option<&'held Value>,
    pub(crate) input_default: Option<&'held Value>,
}

impl ValuesABundleInputHolds<'_> {
    /// What member `member` reads, and where it comes from: the first of the live value, the
    /// saved one and the input's default that holds the member, else the member's own default;
    /// `None` when none does.
    fn the_member_read(
        &self,
        member: &BundleMember,
    ) -> Option<(Value, InputValueComesFromOnTheWire)> {
        let held_in = |held: Option<&Value>| held?.as_object()?.get(&member.name).cloned();
        held_in(self.live)
            .map(|value| (value, InputValueComesFromOnTheWire::Live))
            .or_else(|| {
                held_in(self.saved).map(|value| (value, InputValueComesFromOnTheWire::Saved))
            })
            .or_else(|| {
                held_in(self.input_default)
                    .or_else(|| member.default_value.clone())
                    .map(|value| (value, InputValueComesFromOnTheWire::Default))
            })
    }

    /// The bundle the input reads, each member that reads a value keyed by its name; `None`
    /// when no member reads one.
    pub(crate) fn the_bundle_read(&self, members: &[BundleMember]) -> Option<Value> {
        let read: Map<String, Value> = members
            .iter()
            .filter_map(|member| {
                let (value, _) = self.the_member_read(member)?;
                Some((member.name.clone(), value))
            })
            .collect();
        (!read.is_empty()).then_some(Value::Object(read))
    }

    /// What each of `members` reads, in the order the bundle declares them, as `graph` renders
    /// it; `wire` for each member `fed` says a link feeds, itself or as part of the whole input.
    pub(crate) fn each_member_read(
        &self,
        members: &[BundleMember],
        fed: &WhatTheLinksIntoAnInputFeed,
    ) -> Vec<BundleMemberValueReadOnTheWire> {
        members
            .iter()
            .map(|member| {
                let (value, value_comes_from) = if fed.the_member(&member.name) {
                    (None, Some(InputValueComesFromOnTheWire::Wire))
                } else {
                    match self.the_member_read(member) {
                        Some((value, value_comes_from)) => (Some(value), Some(value_comes_from)),
                        None => (Some(Value::Null), None),
                    }
                };
                BundleMemberValueReadOnTheWire {
                    member: member.name.clone(),
                    value,
                    value_comes_from,
                }
            })
            .collect()
    }
}

/// The input values a live set on input `input` of `node` is checked against: its saved input
/// values, and, on a bundle input, the bundle its members read once `live_after_the_set`
/// is its live value, each member set live over its saved one.
pub(crate) fn the_input_values_a_live_set_is_checked_against(
    node: &GraphNode,
    input: &str,
    live_after_the_set: Option<&Value>,
) -> Map<String, Value> {
    let mut input_values = node.input_values.clone();
    let Some((port, members)) = node
        .input_port(input)
        .and_then(|port| Some((port, the_bundle_members_of(port)?)))
    else {
        return input_values;
    };
    let read = ValuesABundleInputHolds {
        live: live_after_the_set,
        saved: node.input_values.get(&port.name),
        input_default: port.pin_default.as_ref(),
    }
    .the_bundle_read(members);
    match read {
        Some(read) => input_values.insert(port.name.clone(), read),
        None => input_values.remove(&port.name),
    };
    input_values
}

/// `held`, a value saved on a bundle input, with `live`, the members set live on it, over it:
/// each member `live` names takes its live value, and every other keeps the one `held` gives.
pub(crate) fn the_live_members_over_the_held_ones(held: Option<&Value>, live: Value) -> Value {
    let (Some(Value::Object(held)), Value::Object(live)) = (held, &live) else {
        return live;
    };
    let mut over = held.clone();
    over.extend(
        live.iter()
            .map(|(member, value)| (member.clone(), value.clone())),
    );
    Value::Object(over)
}

#[cfg(test)]
mod tests;
