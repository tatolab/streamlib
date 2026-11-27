// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The judgement of a link by its two ends' pin types, by the one rule [`pin_link_verdict`]. A
//! link with an end its graph does not type yet is judged once that end is typed; one from an
//! output of any type, which nothing types, is made as it is, and its reader judges each message
//! it reads.

use super::bundle_member_link_ends::{
    refuse_a_link_into_a_member_no_bundle_is_built_with, refuse_a_member_its_port_does_not_hold,
    refuse_a_whole_link_beside_a_member_link, what_a_link_end_at_a_port_carries,
};
use super::pin_type_resolution::{
    PinTypeInItsGraph, the_pin_type_it_states, the_pin_type_of_a_port_in_its_graph,
};
use super::{Graph, GraphNode, InputLinkPortRef, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::{
    BundleMember, DeclaredPinType, NodeClassImportPath, PinConversion, PinLinkEnd, PinLinkInputEnd,
    PinLinkOutcome, PinLinkRefusal, PinLinkVerdict, PinType, pin_link_verdict,
};
use crate::core::pin_value_packing::message_kind_of;
use crate::core::{Error, PortDirection, Result};
use streamlib_ipc_types::DataChannelPayloadDescriptorKind;

mod bundle_frames_into_an_untyped_input;
mod links_a_type_choice_reaches;
mod named_type_at_an_untyped_end;
mod wired_links_past_an_input;

use bundle_frames_into_an_untyped_input::refuse_bundle_frames_into_an_untyped_input;
pub(crate) use bundle_frames_into_an_untyped_input::the_bundle_frame_type_an_output_may_carry;
use links_a_type_choice_reaches::a_saved_link_differs_by_a_type_choice;
pub(crate) use links_a_type_choice_reaches::a_type_choice_of_the_node_types_a_port;
use named_type_at_an_untyped_end::refuse_a_named_type_at_one_end_of_a_link_untyped_at_the_other;
use wired_links_past_an_input::the_first_wired_link_past_an_input_refused;

/// Whose link a connect makes, which decides how leniently its pin types are judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ALinkConnected {
    /// A link made now: one whose two typed ends the verdict finds no rule connecting, or whose
    /// Image output may write a format an input it reaches does not read, is refused.
    MadeNow,
    /// A link a saved graph already holds, so the graph opens: one the verdict refuses by its
    /// pin types alone is kept when both its ends travel as one kind of message, the refusal
    /// logged as a warning.
    OfASavedGraph,
}

/// The two ports of a link from `from` to `to`, each with what it carries in its graph, when
/// both are typed.
struct TypedLinkEnds<'graph> {
    from_node: &'graph GraphNode,
    output_port: &'graph PortInfo,
    /// The member of the bundle output the link starts at; `None` for the whole port, whose
    /// type is `output_pin_type`. A member's own type is `output_pin_type`.
    output_member: Option<BundleMember>,
    output_pin_type: PinTypeInItsGraph,
    to_node: &'graph GraphNode,
    input_port: &'graph PortInfo,
    /// The member of the bundle input the link ends at; `None` for the whole port, whose type
    /// is `input_pin_type`. A member's own type is `input_pin_type`.
    input_member: Option<BundleMember>,
    input_pin_type: PinTypeInItsGraph,
}

impl TypedLinkEnds<'_> {
    /// Whether a type the output may carry and a type the input may read travel as one kind of
    /// message, so a saved link between them still carries what it carried when it was saved.
    fn both_travel_as_one_kind_of_message(&self) -> bool {
        fn kinds(
            pin_type: &PinTypeInItsGraph,
        ) -> impl Iterator<Item = DataChannelPayloadDescriptorKind> + '_ {
            pin_type
                .the_types_it_may_carry()
                .iter()
                .filter_map(message_kind_of)
        }
        kinds(&self.output_pin_type)
            .any(|output_kind| kinds(&self.input_pin_type).any(|kind| kind == output_kind))
    }

    /// The verdict on the link among `conversions`: [`verdict_between`] its two typed ends.
    fn verdict(&self, conversions: &[PinConversion]) -> PinLinkVerdict {
        verdict_between(
            TypedEnd {
                node: self.from_node,
                port: self.output_port,
                member: self.output_member.as_ref(),
                pin_type: &self.output_pin_type,
            },
            TypedEnd {
                node: self.to_node,
                port: self.input_port,
                member: self.input_member.as_ref(),
                pin_type: &self.input_pin_type,
            },
            conversions,
        )
    }
}

