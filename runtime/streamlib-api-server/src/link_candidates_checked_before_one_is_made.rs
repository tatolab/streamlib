// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link_candidates`: what `check_link` would answer for each link between one placed end
//! and many candidate ends of the other direction, each judged by the one judgement `check_link`
//! makes, and made nowhere.

use streamlib::sdk::descriptors::{DeclaredPinType, PinType, PortDescriptor};
use streamlib::sdk::error::PortDirection;
use streamlib::sdk::runtime::ANodeTypesPort;
use streamlib_control_contract::{
    CheckLinkCandidates, CheckLinkCandidatesRequest, ControlOperation, LinkCandidatesAmong,
    LinkCandidatesFrom,
};
use streamlib_project_repository::StreamGraphActionLinkEnd;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    LinkCandidateChecked, LinkCandidateEnd, LinkCandidateNotChecked, LinkCandidatesCheckResult,
    PinLinkOutcomeOnTheWire, StreamGraphOnTheWire,
};

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::link_checked_before_it_is_made::{NoRunningGraphToCheckLinksOn, the_ports_of};
use crate::stream_graph_transaction_executor::{GraphRequestRefusal, GraphTransactionRefusal};

impl ControlOperationDispatcher {
    /// What `check_link` would answer for each link between the placed end `request` names and
    /// each candidate it names, in the order named: the stream resolved and rendered once, as
    /// `check_link` resolves it, and each link judged as `check_link` judges it, every placed
    /// candidate against that one render and every node type's port after them. A stream the
    /// runtime does not hold, a non-empty `level`, and a `from` naming a node or a port of its
    /// direction the graph does not hold refuse the call itself.
    pub(crate) async fn check_link_candidates(
        &self,
        CheckLinkCandidatesRequest {
            stream,
            level,
            from,
            among,
            include_refused,
        }: CheckLinkCandidatesRequest,
    ) -> Result<LinkCandidatesCheckResult, ControlOperationRefusal> {
        let method_name = CheckLinkCandidates::METHOD_NAME;
        let refusing_the_call = |refusal: GraphRequestRefusal| {
            ControlOperationRefusal::RefusedAction(Box::new(
                GraphTransactionRefusal::of_the_whole_transaction(refusal)
                    .into_the_refusal_on_the_wire(method_name),
            ))
        };
        let running_graph = self
            .the_running_graph_links_are_checked_on(method_name, &stream, level.as_deref())
            .await
            .map_err(|no_running_graph| match no_running_graph {
                NoRunningGraphToCheckLinksOn::StreamNotFound(refusal) => refusing_the_call(refusal),
                NoRunningGraphToCheckLinksOn::Refused(refusal) => refusal,
            })?;
        let (held, held_direction) = match from {
            LinkCandidatesFrom::Output(held) => (held, PortDirection::Output),
            LinkCandidatesFrom::Input(held) => (held, PortDirection::Input),
        };
        let placed_end = running_graph
            .the_placed_end(&held, held_direction)
            .map_err(refusing_the_call)?;
        let candidate_direction = held_direction.opposite();
        let ends = match among {
            LinkCandidatesAmong::PlacedPorts => {
                the_placed_ports_of(running_graph.rendered(), candidate_direction)
            }
            LinkCandidatesAmong::NodeTypePorts => self
                .operations_on_the_loaded_streams
                .node_catalog_of_the_stream_a_call_names(running_graph.stream_id())
                .map_err(|refusal| {
                    ControlOperationRefusal::of_an_engine_error(method_name, &refusal)
                })?
                .into_iter()
                .flat_map(|node_type| {
                    let ports = match candidate_direction {
                        PortDirection::Input => node_type.inputs,
                        PortDirection::Output => node_type.outputs,
                    };
                    let node_type = node_type.node_class_import_path.as_str().to_owned();
                    ports.into_iter().flat_map(move |port| {
                        let node_type = node_type.clone();
                        let members = match candidate_direction {
                            PortDirection::Output => the_members_a_declared_port_carries(&port),
                            PortDirection::Input => Vec::new(),
                        };
                        the_port_and_its_members(port.name, members).map(move |(port, member)| {
                            LinkCandidateEnd::NodeTypePort {
                                node_type: node_type.clone(),
                                port,
                                member,
                            }
                        })
                    })
                })
                .collect(),
            LinkCandidatesAmong::Ends(ends) => ends,
        };
        let mut placed_checks = Vec::with_capacity(ends.len());
        let mut node_types_ports = Vec::new();
        for end in &ends {
            match end {
                LinkCandidateEnd::PlacedPort { node, port, member } => {
                    let candidate = StreamGraphActionLinkEnd {
                        node: node.clone(),
                        port: port.clone(),
                        member: member.clone(),
                    };
                    placed_checks.push(Some(match held_direction {
                        PortDirection::Output => running_graph.check(&held, &candidate),
                        PortDirection::Input => running_graph.check(&candidate, &held),
                    }));
                }
                LinkCandidateEnd::NodeTypePort {
                    node_type,
                    port,
                    member,
                } => {
                    node_types_ports.push(ANodeTypesPort {
                        node_type,
                        port,
                        member: member.as_deref(),
                    });
                    placed_checks.push(None);
                }
            }
        }
        let mut node_types_checks = running_graph
            .check_with_node_types_ports(&placed_end, held_direction, &node_types_ports)
            .await
            .into_iter();
        let mut answer = LinkCandidatesCheckResult {
            candidates: Vec::new(),
            not_checked: Vec::new(),
        };
        for (end, placed_check) in ends.into_iter().zip(placed_checks) {
            let Some(checked) = placed_check.or_else(|| node_types_checks.next()) else {
                return Err(ControlOperationRefusal::runtime_failed(format!(
                    "{method_name} failed: a node type's port was answered no check"
                )));
            };
            match checked {
                Ok(check)
                    if check.outcome == PinLinkOutcomeOnTheWire::Refused && !include_refused => {}
                Ok(check) => answer.candidates.push(LinkCandidateChecked { end, check }),
                Err(not_checked) => answer.not_checked.push(LinkCandidateNotChecked {
                    end,
                    why: not_checked.to_string(),
                }),
            }
        }
        Ok(answer)
    }
}

