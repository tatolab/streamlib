// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link`: what `connect` would answer for one link, judged against the stream's running
//! graph, or the placed stream's level it is inside, by the engine's own judgement of a
//! connect, and made nowhere; and that one judgement of a link on a running graph, which
//! `check_link_candidates` makes of each candidate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use streamlib::sdk::error::PortDirection;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::runtime::{ANodeTypesPort, RuntimeOperations, WhatALiveConnectMakesOfALink};
use streamlib_control_contract::{CheckLink, CheckLinkRequest, Connect, ControlOperation};
use streamlib_project_repository::{
    GraphEditStream, ProjectRepositoriesOpenInThisRuntime, StreamGraphActionLinkEnd,
    StreamGraphDocument, StreamGraphDocumentLinkEnd,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, GraphNodePortOnTheWire, GraphNodePortsOnTheWire,
    GraphTransactionNodeInsertedBecause, LinkCheckResult, StreamGraphOnTheWire,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::refusal_code_and_place::RefusalCodeAndPlace;
use crate::stream_graph_transaction_executor::{
    AConnectJudgedInsideAPlacedStream, GraphRequestRefusal, GraphTransactionRefusal,
    StreamsTheRuntimeHolds, WhatAConnectAsks, WhereAnOperationEdits,
    a_connect_judged_inside_a_placed_stream, the_current_name_of_a_port,
    the_level_holds_both_ends_by_uuid, the_node_a_connects_end_names,
    what_a_connect_asks_of_the_running_stream,
};

/// Why a link is not checked yet, in the runtime's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkNotCheckedYet {
    /// It is inside a placed stream, under a non-empty `level`, which only `check_link` judges.
    InsideAPlacedStream,
    /// It closes a loop of function nodes, which `connect` closes through a `PreviousValue`
    /// node it adds.
    ClosesALoopOfFunctionNodes,
    /// A connect would make it without judging it, for the reason held, in the runtime's words.
    ItsConnectMakesItUnjudged(String),
}

impl std::fmt::Display for LinkNotCheckedYet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InsideAPlacedStream => {
                "the link is inside a placed stream; check each such link with `check_link`"
            }
            Self::ClosesALoopOfFunctionNodes => {
                "the link closes a loop of function nodes no `PreviousValue` node is in, which \
                 `connect` closes through a `PreviousValue` node it adds"
            }
            Self::ItsConnectMakesItUnjudged(why) => why.as_str(),
        })
    }
}

impl LinkNotCheckedYet {
    /// The refusal of `method_name` itself for a link not checked yet.
    pub(crate) fn refusing(&self, method_name: &str) -> ControlOperationRefusal {
        ControlOperationRefusal::failed(format!(
            "{method_name} failed: {self}, and such a link is not checked yet"
        ))
    }
}

/// The running graph of one loaded stream, as each link checked on it reads it.
pub(crate) struct TheRunningGraphLinksAreCheckedOn {
    stream_asset: Uuid,
    stream_id: String,
    stream_operations: Arc<dyn RuntimeOperations>,
    /// The directory of the stream's project, where the runtime lists one: a refusal of a port
    /// a node does not have suggests the ports its type has, as the project describes them.
    project_directory: Option<PathBuf>,
    rendered_graph: Value,
    rendered: StreamGraphOnTheWire,
}

/// Why links are not checked on a running graph: the stream named is not held, or the call
/// itself is refused.
pub(crate) enum NoRunningGraphToCheckLinksOn {
    /// The runtime holds no stream of the name, refused as `connect` refuses it.
    StreamNotFound(GraphRequestRefusal),
    /// The call itself is refused.
    Refused(ControlOperationRefusal),
}