/// The verdict of a link from `output_port` of the node named `output_node_name` to
/// `input_port` of the one named `input_node_name`, of `input_node_type`, judged by the one
/// type each port states, as
/// their node types declare them, among `conversions`, before either node is in a graph; `None`
/// when either port states no one type.
pub(crate) fn the_verdict_of_a_link_between_declared_ports(
    (output_node_name, output_port): (&str, &PortInfo),
    (input_node_name, input_node_type, input_port): (&str, &NodeClassImportPath, &PortInfo),
    conversions: &[PinConversion],
) -> Option<PinLinkVerdict> {
    let the_one_type_it_states = |port: &PortInfo| {
        let declared = port
            .declared_pin_type
            .as_ref()
            .map(|declared| &declared.pin_type);
        match (declared, the_pin_type_it_states(port)) {
            (Some(DeclaredPinType::Is { .. }) | None, PinTypeInItsGraph::Is(pin_type)) => {
                Some(pin_type)
            }
            _ => None,
        }
    };
    let (output_pin_type, input_pin_type) = (
        the_one_type_it_states(output_port)?,
        the_one_type_it_states(input_port)?,
    );
    fn end<'end>(
        node_name: &'end str,
        port: &'end PortInfo,
        pin_type: &'end PinType,
    ) -> PinLinkEnd<'end> {
        PinLinkEnd {
            node_name,
            port_name: &port.name,
            member_name: None,
            pin_type,
            meaning: port.meaning(),
            flow: port.pin.as_ref().map(|pin| pin.flow),
        }
    }
    Some(pin_link_verdict(
        end(output_node_name, output_port, &output_pin_type),
        PinLinkInputEnd {
            end: end(input_node_name, input_port, &input_pin_type),
            node_type: input_node_type,
            input_mode: input_port.input_mode,
        },
        conversions,
    ))
}

/// One typed end of a link: its node, its port, the member of a bundle port it names, and what
/// the end carries in its graph.
#[derive(Clone, Copy)]
struct TypedEnd<'end> {
    node: &'end GraphNode,
    port: &'end PortInfo,
    member: Option<&'end BundleMember>,
    pin_type: &'end PinTypeInItsGraph,
}

impl<'end> TypedEnd<'end> {
    fn link_end(&self, pin_type: &'end PinType) -> PinLinkEnd<'end> {
        PinLinkEnd {
            node_name: &self.node.display_name,
            port_name: &self.port.name,
            member_name: self.member.map(|member| member.name.as_str()),
            pin_type,
            meaning: match self.member {
                Some(member) => member.meaning.as_deref(),
                None => self.port.meaning(),
            },
            flow: self.port.pin.as_ref().map(|pin| pin.flow),
        }
    }

    /// Each type it may carry: its one type, or each of several.
    fn the_types_it_may_carry(&self) -> &'end [PinType] {
        self.pin_type.the_types_it_may_carry()
    }
}

/// Whether what a port carries in its graph is typed enough for the verdict to judge a link at
/// it: one type, one of several, or, at an input, any type.
fn is_typed(pin_type: &PinTypeInItsGraph) -> bool {
    match pin_type {
        PinTypeInItsGraph::Is(_) | PinTypeInItsGraph::Any => true,
        PinTypeInItsGraph::OneOf(pin_types) => !pin_types.is_empty(),
        PinTypeInItsGraph::Untyped => false,
    }
}

/// How a link from `output` to `input`, two typed ends, connects among `conversions`. An input
/// of any type takes every typed output as it is; at an end of several types, the link connects
/// as the best pair of their types does, refused as the first pair is when none connects.
fn verdict_between(
    output: TypedEnd<'_>,
    input: TypedEnd<'_>,
    conversions: &[PinConversion],
) -> PinLinkVerdict {
    if matches!(input.pin_type, PinTypeInItsGraph::Any) {
        // A setting of any type takes the output's own, judged by its mode and its flow alone.
        if input.port.is_a_setting()
            && let Some(output_pin_type) = output.the_types_it_may_carry().first()
        {
            return pin_link_verdict(
                output.link_end(output_pin_type),
                PinLinkInputEnd {
                    end: input.link_end(output_pin_type),
                    node_type: &input.node.node_type,
                    input_mode: input.port.input_mode,
                },
                &[],
            );
        }
        return PinLinkVerdict {
            outcome: if input.port.takes_a_wire_on_a_node_of(&input.node.node_type) {
                PinLinkOutcome::Connects
            } else {
                PinLinkOutcome::Refused(Box::new(PinLinkRefusal::input_takes_no_wire(
                    &input.node.display_name,
                    &input.port.name,
                )))
            },
            warnings: Vec::new(),
        };
    }
    let mut best: Option<PinLinkVerdict> = None;
    for output_pin_type in output.the_types_it_may_carry() {
        for input_pin_type in input.the_types_it_may_carry() {
            let verdict = pin_link_verdict(
                output.link_end(output_pin_type),
                PinLinkInputEnd {
                    end: input.link_end(input_pin_type),
                    node_type: &input.node.node_type,
                    input_mode: input.port.input_mode,
                },
                conversions,
            );
            if best
                .as_ref()
                .is_none_or(|best| how_well_it_connects(&verdict) < how_well_it_connects(best))
            {
                best = Some(verdict);
            }
        }
    }
    best.expect("a typed end of no Any input carries at least one type")
}

