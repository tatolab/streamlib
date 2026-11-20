// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The member of a bundle port a link starts or ends at: what such an end carries, the member
//! it names, the refusal of a member its port does not hold, and what the links into one input
//! feed.

use std::collections::{BTreeSet, HashMap};

use super::pin_type_resolution::{PinTypeInItsGraph, the_pin_type_of_a_port_in_its_graph};
use super::{Graph, GraphNode, InputLinkPortRef, NodeRunId, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::{BundleMember, PinType};
use crate::core::error::AMemberTheLinkEndDoesNotHold;
use crate::core::{Error, PortDirection, Result};

/// What a link end naming `member` of a port carrying `port_pin_type` carries: the port's type
/// for an end at the whole port; for one at a member of a bundle, that member and its own type.
/// `Err` naming the member when the port holds no such member.
pub(super) fn what_a_link_end_at_a_port_carries(
    member: Option<&str>,
    port_pin_type: PinTypeInItsGraph,
) -> std::result::Result<(Option<BundleMember>, PinTypeInItsGraph), &str> {
    let Some(member) = member else {
        return Ok((None, port_pin_type));
    };
    let held = the_member_of(&port_pin_type, member).ok_or(member)?.clone();
    let member_pin_type = PinTypeInItsGraph::Is(held.member_type.clone());
    Ok((Some(held), member_pin_type))
}

/// The member named `member` of a port carrying `pin_type`, when it carries one bundle that
/// holds it.
fn the_member_of<'pin>(
    pin_type: &'pin PinTypeInItsGraph,
    member: &str,
) -> Option<&'pin BundleMember> {
    match pin_type {
        PinTypeInItsGraph::Is(PinType::Bundle { members }) => {
            members.iter().find(|held| held.name == member)
        }
        _ => None,
    }
}

/// The bundle a port of `node` carries in `graph` and its member named `member`; `None` when it
/// carries no bundle holding it.
fn the_bundle_member_of_a_port(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    direction: PortDirection,
    member: &str,
) -> Option<(PinType, BundleMember)> {
    let pin_type = the_pin_type_of_a_port_in_its_graph(graph, node, port, direction);
    let held = the_member_of(&pin_type, member)?.clone();
    match pin_type {
        PinTypeInItsGraph::Is(bundle) => Some((bundle, held)),
        _ => None,
    }
}

/// The bundle the output of `from` carries and the member of it the link starts at; `None` for
/// a link from a whole port, or one whose port holds no such member.
pub(crate) fn the_bundle_member_a_link_starts_at(
    graph: &Graph,
    from: &OutputLinkPortRef,
) -> Option<(PinType, BundleMember)> {
    let member = from.member()?;
    let from_node = graph.traversal().v(from.node_run_id()).first()?;
    let output_port = from_node.output_port(from.port_name())?;
    the_bundle_member_of_a_port(graph, from_node, output_port, PortDirection::Output, member)
}

/// The bundle the input of `to` carries and the member of it the link ends at; `None` for a
/// link into a whole port, or one whose port holds no such member.
pub(crate) fn the_bundle_member_a_link_ends_at(
    graph: &Graph,
    to: &InputLinkPortRef,
) -> Option<(PinType, BundleMember)> {
    let member = to.member()?;
    let to_node = graph.traversal().v(to.node_run_id()).first()?;
    let input_port = to_node.input_port(to.port_name())?;
    the_bundle_member_of_a_port(graph, to_node, input_port, PortDirection::Input, member)
}

/// Refuse a link from `from` to `to` whose end names a member its port does not hold, or names
/// one of a port carrying no bundle, naming the members the port has. An end at a whole port,
/// or one that is no port of `graph`, is left for the rest of the judgement.
pub(crate) fn refuse_a_member_its_port_does_not_hold(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    let ends = [
        (
            from.node_run_id(),
            from.port_name(),
            from.member(),
            PortDirection::Output,
        ),
        (
            to.node_run_id(),
            to.port_name(),
            to.member(),
            PortDirection::Input,
        ),
    ];
    for (node_run_id, port_name, member, direction) in ends {
        let Some(member) = member else {
            continue;
        };
        let Some(node) = graph.traversal().v(node_run_id).first() else {
            continue;
        };
        let port = match direction {
            PortDirection::Output => node.output_port(port_name),
            PortDirection::Input => node.input_port(port_name),
        };
        let Some(port) = port else {
            continue;
        };
        let pin_type = the_pin_type_of_a_port_in_its_graph(graph, node, port, direction);
        if the_member_of(&pin_type, member).is_some() {
            continue;
        }
        let (pin_type_in_words, members_it_has) = match &pin_type {
            PinTypeInItsGraph::Is(pin_type @ PinType::Bundle { members }) => (
                pin_type.to_string(),
                members.iter().map(|held| held.name.clone()).collect(),
            ),
            PinTypeInItsGraph::Is(pin_type) => (pin_type.to_string(), Vec::new()),
            PinTypeInItsGraph::OneOf(_) => ("one of several types".to_owned(), Vec::new()),
            PinTypeInItsGraph::Any => ("any type".to_owned(), Vec::new()),
            PinTypeInItsGraph::Untyped => ("no pin type".to_owned(), Vec::new()),
        };
        return Err(Error::PortHasNoMemberNamed(Box::new(
            AMemberTheLinkEndDoesNotHold {
                direction,
                node: node.display_name.clone(),
                port: port.name.clone(),
                member: member.to_owned(),
                pin_type: pin_type_in_words,
                members_it_has,
            },
        )));
    }
    Ok(())
}

