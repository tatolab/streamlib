// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The judgement of a link by its two ends' pin types, or by their families when one is untyped.

use super::input_values::the_pin_values_with_the_live_ones_over_them;
use super::pin_type_resolution::{
    PinTypeInItsGraph, the_pin_type_it_states, the_pin_type_of_a_port_in_its_graph,
};
use super::port_family_link_checks::{
    FamiliesTakeTheLink, refuse_a_link_between_ports_whose_families_do_not_match,
    refuse_what_a_link_carries_on_past_its_input, the_inputs_an_input_is_carried_on_to,
    the_outputs_a_link_carries_on,
};
use super::{
    Graph, GraphEdgeWithComponents, GraphNode, InputLinkPortRef, NodeRunId, OutputLinkPortRef,
    PendingDeletionComponent, PortInfo,
};
use crate::core::descriptors::{
    DeclaredPinType, PinConversion, PinLinkEnd, PinLinkInputEnd, PinLinkOutcome, PinLinkRefusal,
    PinLinkVerdict, PinType, PortFamilyDeclaration, PortFamilyLinkEndpoint, pin_link_verdict,
    refusal_of_the_names_and_codec_a_link_carries,
};
use crate::core::{Error, PortDirection, Result};

/// Whose link a connect makes, which decides how leniently it is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ALinkConnected {
    /// A link made now: one whose two typed ends the verdict finds no rule connecting, or whose
    /// Image output may write a format an input it reaches does not read, is refused, and one
    /// an island cannot take is refused before it is made.
    MadeNow,
    /// A link a saved graph already holds, so the graph opens: one the verdict refuses by its
    /// pin types alone, or whose Image formats alone are refused, is kept when its ports'
    /// families, names and codec take it, each refusal logged as a warning, and one an island
    /// cannot take is left to the commit that builds the stream to refuse.
    OfASavedGraph,
}

/// The two ports of a link from `from` to `to`, each with what it carries in its graph, when
/// both are typed.
struct TypedLinkEnds<'graph> {
    from_node: &'graph GraphNode,
    output_port: &'graph PortInfo,
    output_pin_type: PinTypeInItsGraph,
    to_node: &'graph GraphNode,
    input_port: &'graph PortInfo,
    input_pin_type: PinTypeInItsGraph,
}

impl TypedLinkEnds<'_> {
    /// Whether each end carries exactly one type. A port of any type or of one of several is
    /// judged by its family first, as an untyped port is: the message it carries travels on its
    /// family whatever type it takes.
    fn has_one_type_at_each_end(&self) -> bool {
        matches!(self.output_pin_type, PinTypeInItsGraph::Is(_))
            && matches!(self.input_pin_type, PinTypeInItsGraph::Is(_))
    }

    /// The verdict on the link among `conversions`: [`verdict_between`] its two typed ends, the
    /// output of a node of a conversion whose output follows a setting judged by the type it
    /// gives under the settings it holds in `graph`.
    fn verdict(&self, graph: &Graph, conversions: &[PinConversion]) -> PinLinkVerdict {
        let given_under_what_it_holds = the_type_a_convert_node_gives_under_what_it_holds(
            graph,
            self.from_node,
            self.output_port,
            conversions,
        )
        .map(PinTypeInItsGraph::Is);
        verdict_between(
            TypedEnd {
                node: self.from_node,
                port: self.output_port,
                pin_type: given_under_what_it_holds
                    .as_ref()
                    .unwrap_or(&self.output_pin_type),
            },
            TypedEnd {
                node: self.to_node,
                port: self.input_port,
                pin_type: &self.input_pin_type,
            },
            conversions,
        )
    }
}

/// The type `output_port` of `node` gives under the settings it holds, its config with its pin
/// values over it, when `node` is a node of a conversion whose output follows a setting; `None`
/// for any other output, which gives what it carries in its graph.
fn the_type_a_convert_node_gives_under_what_it_holds(
    graph: &Graph,
    node: &GraphNode,
    output_port: &PortInfo,
    conversions: &[PinConversion],
) -> Option<PinType> {
    let conversion = conversions.iter().find(|conversion| {
        conversion.node_type == node.node_type
            && conversion.output_port_name == output_port.name
            && conversion.output_picked_for_the_input.is_some()
    })?;
    let mut held = node
        .config
        .as_ref()
        .and_then(|config| config.as_object())
        .cloned()
        .unwrap_or_default();
    held.extend(the_pin_values_with_the_live_ones_over_them(graph, node));
    conversion.output_given_under(&held)
}