/// The rank of a verdict's outcome, lowest best: as it is, through a free conversion, through a
/// convert node, refused.
fn how_well_it_connects(verdict: &PinLinkVerdict) -> u8 {
    match verdict.outcome {
        PinLinkOutcome::Connects => 0,
        PinLinkOutcome::ConnectsWithFreeConversion(_) => 1,
        PinLinkOutcome::ConnectsThroughConvertNode(_) => 2,
        PinLinkOutcome::Refused(_) => 3,
    }
}

/// The typed ends of a link from `from` to `to` in `graph`; `None` when either end is no node
/// of it, or either port is untyped there.
fn the_typed_ends_of<'graph>(
    graph: &'graph Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Option<TypedLinkEnds<'graph>> {
    let from_node = graph.traversal().v(from.node_run_id()).first()?;
    let to_node = graph.traversal().v(to.node_run_id()).first()?;
    let output_port = from_node.output_port(from.port_name())?;
    let input_port = to_node.input_port(to.port_name())?;
    let port_pin_type =
        the_pin_type_of_a_port_in_its_graph(graph, from_node, output_port, PortDirection::Output);
    let (output_member, output_pin_type) =
        what_a_link_end_at_a_port_carries(from.member(), port_pin_type).ok()?;
    let (input_member, input_pin_type) = what_a_link_end_at_a_port_carries(
        to.member(),
        the_pin_type_of_a_port_in_its_graph(graph, to_node, input_port, PortDirection::Input),
    )
    .ok()?;
    (is_typed(&output_pin_type) && is_typed(&input_pin_type)).then_some(TypedLinkEnds {
        from_node,
        output_port,
        output_member,
        output_pin_type,
        to_node,
        input_port,
        input_member,
        input_pin_type,
    })
}

/// What the input end `to` of a wired link carries, its input carrying `input_pin_type`: the
/// member it ends at and that member's type, or the whole port's type for an end at the whole
/// port, or at a member the port no longer holds, which its own refusal names.
fn what_the_input_end_of_a_link_carries(
    to: &InputLinkPortRef,
    input_pin_type: PinTypeInItsGraph,
) -> (Option<BundleMember>, PinTypeInItsGraph) {
    match what_a_link_end_at_a_port_carries(to.member(), input_pin_type.clone()) {
        Ok(what_it_carries) => what_it_carries,
        Err(_) => (None, input_pin_type),
    }
}

/// What a link is judged by at its two ends: what each port carries in its graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TheEndsALinkIsJudgedBy {
    output_pin_type: PinTypeInItsGraph,
    input_pin_type: PinTypeInItsGraph,
}