impl ControlOperationDispatcher {
    /// The running graph of the stream `stream` names, resolved as a graph edit resolves it, on
    /// which `method_name` checks links at `level`: a non-empty `level` refused unchecked, and
    /// a stream the runtime does not hold answered as the refusal `connect` answers for it.
    pub(crate) async fn the_running_graph_links_are_checked_on(
        &self,
        method_name: &str,
        stream: &GraphEditStream,
        level: Option<&[Uuid]>,
    ) -> Result<TheRunningGraphLinksAreCheckedOn, NoRunningGraphToCheckLinksOn> {
        let of_an_engine_error = |refusal: streamlib::sdk::error::Error| {
            NoRunningGraphToCheckLinksOn::Refused(ControlOperationRefusal::of_an_engine_error(
                method_name,
                &refusal,
            ))
        };
        if level.is_some_and(|level| !level.is_empty()) {
            return Err(NoRunningGraphToCheckLinksOn::Refused(
                LinkNotCheckedYet::InsideAPlacedStream.refusing(method_name),
            ));
        }
        let listings = self.operations_on_the_loaded_streams.list_streams();
        let streams_the_runtime_holds = StreamsTheRuntimeHolds::listed_in(&listings);
        let (stream_asset, stream_id) = streams_the_runtime_holds
            .the_stream_named(stream)
            .map_err(|runtime_words| {
                NoRunningGraphToCheckLinksOn::StreamNotFound(GraphRequestRefusal::coded(
                    RefusalCodeAndPlace::coded(ControlOperationRefusalCode::StreamNotFound),
                    runtime_words,
                ))
            })?;
        let project_directory = streams_the_runtime_holds
            .the_location_of(&stream_id)
            .map(|location| location.project_directory.clone());
        let stream_operations = self
            .operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(&stream_id)
            .map_err(of_an_engine_error)?;
        let rendered_graph = stream_operations
            .to_json_async()
            .await
            .map_err(of_an_engine_error)?;
        let rendered = StreamGraphOnTheWire::deserialize(&rendered_graph).map_err(|refusal| {
            NoRunningGraphToCheckLinksOn::Refused(ControlOperationRefusal::runtime_failed(format!(
                "{method_name} failed: the stream's graph did not read as `graph` renders \
                     one: {refusal}"
            )))
        })?;
        Ok(TheRunningGraphLinksAreCheckedOn {
            stream_asset,
            stream_id,
            stream_operations,
            project_directory,
            rendered_graph,
            rendered,
        })
    }

    /// What `connect` would answer for the link `request` names: the stream it names resolved
    /// as a graph edit resolves it, the link read as a transaction's `connect` reads it, and
    /// the rest judged by the engine's own judgement of a live connect. A link `connect` carries
    /// through the convert node it places answers that node. Neither the graph nor the project's
    /// history is changed, and no refusal is kept on the project's change feed.
    ///
    /// A link inside a placed stream, under a non-empty `level`, is read from the project's
    /// documents as a transaction's `connect` reads it, and judged among that level's nodes by
    /// the engine's own judgement of a link there.
    ///
    /// Each link [`LinkNotCheckedYet`] names is refused unchecked.
    pub(crate) async fn check_link(
        &self,
        CheckLinkRequest {
            stream,
            level,
            source,
            target,
        }: CheckLinkRequest,
    ) -> Result<LinkCheckResult, ControlOperationRefusal> {
        let running_graph = match self
            .the_running_graph_links_are_checked_on(CheckLink::METHOD_NAME, &stream, None)
            .await
        {
            Ok(running_graph) => running_graph,
            Err(NoRunningGraphToCheckLinksOn::StreamNotFound(refusal)) => {
                return Ok(LinkCheckResult::of_a_link_connect_refuses(
                    GraphTransactionRefusal::of_a_connect(None, &source, &target, refusal)
                        .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
                ));
            }
            Err(NoRunningGraphToCheckLinksOn::Refused(refusal)) => return Err(refusal),
        };
        match level.filter(|level| !level.is_empty()) {
            None => running_graph.check(&source, &target),
            Some(level) => {
                running_graph
                    .check_inside_a_placed_stream(
                        &level,
                        &source,
                        &target,
                        Arc::clone(&self.project_repositories),
                    )
                    .await?
            }
        }
        .map_err(|not_checked| not_checked.refusing(CheckLink::METHOD_NAME))
    }
}

impl TheRunningGraphLinksAreCheckedOn {
    /// The stream's id.
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// The running graph, rendered as `graph` renders it, read once.
    pub(crate) fn rendered(&self) -> &StreamGraphOnTheWire {
        &self.rendered
    }