/// The verdict of a link from `output_port` of the node named `output_node_name` to
/// `input_port` of the one named `input_node_name`, judged by the one type each port states, as
/// their node types declare them, among `conversions`, before either node is in a graph; `None`
/// when either port states no one type.
pub(crate) fn the_verdict_of_a_link_between_declared_ports(
    (output_node_name, output_port): (&str, &PortInfo),
    (input_node_name, input_port): (&str, &PortInfo),
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
            meaning: port.meaning.as_deref(),
        }
    }
    Some(pin_link_verdict(
        end(output_node_name, output_port, &output_pin_type),
        PinLinkInputEnd {
            end: end(input_node_name, input_port, &input_pin_type),
            input_mode: input_port.input_mode,
        },
        conversions,
    ))
}

/// One typed end of a link: its node, its port and what the port carries in its graph.
#[derive(Clone, Copy)]
struct TypedEnd<'end> {
    node: &'end GraphNode,
    port: &'end PortInfo,
    pin_type: &'end PinTypeInItsGraph,
}

impl<'end> TypedEnd<'end> {
    fn link_end(&self, pin_type: &'end PinType) -> PinLinkEnd<'end> {
        PinLinkEnd {
            node_name: &self.node.display_name,
            port_name: &self.port.name,
            member_name: None,
            pin_type,
            meaning: self.port.meaning.as_deref(),
        }
    }

    /// Each type it may carry: its one type, or each of several.
    fn the_types_it_may_carry(&self) -> &'end [PinType] {
        match self.pin_type {
            PinTypeInItsGraph::Is(pin_type) => std::slice::from_ref(pin_type),
            PinTypeInItsGraph::OneOf(pin_types) => pin_types,
            PinTypeInItsGraph::Any | PinTypeInItsGraph::Untyped => &[],
        }
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
        return PinLinkVerdict {
            outcome: if input.port.takes_a_wire() {
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
    let output_pin_type =
        the_pin_type_of_a_port_in_its_graph(graph, from_node, output_port, PortDirection::Output);
    let input_pin_type =
        the_pin_type_of_a_port_in_its_graph(graph, to_node, input_port, PortDirection::Input);
    (is_typed(&output_pin_type) && is_typed(&input_pin_type)).then_some(TypedLinkEnds {
        from_node,
        output_port,
        output_pin_type,
        to_node,
        input_port,
        input_pin_type,
    })
}

/// What a link is judged by at its two ends: what each port carries in its graph, and its
/// family declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TheEndsALinkIsJudgedBy {
    output_pin_type: PinTypeInItsGraph,
    output_family: PortFamilyDeclaration,
    input_pin_type: PinTypeInItsGraph,
    input_family: PortFamilyDeclaration,
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
        output_family: output_port.family.clone(),
        input_pin_type: the_pin_type_of_a_port_in_its_graph(
            graph,
            to_node,
            input_port,
            PortDirection::Input,
        ),
        input_family: input_port.family.clone(),
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
    the_typed_ends_of(graph, from, to).map(|ends| ends.verdict(graph, conversions))
}

/// Whether `connect` would refuse a link from `from` to `to` made now in `graph`.
pub(crate) fn a_link_made_now_would_be_refused(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> bool {
    the_link_its_pin_types_or_families_take(graph, from, to, ALinkConnected::MadeNow).is_err()
}

/// Refuse a link from `from` to `to` its verdict among the conversions `graph`'s stream
/// resolves, or its families, refuse; `a_link_connected` says whether one the verdict, or its
/// Image formats, refuse alone is kept. Answers the verdict of a link between two typed ends
/// that connects, and `None` for one its families judge or take.
pub(crate) fn refuse_a_link_its_pin_types_or_families_refuse(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<Option<PinLinkVerdict>> {
    let LinkTaken { verdict, families } =
        the_link_its_pin_types_or_families_take(graph, from, to, a_link_connected)?;
    if let FamiliesTakeTheLink::ButForImageFormatsNotRead(refusal) = families {
        tracing::warn!(
            from = %from,
            to = %to,
            "a saved link is kept, its ports' families taking it, though its output may write an \
             Image format an input it reaches does not read: {refusal}"
        );
    }
    Ok(verdict)
}

/// How a link neither its verdict nor its families refuse is taken: the verdict of its two typed
/// ends when it connects, and how its families take it.
struct LinkTaken {
    verdict: Option<PinLinkVerdict>,
    families: FamiliesTakeTheLink,
}

/// How a link from `from` to `to` that neither its verdict nor its families refuse is taken, as
/// [`refuse_a_link_its_pin_types_or_families_refuse`] judges it.
fn the_link_its_pin_types_or_families_take(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<LinkTaken> {
    let Some(ends) = the_typed_ends_of(graph, from, to) else {
        return refuse_a_link_between_ports_whose_families_do_not_match(
            graph,
            from,
            to,
            a_link_connected,
        )
        .map(|families| LinkTaken {
            verdict: None,
            families,
        });
    };
    let families_judged_first = if ends.has_one_type_at_each_end() {
        None
    } else {
        Some(refuse_a_link_between_ports_whose_families_do_not_match(
            graph,
            from,
            to,
            a_link_connected,
        )?)
    };
    let conversions = graph.node_types_this_stream_resolves().pin_conversions();
    let verdict = ends.verdict(graph, &conversions);
    let TypedLinkEnds {
        from_node,
        output_port,
        to_node,
        input_port,
        ..
    } = &ends;
    let (from_node, output_port, to_node, input_port) =
        (*from_node, *output_port, *to_node, *input_port);
    if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
        if a_link_connected == ALinkConnected::OfASavedGraph
            && matches!(refusal, PinLinkRefusal::PinTypesDoNotConnect { .. })
            && let Ok(families) = refuse_a_link_between_ports_whose_families_do_not_match(
                graph,
                from,
                to,
                a_link_connected,
            )
        {
            tracing::warn!(
                from = %from,
                to = %to,
                "a saved link is kept, its ports' families taking it, though its pin types \
                 refuse it: {refusal}"
            );
            return Ok(LinkTaken {
                verdict: None,
                families,
            });
        }
        return Err(Error::PinLinkRefused(Box::new(refusal.clone())));
    }
    if let Some(refusal) = refusal_of_the_names_and_codec_a_link_carries(
        PortFamilyLinkEndpoint {
            node_name: &from_node.display_name,
            port_name: &output_port.name,
            declaration: &output_port.family,
        },
        output_port.family.family,
        from_node.config.as_ref(),
        PortFamilyLinkEndpoint {
            node_name: &to_node.display_name,
            port_name: &input_port.name,
            declaration: &input_port.family,
        },
    ) {
        return Err(Error::InvalidLink(refusal));
    }
    let mut families = families_judged_first.unwrap_or(FamiliesTakeTheLink::Wholly);
    let inputs_carried_on_to = the_inputs_an_input_is_carried_on_to(graph, to_node, to.port_name());
    for carried_on in the_outputs_a_link_carries_on(graph, from_node, from.port_name()) {
        refuse_what_a_link_carries_on_past_its_input(
            carried_on,
            &inputs_carried_on_to,
            from,
            to,
            a_link_connected,
            &mut families,
        )?;
    }
    if a_link_connected == ALinkConnected::MadeNow {
        refuse_a_link_out_of_an_output_following_the_input(graph, &ends, &conversions)?;
    }
    Ok(LinkTaken {
        verdict: Some(verdict),
        families,
    })
}

/// Refuse the link of `ends` when an output of its input's node that declares the type of that
/// input carries a link the type it would now carry does not connect to, naming that link.
fn refuse_a_link_out_of_an_output_following_the_input(
    graph: &Graph,
    ends: &TypedLinkEnds<'_>,
    conversions: &[PinConversion],
) -> Result<()> {
    let outputs_following_the_input = ends.to_node.ports.outputs.iter().filter(|output| {
        matches!(
            output.declared_pin_type.as_ref().map(|declared| &declared.pin_type),
            Some(DeclaredPinType::SameAsInput { input_port_name })
                if *input_port_name == ends.input_port.name
        )
    });
    for following_output in outputs_following_the_input {
        let links_out_of_it = graph
            .traversal()
            .v(&ends.to_node.id)
            .out_e()
            .iter()
            .filter(|link| {
                link.source().port_name() == following_output.name
                    && !link.has::<PendingDeletionComponent>()
            });
        for link_out in links_out_of_it {
            let Some(downstream_node) = graph
                .traversal()
                .v(link_out.to_port().node_run_id())
                .first()
            else {
                continue;
            };
            let Some(downstream_input) = downstream_node.input_port(link_out.to_port().port_name())
            else {
                continue;
            };
            let downstream_pin_type = the_pin_type_of_a_port_in_its_graph(
                graph,
                downstream_node,
                downstream_input,
                PortDirection::Input,
            );
            if !is_typed(&downstream_pin_type) {
                continue;
            }
            let verdict = verdict_between(
                TypedEnd {
                    node: ends.to_node,
                    port: following_output,
                    pin_type: &ends.output_pin_type,
                },
                TypedEnd {
                    node: downstream_node,
                    port: downstream_input,
                    pin_type: &downstream_pin_type,
                },
                conversions,
            );
            if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
                return Err(Error::InvalidLink(format!(
                    "output `{}` of `{}` takes the type linked into `{}`, and under it the wired \
                     link {} → {} is refused: {refusal}",
                    following_output.name,
                    ends.to_node.display_name,
                    ends.input_port.name,
                    link_out.source(),
                    link_out.to_port(),
                )));
            }
        }
    }
    Ok(())
}

/// Refuse the type the setting `setting_name` of the node `node_run_id` now holds when a link
/// wired at a port whose type it chooses, or at an output declaring the type of such an input,
/// would not connect under it, naming that link.
pub(crate) fn refuse_a_type_choice_a_wired_link_of_the_node_does_not_fit(
    graph: &Graph,
    node_run_id: &NodeRunId,
    setting_name: &str,
) -> Result<()> {
    let Some(node) = graph.traversal().v(node_run_id).first() else {
        return Ok(());
    };
    let declared_as = |port: &PortInfo| {
        port.declared_pin_type
            .as_ref()
            .map(|declared| declared.pin_type.clone())
    };
    let inputs_it_types: Vec<&str> = node
        .ports
        .inputs
        .iter()
        .filter(|port| {
            matches!(declared_as(port), Some(DeclaredPinType::SetByInput { input_name })
                if input_name == setting_name)
        })
        .map(|port| port.name.as_str())
        .collect();
    let outputs_it_types: Vec<&str> = node
        .ports
        .outputs
        .iter()
        .filter(|port| match declared_as(port) {
            Some(DeclaredPinType::SetByInput { input_name }) => input_name == setting_name,
            Some(DeclaredPinType::SameAsInput { input_port_name }) => {
                inputs_it_types.contains(&input_port_name.as_str())
            }
            _ => false,
        })
        .map(|port| port.name.as_str())
        .collect();
    if inputs_it_types.is_empty() && outputs_it_types.is_empty() {
        return Ok(());
    }
    let conversions = graph.node_types_this_stream_resolves().pin_conversions();
    let links_it_types = graph.traversal().e(()).iter().filter(|link| {
        !link.has::<PendingDeletionComponent>()
            && ((link.to_port().node_run_id() == node_run_id
                && inputs_it_types.contains(&link.to_port().port_name()))
                || (link.source().node_run_id() == node_run_id
                    && outputs_it_types.contains(&link.source().port_name())))
    });
    for link in links_it_types {
        let Some(ends) = the_typed_ends_of(graph, link.source(), link.to_port()) else {
            continue;
        };
        if let Some(refusal) = ends
            .verdict(graph, &conversions)
            .refusal_of_the_link_made_as_it_is()
        {
            return Err(Error::Configuration(format!(
                "setting `{setting_name}` of `{}` chooses a type under which the wired link {} → \
                 {} is refused: {refusal}. To fix: choose a type that link connects to, or \
                 remove the link first",
                node.display_name,
                link.source(),
                link.to_port(),
            )));
        }
    }
    Ok(())
}