/// Refuse a link ending at a member of a bundle input `to` whose type travels only as messages,
/// such as a Tensor: the input's port builds one bundle out of the value each member link
/// carries, and a member of such a type holds no value to build it from.
pub(crate) fn refuse_a_link_into_a_member_no_bundle_is_built_with(
    graph: &Graph,
    to: &InputLinkPortRef,
) -> Result<()> {
    let Some((_, member)) = the_bundle_member_a_link_ends_at(graph, to) else {
        return Ok(());
    };
    if member.member_type.holds_a_stored_value() {
        return Ok(());
    }
    Err(Error::InvalidLink(format!(
        "a link into `{to}` ends at member `{}`, a {}, which travels only as messages, and the \
         input builds its bundle out of the value each member link carries. To fix: link the \
         whole input",
        member.name, member.member_type
    )))
}

/// Refuse a link into `to` when another link already feeds that input and one of the two ends at
/// a member while the other feeds the whole port: a bundle input is fed either whole or member
/// by member, so its node reads one bundle either way.
pub(crate) fn refuse_a_whole_link_beside_a_member_link(
    graph: &Graph,
    to: &InputLinkPortRef,
) -> Result<()> {
    let ends_at_a_member = to.member().is_some();
    let beside = graph.traversal().e(()).iter().find(|link| {
        let held = link.to_port();
        held.node_run_id() == to.node_run_id()
            && held.port_name() == to.port_name()
            && held.member().is_some() != ends_at_a_member
    });
    let Some(beside) = beside else {
        return Ok(());
    };
    let (this_end, the_other_end) = (to.to_string(), beside.to_port().to_string());
    Err(Error::InvalidLink(format!(
        "a link into `{this_end}` would feed input `{}` beside the link into `{the_other_end}`, \
         and a bundle input is fed either whole or member by member. To fix: disconnect the link \
         into `{the_other_end}` first, or end this link {}",
        to.port_name(),
        if ends_at_a_member {
            "at the whole port"
        } else {
            "at one member"
        }
    )))
}

/// What the links into one input of a graph feed: the whole input, and each member of a bundle
/// input a link ends at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WhatTheLinksIntoAnInputFeed {
    the_whole_input: bool,
    members: BTreeSet<String>,
}

impl WhatTheLinksIntoAnInputFeed {
    /// What the links into input `input` of the node `node_run_id` in `graph` feed.
    pub(crate) fn of(graph: &Graph, node_run_id: &NodeRunId, input: &str) -> Self {
        let mut fed = Self::default();
        for link in graph.traversal().e(()).iter() {
            let to = link.to_port();
            if to.node_run_id() == node_run_id && to.port_name() == input {
                fed.add_the_end(to);
            }
        }
        fed
    }

    /// What the links into each input of `graph` feed, by node and input.
    pub(crate) fn of_each_input_of(graph: &Graph) -> HashMap<(NodeRunId, String), Self> {
        let mut fed_by_input: HashMap<(NodeRunId, String), Self> = HashMap::new();
        for link in graph.traversal().e(()).iter() {
            let to = link.to_port();
            fed_by_input
                .entry((to.node_run_id().clone(), to.port_name().to_owned()))
                .or_default()
                .add_the_end(to);
        }
        fed_by_input
    }

    /// Links feeding the whole input when `the_whole_input`, and each of `members`.
    #[cfg(test)]
    pub(crate) fn feeding(the_whole_input: bool, members: &[&str]) -> Self {
        Self {
            the_whole_input,
            members: members.iter().map(|member| (*member).to_owned()).collect(),
        }
    }

    fn add_the_end(&mut self, to: &InputLinkPortRef) {
        match to.member() {
            Some(member) => {
                self.members.insert(member.to_owned());
            }
            None => self.the_whole_input = true,
        }
    }

    /// Whether a link feeds the whole input.
    pub(crate) fn the_whole_input(&self) -> bool {
        self.the_whole_input
    }

    /// Whether a link feeds member `member`, itself or as part of the whole input.
    pub(crate) fn the_member(&self, member: &str) -> bool {
        self.the_whole_input || self.members.contains(member)
    }

    /// Whether any link feeds the input, whole or at one of its members.
    pub(crate) fn anything(&self) -> bool {
        self.the_whole_input || !self.members.is_empty()
    }
}