    /// The placed end `held` of `held_direction`, its node by UUID, once the running graph is
    /// seen to hold its node and a port of `held_direction` its port names, under the port's
    /// current name or a name it had; otherwise refused as `connect` refuses such an end.
    pub(crate) fn the_placed_end(
        &self,
        held: &StreamGraphActionLinkEnd,
        held_direction: PortDirection,
    ) -> Result<StreamGraphDocumentLinkEnd, GraphRequestRefusal> {
        let node = the_node_a_connects_end_names(&held.node)?;
        let node_id = node.to_string();
        let Some(placed) = self
            .rendered
            .nodes
            .iter()
            .find(|placed| placed.id == node_id)
        else {
            return Err(GraphRequestRefusal::coded(
                RefusalCodeAndPlace {
                    refused_node: Some(held.node.clone()),
                    ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotFound)
                },
                format!("stream `{}` holds no node {node}", self.stream_id),
            ));
        };
        let port_not_found = || {
            GraphRequestRefusal::coded(
                RefusalCodeAndPlace {
                    refused_node: Some(held.node.clone()),
                    refused_field: (held_direction == PortDirection::Input)
                        .then(|| held.port.clone()),
                    ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::PortNotFound)
                },
                format!(
                    "node `{}` has no {held_direction} port named '{}'",
                    placed.name, held.port
                ),
            )
        };
        let port = cast_exposed_name_to_url_safe(&held.port)
            .map_err(|_| port_not_found())?
            .into_owned();
        let port = the_current_name_of_a_port(
            port,
            held_direction,
            Some(placed.node_type.as_str().to_owned()),
            self.stream_operations.as_ref(),
        );
        if !the_ports_of(&placed.ports, held_direction)
            .iter()
            .any(|placed_port| placed_port.name == port)
        {
            return Err(port_not_found());
        }
        Ok(StreamGraphDocumentLinkEnd {
            node,
            port,
            member: held.member.clone(),
        })
    }

    /// What `connect` would answer for the link from `source` to `target`: read as a
    /// transaction's `connect` reads it, then judged by the engine's own judgement of a live
    /// connect, a link `connect` carries through the convert node it places answering that node.
    /// A refusal of a port a node does not have suggests the ports of the node's type, where the
    /// stream's project is listed.
    pub(crate) fn check(
        &self,
        source: &StreamGraphActionLinkEnd,
        target: &StreamGraphActionLinkEnd,
    ) -> Result<LinkCheckResult, LinkNotCheckedYet> {
        let refused_on_the_running_stream =
            |edits: Option<WhereAnOperationEdits<'_>>, refusal: GraphRequestRefusal| {
                let refusal = GraphTransactionRefusal::of_a_connect(edits, source, target, refusal);
                LinkCheckResult::of_a_link_connect_refuses(
                    match &self.project_directory {
                        Some(project_directory) => refusal
                            .suggesting_the_ports_of_the_node_type_it_names(
                                project_directory,
                                &self.rendered_graph,
                            ),
                        None => refusal,
                    }
                    .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
                )
            };
        let level_as_drawn = match StreamGraphDocument::from_value(self.rendered_graph.clone()) {
            Ok(level_as_drawn) => level_as_drawn,
            Err(not_read) => return Ok(refused_on_the_running_stream(None, not_read.into())),
        };
        let at_its_own_level = WhereAnOperationEdits {
            stream_id: &self.stream_id,
            level: &[],
            level_holds_both_link_ends: the_level_holds_both_ends_by_uuid(
                &level_as_drawn,
                source,
                target,
                &HashMap::new(),
            ),
        };
        let asked = match what_a_connect_asks_of_the_running_stream(
            source.clone(),
            target.clone(),
            &self.rendered_graph,
            &level_as_drawn,
            self.stream_operations.as_ref(),
        ) {
            Ok(asked) => asked,
            Err(refusal) => {
                return Ok(refused_on_the_running_stream(
                    Some(at_its_own_level),
                    refusal,
                ));
            }
        };
        let link = match asked {
            WhatAConnectAsks::Link(link)
            | WhatAConnectAsks::ThroughANode {
                original_link: link,
                because: GraphTransactionNodeInsertedBecause::ConvertsPinTypes,
                ..
            } => link,
            WhatAConnectAsks::ThroughANode {
                because:
                    GraphTransactionNodeInsertedBecause::ClosesALoop
                    | GraphTransactionNodeInsertedBecause::ForAnotherReason,
                ..
            } => return Err(LinkNotCheckedYet::ClosesALoopOfFunctionNodes),
        };
        the_check_of_what_a_live_connect_makes(
            self.stream_operations
                .what_a_live_connect_makes_of_the_link(&link),
            |refusal| refused_on_the_running_stream(Some(at_its_own_level), refusal),
        )
    }

    /// What `connect` would answer for the link from `source` to `target` at `level`, a
    /// non-empty level of placed streams inside this stream: read from the project's documents,
    /// each as `project_repositories` holds it, as a transaction's `connect` reads it, and judged
    /// among that level's nodes by the engine's own judgement of a link there; refused itself
    /// when the read does not finish.
    async fn check_inside_a_placed_stream(
        &self,
        level: &[Uuid],
        source: &StreamGraphActionLinkEnd,
        target: &StreamGraphActionLinkEnd,
        project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    ) -> Result<Result<LinkCheckResult, LinkNotCheckedYet>, ControlOperationRefusal> {
        let refused_as_connect_refuses =
            |edits: Option<WhereAnOperationEdits<'_>>, refusal: GraphRequestRefusal| {
                LinkCheckResult::of_a_link_connect_refuses(
                    GraphTransactionRefusal::of_a_connect(edits, source, target, refusal)
                        .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
                )
            };
        let judged = {
            let (stream_asset, stream_id) = (self.stream_asset, self.stream_id.clone());
            let project_directory = self.project_directory.clone();
            let stream_operations = Arc::clone(&self.stream_operations);
            let rendered_graph = self.rendered_graph.clone();
            let (level, source, target) = (level.to_vec(), source.clone(), target.clone());
            tokio::task::spawn_blocking(move || {
                let project_directory = project_directory.ok_or_else(|| {
                    GraphRequestRefusal::from(format!(
                        "stream `{stream_id}` is of no project, so no placed stream inside it is \
                         read from a document"
                    ))
                })?;
                let project_repository =
                    project_repositories.the_project_repository_at(&project_directory)?;
                Ok(a_connect_judged_inside_a_placed_stream(
                    source,
                    target,
                    stream_asset,
                    &level,
                    &rendered_graph,
                    stream_operations.as_ref(),
                    &project_repository.lock(),
                ))
            })
            .await
            .map_err(ControlOperationRefusal::the_task_did_not_finish(format!(
                "{} failed: the read of the placed stream",
                CheckLink::METHOD_NAME
            )))?
        };
        let AConnectJudgedInsideAPlacedStream {
            level_holds_both_link_ends,
            made,
        } = match judged {
            Ok(judged) => judged,
            Err(refusal) => return Ok(Ok(refused_as_connect_refuses(None, refusal))),
        };
        let inside_the_placed_stream = WhereAnOperationEdits {
            stream_id: &self.stream_id,
            level,
            level_holds_both_link_ends,
        };
        Ok(match made {
            Ok(Some(made)) => the_check_of_a_link_made(made),
            Ok(None) => Err(LinkNotCheckedYet::ClosesALoopOfFunctionNodes),
            Err(refusal) => Ok(refused_as_connect_refuses(
                Some(inside_the_placed_stream),
                refusal,
            )),
        })
    }

    /// What `connect` would answer for each link between `held`, a placed end of
    /// `held_direction`, and one of `node_types_ports`, in their order, once an `add_node` with
    /// no input values had placed a node of the type: judged by the engine's own judgement of a
    /// live connect, the port by what it declares, and nothing described or registered, so a
    /// type the stream has not described yet is not checked yet. A refusal names no link,
    /// since no node holds the port yet.
    pub(crate) async fn check_with_node_types_ports(
        &self,
        held: &StreamGraphDocumentLinkEnd,
        held_direction: PortDirection,
        node_types_ports: &[ANodeTypesPort<'_>],
    ) -> Vec<Result<LinkCheckResult, LinkNotCheckedYet>> {
        let refused_as_connect_refuses = |refusal: GraphRequestRefusal| {
            LinkCheckResult::of_a_link_connect_refuses(
                GraphTransactionRefusal::of_the_whole_transaction(refusal)
                    .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
            )
        };
        match self
            .stream_operations
            .what_a_live_connect_makes_of_links_with_node_types_ports(
                held,
                held_direction,
                node_types_ports,
            )
            .await
        {
            Ok(what_each_makes) => what_each_makes
                .into_iter()
                .map(|what_it_makes| {
                    the_check_of_what_a_live_connect_makes(
                        what_it_makes,
                        refused_as_connect_refuses,
                    )
                })
                .collect(),
            Err(refusal) => {
                let refused = refused_as_connect_refuses(refusal.into());
                vec![Ok(refused); node_types_ports.len()]
            }
        }
    }
}

