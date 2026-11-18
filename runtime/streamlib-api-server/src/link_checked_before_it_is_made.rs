// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link`: what `connect` would answer for one link, judged against the stream's running
//! graph by the engine's own judgement of a connect, and made nowhere; and that one judgement of
//! a link on a running graph, which `check_link_candidates` makes of each candidate.

use std::sync::Arc;

use serde_json::Value;
use streamlib::sdk::error::PortDirection;
use streamlib::sdk::runtime::{RuntimeOperations, WhatALiveConnectMakesOfALink};
use streamlib_control_contract::{CheckLink, CheckLinkRequest, Connect, ControlOperation};
use streamlib_project_repository::{
    GraphEditStream, StreamGraphActionLinkEnd, StreamGraphDocumentLinkEnd, StreamGraphOperation,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, LinkCheckResult,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::refusal_code_and_place::RefusalCodeAndPlace;
use crate::stream_graph_transaction_executor::{
    GraphRequestRefusal, GraphTransactionRefusal, StreamsTheRuntimeHolds,
    the_node_a_connects_end_names, the_operation_a_connect_asks_of_the_running_stream,
};

/// Why a link is not checked yet, in the runtime's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkNotCheckedYet {
    /// It is inside a placed stream, under a non-empty `level`.
    InsideAPlacedStream,
    /// It closes a loop of function nodes, which `connect` closes through a `PreviousValue`
    /// node it adds.
    ClosesALoopOfFunctionNodes,
    /// It reaches a node that runs as no node of its own, which the live graph judges only as
    /// it is reconciled to a document holding the link.
    ReachesAFragmentNode,
}

impl std::fmt::Display for LinkNotCheckedYet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InsideAPlacedStream => {
                "the link is inside a placed stream; check one at the stream's own level, with \
                 `level` omitted"
            }
            Self::ClosesALoopOfFunctionNodes => {
                "the link closes a loop of function nodes no `PreviousValue` node is in, which \
                 `connect` closes through a `PreviousValue` node it adds"
            }
            Self::ReachesAFragmentNode => {
                "the link reaches a node that runs as no node of its own, such as a fragment"
            }
        })
    }
}

impl LinkNotCheckedYet {
    /// The refusal of `method_name` itself for a link not checked yet.
    pub(crate) fn refusing(self, method_name: &str) -> ControlOperationRefusal {
        ControlOperationRefusal::failed(format!(
            "{method_name} failed: {self}, and such a link is not checked yet"
        ))
    }
}

/// The running graph of one loaded stream, as each link checked on it reads it.
pub(crate) struct TheRunningGraphLinksAreCheckedOn {
    stream_id: String,
    stream_operations: Arc<dyn RuntimeOperations>,
    rendered_graph: Value,
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
    ) -> Result<
        Result<TheRunningGraphLinksAreCheckedOn, GraphRequestRefusal>,
        ControlOperationRefusal,
    > {
        let of_an_engine_error = |refusal: streamlib::sdk::error::Error| {
            ControlOperationRefusal::of_an_engine_error(method_name, &refusal)
        };
        if level.is_some_and(|level| !level.is_empty()) {
            return Err(LinkNotCheckedYet::InsideAPlacedStream.refusing(method_name));
        }
        let listings = self.operations_on_the_loaded_streams.list_streams();
        let stream_id = match StreamsTheRuntimeHolds::listed_in(&listings).the_stream_named(stream)
        {
            Ok((_, stream_id)) => stream_id,
            Err(runtime_words) => {
                return Ok(Err(GraphRequestRefusal::coded(
                    RefusalCodeAndPlace::coded(ControlOperationRefusalCode::StreamNotFound),
                    runtime_words,
                )));
            }
        };
        let stream_operations = self
            .operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(&stream_id)
            .map_err(of_an_engine_error)?;
        let rendered_graph = stream_operations
            .to_json_async()
            .await
            .map_err(of_an_engine_error)?;
        Ok(Ok(TheRunningGraphLinksAreCheckedOn {
            stream_id,
            stream_operations,
            rendered_graph,
        }))
    }

    /// What `connect` would answer for the link `request` names: the stream it names resolved
    /// as a graph edit resolves it, the link read as a transaction's `connect` reads it, and
    /// the rest judged by the engine's own judgement of a live connect. Neither the graph nor
    /// the project's history is changed, and no refusal is kept on the project's change feed.
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
            .the_running_graph_links_are_checked_on(
                CheckLink::METHOD_NAME,
                &stream,
                level.as_deref(),
            )
            .await?
        {
            Ok(running_graph) => running_graph,
            Err(refusal) => {
                return Ok(LinkCheckResult::of_a_link_connect_refuses(
                    GraphTransactionRefusal::of_a_connect(&source, &target, refusal)
                        .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
                ));
            }
        };
        running_graph
            .check(&source, &target)
            .map_err(|not_checked| not_checked.refusing(CheckLink::METHOD_NAME))
    }
}

