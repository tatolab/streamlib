// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link_candidates`: what `check_link` would answer for each link between one placed end
//! and many candidate ends of the other direction, each judged by the one judgement `check_link`
//! makes, and made nowhere.

use streamlib::sdk::error::PortDirection;
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
use crate::link_checked_before_it_is_made::TheRunningGraphLinksAreCheckedOn;
use crate::stream_graph_transaction_executor::GraphTransactionRefusal;

impl ControlOperationDispatcher {
    /// What `check_link` would answer for each link between the placed end `request` names and
    /// each candidate it names, in the order judged: the stream resolved once, as `check_link`
    /// resolves it, and each link judged as `check_link` judges it. A stream the runtime does
    /// not hold, and a non-empty `level`, refuse the call itself.
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
        let running_graph = self
            .the_running_graph_links_are_checked_on(method_name, &stream, level.as_deref())
            .await?
            .map_err(|refusal| {
                ControlOperationRefusal::RefusedAction(Box::new(
                    GraphTransactionRefusal::of_the_whole_transaction(refusal)
                        .into_the_refusal_on_the_wire(method_name),
                ))
            })?;
        let (held, held_direction) = match from {
            LinkCandidatesFrom::Output(held) => (held, PortDirection::Output),
            LinkCandidatesFrom::Input(held) => (held, PortDirection::Input),
        };
        let candidate_direction = match held_direction {
            PortDirection::Output => PortDirection::Input,
            PortDirection::Input => PortDirection::Output,
        };
        let ends = match among {
            LinkCandidatesAmong::PlacedPorts => {
                the_placed_ports_of(&running_graph, candidate_direction)?
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
                    ports
                        .into_iter()
                        .map(move |port| LinkCandidateEnd::NodeTypePort {
                            node_type: node_type.clone(),
                            port: port.name,
                        })
                })
                .collect(),
            LinkCandidatesAmong::Ends(ends) => ends,
        };
        let mut answer = LinkCandidatesCheckResult {
            candidates: Vec::new(),
            not_checked: Vec::new(),
        };
        for end in ends {
            let checked = match &end {
                LinkCandidateEnd::PlacedPort { node, port } => {
                    let candidate = StreamGraphActionLinkEnd {
                        node: node.clone(),
                        port: port.clone(),
                    };
                    match held_direction {
                        PortDirection::Output => running_graph.check(&held, &candidate),
                        PortDirection::Input => running_graph.check(&candidate, &held),
                    }
                }
                LinkCandidateEnd::NodeTypePort { node_type, port } => {
                    running_graph
                        .check_with_a_node_types_port(&held, held_direction, node_type, port)
                        .await
                }
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

/// Each port of `direction` of each node at the running graph's own level, as `graph` lists
/// them.
fn the_placed_ports_of(
    running_graph: &TheRunningGraphLinksAreCheckedOn,
    direction: PortDirection,
) -> Result<Vec<LinkCandidateEnd>, ControlOperationRefusal> {
    let rendered: StreamGraphOnTheWire =
        serde_json::from_value(running_graph.rendered_graph().clone()).map_err(|refusal| {
            ControlOperationRefusal::runtime_failed(format!(
                "{} failed: the stream's graph did not read as `graph` renders one: {refusal}",
                CheckLinkCandidates::METHOD_NAME
            ))
        })?;
    Ok(rendered
        .nodes
        .into_iter()
        .flat_map(|node| {
            let ports = match direction {
                PortDirection::Input => node.ports.inputs,
                PortDirection::Output => node.ports.outputs,
            };
            let node_id = node.id;
            ports
                .into_iter()
                .map(move |port| LinkCandidateEnd::PlacedPort {
                    node: node_id.clone(),
                    port: port.name,
                })
        })
        .collect())
}