/// Each port of `direction` of each node at the level `rendered` holds, as `graph` lists them,
/// each output followed by each member of the bundle it carries.
fn the_placed_ports_of(
    rendered: &StreamGraphOnTheWire,
    direction: PortDirection,
) -> Vec<LinkCandidateEnd> {
    rendered
        .nodes
        .iter()
        .flat_map(|node| {
            the_ports_of(&node.ports, direction)
                .iter()
                .flat_map(move |port| {
                    let members = match direction {
                        PortDirection::Output => {
                            the_members_of(port.pin.as_ref().map(|pin| &pin.pin_type))
                        }
                        PortDirection::Input => Vec::new(),
                    };
                    the_port_and_its_members(port.name.clone(), members).map(|(port, member)| {
                        LinkCandidateEnd::PlacedPort {
                            node: node.id.clone(),
                            port,
                            member,
                        }
                    })
                })
        })
        .collect()
}

/// `port`, then `port` at each of `members`, each as a port name and the member it names.
fn the_port_and_its_members(
    port: String,
    members: Vec<String>,
) -> impl Iterator<Item = (String, Option<String>)> {
    std::iter::once((port.clone(), None)).chain(
        members
            .into_iter()
            .map(move |member| (port.clone(), Some(member))),
    )
}

/// The members of the bundle a port declaring `port` carries whatever it is linked to: its
/// declared type's, when that is one bundle; none for any other declaration.
fn the_members_a_declared_port_carries(port: &PortDescriptor) -> Vec<String> {
    match port
        .pin_declaration
        .as_ref()
        .map(|declared| &declared.pin_type)
    {
        Some(DeclaredPinType::Is { pin_type }) => the_members_of(Some(pin_type)),
        _ => Vec::new(),
    }
}

/// The names of the members of `pin_type`, in declared order, when it is a bundle.
fn the_members_of(pin_type: Option<&PinType>) -> Vec<String> {
    match pin_type {
        Some(PinType::Bundle { members }) => {
            members.iter().map(|member| member.name.clone()).collect()
        }
        _ => Vec::new(),
    }
}