/// What a link from `from` to `to` is judged by at its ends; `None` when either end is no port
/// of `graph`.
pub(crate) fn the_ends_a_link_is_judged_by(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Option<TheEndsALinkIsJudgedBy> {
    let from_node = graph.traversal().v(from.node_run_id()).first()?;
    let to_node = graph.traversal().v(to.node_run_id()).first()?;
    let output_port = from_node.output_port(from.port_name())?;
    let input_port = to_node.input_port(to.port_name())?;
    Some(TheEndsALinkIsJudgedBy {
        output_pin_type: the_pin_type_of_a_port_in_its_graph(
            graph,
            from_node,
            output_port,
            PortDirection::Output,
        ),
        input_pin_type: the_pin_type_of_a_port_in_its_graph(
            graph,
            to_node,
            input_port,
            PortDirection::Input,
        ),
    })
}

/// The judgement of a link from `from` to `to` by its two ends' pin types, among
/// `conversions`: a convert node's output judged by the type it gives under the settings it
/// holds. `None` when either end is no node of `graph`, or either port is untyped.
pub(crate) fn the_verdict_on_a_link_between_typed_ends(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    conversions: &[PinConversion],
) -> Option<PinLinkVerdict> {
    the_typed_ends_of(graph, from, to).map(|ends| ends.verdict(conversions))
}

/// Whether `connect` would refuse a link from `from` to `to` made now in `graph`.
pub(crate) fn a_link_made_now_would_be_refused(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> bool {
    refuse_a_link_its_pin_types_refuse(graph, from, to, ALinkConnected::MadeNow).is_err()
}

/// Refuse a link from `from` to `to` its verdict among the conversions `graph`'s stream
/// resolves refuses; `a_link_connected` says whether one the verdict refuses by its pin types
/// alone is kept. Answers the verdict of a link between two typed ends that connects, and `None`
/// for one with an end its graph does not type yet, which is judged once it is, or for a saved
/// graph's link whose ends differ since a type choice at one of them changed, which is kept for
/// the compile to hold unwired.
pub(crate) fn refuse_a_link_its_pin_types_refuse(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<Option<PinLinkVerdict>> {
    let judged = refuse_a_link_its_pin_types_refuse_as_it_is(graph, from, to, a_link_connected);
    if judged.is_err()
        && a_link_connected == ALinkConnected::OfASavedGraph
        && a_saved_link_differs_by_a_type_choice(graph, from, to)
    {
        tracing::info!(
            from = %from,
            to = %to,
            "a saved link is kept, held unwired: its ends differ since a type choice changed"
        );
        return Ok(None);
    }
    judged
}

/// Refuse a link from `from` to `to` as [`refuse_a_link_its_pin_types_refuse`] does, keeping no
/// saved link a type choice left differing.
fn refuse_a_link_its_pin_types_refuse_as_it_is(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<Option<PinLinkVerdict>> {
    refuse_a_member_its_port_does_not_hold(graph, from, to)?;
    refuse_a_link_into_a_member_no_bundle_is_built_with(graph, to)?;
    if a_link_connected == ALinkConnected::MadeNow {
        refuse_a_whole_link_beside_a_member_link(graph, to)?;
    }
    let conversions = graph.node_types_this_stream_resolves().pin_conversions();
    let Some(ends) = the_typed_ends_of(graph, from, to) else {
        if let Some(member) = from.member() {
            return Err(Error::InvalidLink(format!(
                "a link from member `{member}` of `{from}` enters `{to}`, which carries no type \
                 yet, and a member is read only by a typed input. To fix: link the whole port, \
                 or link the member to a typed input"
            )));
        }
        if let Some(member) = to.member() {
            return Err(Error::InvalidLink(format!(
                "a link from `{from}` ends at member `{member}` of `{to}`, and `{from}` carries \
                 no type yet, so the member's value cannot be read off its messages. To fix: \
                 link the member from a typed output, or link the whole port"
            )));
        }
        refuse_bundle_frames_into_an_untyped_input(graph, from, to)?;
        refuse_a_named_type_at_one_end_of_a_link_untyped_at_the_other(graph, from, to)?;
        if a_link_connected == ALinkConnected::MadeNow {
            refuse_a_link_made_now_a_wired_link_past_its_input_refuses(
                graph,
                from,
                to,
                &conversions,
            )?;
        }
        return Ok(None);
    };
    let verdict = ends.verdict(&conversions);
    if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
        if a_link_connected == ALinkConnected::OfASavedGraph
            && matches!(refusal, PinLinkRefusal::PinTypesDoNotConnect { .. })
            && ends.both_travel_as_one_kind_of_message()
        {
            tracing::warn!(
                from = %from,
                to = %to,
                "a saved link is kept, both its ends travelling as one kind of message, though \
                 its pin types refuse it: {refusal}"
            );
            return Ok(None);
        }
        return Err(Error::PinLinkRefused(Box::new(refusal.clone())));
    }
    if a_link_connected == ALinkConnected::MadeNow
        && let Some(refused) = the_first_wired_link_past_an_input_refused(
            graph,
            ends.to_node,
            &ends.input_port.name,
            &ends.output_pin_type,
            &conversions,
        )
    {
        return Err(Error::InvalidLink(refused));
    }
    Ok(Some(verdict))
}

/// Refuse a link from `from` to `to` made now, its input untyped, when the output it comes from
/// carries a type an output following that input would then carry into a wired link the type
/// does not connect to, naming that link.
fn refuse_a_link_made_now_a_wired_link_past_its_input_refuses(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    conversions: &[PinConversion],
) -> Result<()> {
    let (Some(from_node), Some(to_node)) = (
        graph.traversal().v(from.node_run_id()).first(),
        graph.traversal().v(to.node_run_id()).first(),
    ) else {
        return Ok(());
    };
    let (Some(output_port), Some(input_port)) = (
        from_node.output_port(from.port_name()),
        to_node.input_port(to.port_name()),
    ) else {
        return Ok(());
    };
    let carried =
        the_pin_type_of_a_port_in_its_graph(graph, from_node, output_port, PortDirection::Output);
    if !is_typed(&carried) {
        return Ok(());
    }
    match the_first_wired_link_past_an_input_refused(
        graph,
        to_node,
        &input_port.name,
        &carried,
        conversions,
    ) {
        Some(refused) => Err(Error::InvalidLink(refused)),
        None => Ok(()),
    }
}

/// `refusal` with its own fix left out, for a refusal that names the fix of what it is under.
fn the_refusal_without_its_fix(refusal: &str) -> &str {
    refusal
        .split_once(". To fix:")
        .map_or(refusal, |(refused, _)| refused)
}