/// The ports of `direction` among `ports`, as `graph` renders a node's.
pub(crate) fn the_ports_of(
    ports: &GraphNodePortsOnTheWire,
    direction: PortDirection,
) -> &[GraphNodePortOnTheWire] {
    match direction {
        PortDirection::Input => &ports.inputs,
        PortDirection::Output => &ports.outputs,
    }
}

/// The check of a link of which a live connect would make `what_it_makes`, refused as
/// `refused_as_connect_refuses` answers a refusal.
fn the_check_of_what_a_live_connect_makes(
    what_it_makes: streamlib::sdk::error::Result<WhatALiveConnectMakesOfALink>,
    refused_as_connect_refuses: impl FnOnce(GraphRequestRefusal) -> LinkCheckResult,
) -> Result<LinkCheckResult, LinkNotCheckedYet> {
    match what_it_makes {
        Ok(made) => the_check_of_a_link_made(made),
        Err(refusal) => Ok(refused_as_connect_refuses(refusal.into())),
    }
}

/// The check of a link a connect would make as `made`: the verdict it judges the link by; why
/// it is not checked yet when the link is made without being judged.
fn the_check_of_a_link_made(
    made: WhatALiveConnectMakesOfALink,
) -> Result<LinkCheckResult, LinkNotCheckedYet> {
    made.the_verdict_it_judges_by()
        .map(|verdict| LinkCheckResult::of_a_link_connect_makes(verdict.as_deref()))
        .map_err(LinkNotCheckedYet::ItsConnectMakesItUnjudged)
}