impl TheRunningGraphLinksAreCheckedOn {
    /// The stream's id.
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// The running graph, rendered as `graph` renders it.
    pub(crate) fn rendered_graph(&self) -> &Value {
        &self.rendered_graph
    }

    /// What `connect` would answer for the link from `source` to `target`: read as a
    /// transaction's `connect` reads it, then judged by the engine's own judgement of a live
    /// connect.
    pub(crate) fn check(
        &self,
        source: &StreamGraphActionLinkEnd,
        target: &StreamGraphActionLinkEnd,
    ) -> Result<LinkCheckResult, LinkNotCheckedYet> {
        let refused_as_connect_refuses = |refusal: GraphRequestRefusal| {
            LinkCheckResult::of_a_link_connect_refuses(
                GraphTransactionRefusal::of_a_connect(source, target, refusal)
                    .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
            )
        };
        let operation = match the_operation_a_connect_asks_of_the_running_stream(
            source.clone(),
            target.clone(),
            &self.rendered_graph,
            self.stream_operations.as_ref(),
        ) {
            Ok(operation) => operation,
            Err(refusal) => return Ok(refused_as_connect_refuses(refusal)),
        };
        let StreamGraphOperation::Connect { link } = operation else {
            return Err(LinkNotCheckedYet::ClosesALoopOfFunctionNodes);
        };
        the_check_of_what_a_live_connect_makes(
            self.stream_operations
                .what_a_live_connect_makes_of_the_link(&link),
            refused_as_connect_refuses,
        )
    }

    /// What `connect` would answer for the link between `held`, an end of `held_direction` the
    /// graph holds, and port `port` of `node_type`, once an `add_node` with no input values had
    /// placed a node of the type: `held` read as a transaction's `connect` reads an end, then
    /// the link judged by the engine's own judgement of a live connect, the port by what it
    /// declares, the type described first as an `add_node` describes it. A refusal names no
    /// link, since no node holds the port yet.
    pub(crate) async fn check_with_a_node_types_port(
        &self,
        held: &StreamGraphActionLinkEnd,
        held_direction: PortDirection,
        node_type: &str,
        port: &str,
    ) -> Result<LinkCheckResult, LinkNotCheckedYet> {
        let refused_as_connect_refuses = |refusal: GraphRequestRefusal| {
            LinkCheckResult::of_a_link_connect_refuses(
                GraphTransactionRefusal::of_the_whole_transaction(refusal)
                    .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
            )
        };
        let held = match the_node_a_connects_end_names(&held.node) {
            Ok(node) => StreamGraphDocumentLinkEnd {
                node,
                port: held.port.clone(),
            },
            Err(refusal) => return Ok(refused_as_connect_refuses(refusal)),
        };
        the_check_of_what_a_live_connect_makes(
            self.stream_operations
                .what_a_live_connect_makes_of_a_link_with_a_node_types_port(
                    &held,
                    held_direction,
                    node_type,
                    port,
                )
                .await,
            refused_as_connect_refuses,
        )
    }
}

/// The check of a link of which a live connect would make `what_it_makes`, refused as
/// `refused_as_connect_refuses` answers a refusal.
fn the_check_of_what_a_live_connect_makes(
    what_it_makes: streamlib::sdk::error::Result<WhatALiveConnectMakesOfALink>,
    refused_as_connect_refuses: impl FnOnce(GraphRequestRefusal) -> LinkCheckResult,
) -> Result<LinkCheckResult, LinkNotCheckedYet> {
    match what_it_makes {
        Ok(WhatALiveConnectMakesOfALink::Makes(verdict)) => {
            Ok(LinkCheckResult::of_a_link_connect_makes(verdict.as_deref()))
        }
        Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade) => {
            Err(LinkNotCheckedYet::ReachesAFragmentNode)
        }
        Err(refusal) => Ok(refused_as_connect_refuses(refusal.into())),
    }
}
