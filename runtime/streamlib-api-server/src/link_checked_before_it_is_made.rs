// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link`: what `connect` would answer for one link, judged against the stream's running
//! graph by the engine's own judgement of a connect, and made nowhere.

use streamlib::sdk::runtime::WhatALiveConnectMakesOfALink;
use streamlib_control_contract::{CheckLinkRequest, Connect, ControlOperation};
use streamlib_project_repository::{
    StreamGraphActionLinkEnd, StreamGraphDocument, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, GraphLinkEndOnTheWire, LinkCheckResult,
    ProjectActionRefusalOnTheWire, RefusedGraphLinkOnTheWire,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::refusal_code_and_place::RefusalCodeAndPlace;
use crate::stream_graph_transaction_executor::StreamsTheRuntimeHolds;

impl ControlOperationDispatcher {
    /// What `connect` would answer for the link `request` names: the stream it names resolved
    /// as a graph edit resolves it, each end's node read as `connect` reads it, a link the
    /// stream's graph already holds refused as `connect` refuses it, and the rest judged by the
    /// engine's own judgement of a live connect. Neither the graph nor the project's history is
    /// changed, and no refusal is kept on the project's change feed.
    ///
    /// A link inside a placed stream, under a non-empty `level`, is refused unchecked, and so is
    /// one reaching a node that runs as no node of its own, which the live graph judges only as
    /// it is reconciled to a document holding the link.
    pub(crate) async fn check_link(
        &self,
        CheckLinkRequest {
            stream,
            level,
            source,
            target,
        }: CheckLinkRequest,
    ) -> Result<LinkCheckResult, ControlOperationRefusal> {
        let method_name = streamlib_control_contract::CheckLink::METHOD_NAME;
        if level.is_some_and(|level| !level.is_empty()) {
            return Err(ControlOperationRefusal::failed(format!(
                "{method_name} failed: a link inside a placed stream is not checked yet; check \
                 one at the stream's own level, with `level` omitted"
            )));
        }
        let link_as_named = RefusedGraphLinkOnTheWire {
            source: GraphLinkEndOnTheWire {
                node: source.node.clone(),
                port: source.port.clone(),
            },
            target: GraphLinkEndOnTheWire {
                node: target.node.clone(),
                port: target.port.clone(),
            },
        };
        let refused_as_connect_refuses =
            |code_and_place: RefusalCodeAndPlace, runtime_words: String| {
                LinkCheckResult::of_a_link_connect_refuses(the_refusal_connect_answers(
                    &link_as_named,
                    code_and_place,
                    runtime_words,
                ))
            };

        let listings = self.operations_on_the_loaded_streams.list_streams();
        let stream_id = match StreamsTheRuntimeHolds::listed_in(&listings).the_stream_named(&stream)
        {
            Ok((_, stream_id)) => stream_id,
            Err(runtime_words) => {
                return Ok(refused_as_connect_refuses(
                    RefusalCodeAndPlace::coded(ControlOperationRefusalCode::StreamNotFound),
                    runtime_words,
                ));
            }
        };
        let link = match the_link_between(&source, &target) {
            Ok(link) => link,
            Err((node, runtime_words)) => {
                return Ok(refused_as_connect_refuses(
                    RefusalCodeAndPlace {
                        refused_node: Some(node),
                        ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotFound)
                    },
                    runtime_words,
                ));
            }
        };
        let stream_operations = self
            .operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(&stream_id)
            .map_err(|refusal| {
                ControlOperationRefusal::of_an_engine_error(method_name, &refusal)
            })?;
        let graph =
            StreamGraphDocument::from_value(stream_operations.to_json_async().await.map_err(
                |refusal| ControlOperationRefusal::of_an_engine_error(method_name, &refusal),
            )?)
            .map_err(|why| {
                ControlOperationRefusal::runtime_failed(format!(
                    "{method_name} failed: stream `{stream_id}`'s graph does not read as a graph \
                 document: {why}"
                ))
            })?;
        if graph.links().contains(&link) {
            return Ok(refused_as_connect_refuses(
                RefusalCodeAndPlace::coded(ControlOperationRefusalCode::Refused),
                the_words_of_a_link_the_graph_already_holds(&link),
            ));
        }
        match stream_operations.what_a_live_connect_makes_of_the_link(&link) {
            Ok(WhatALiveConnectMakesOfALink::Makes(verdict)) => {
                Ok(LinkCheckResult::of_a_link_connect_makes(verdict.as_ref()))
            }
            Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade) => {
                Err(ControlOperationRefusal::failed(format!(
                    "{method_name} failed: the link reaches a node that runs as no node of its \
                     own, such as a fragment, and such a link is not checked yet"
                )))
            }
            Err(refusal) => Ok(refused_as_connect_refuses(
                RefusalCodeAndPlace::of_an_engine_error(&refusal),
                refusal.to_string(),
            )),
        }
    }
}

/// The link from `source` to `target`, each node by its UUID; the node and the words of why
/// when one names no UUID, as `connect` words it.
fn the_link_between(
    source: &StreamGraphActionLinkEnd,
    target: &StreamGraphActionLinkEnd,
) -> Result<StreamGraphDocumentLink, (String, String)> {
    let the_end = |end: &StreamGraphActionLinkEnd| -> Result<_, (String, String)> {
        let node = Uuid::parse_str(&end.node).map_err(|_| {
            (
                end.node.clone(),
                format!(
                    "`{}` is neither a node's UUID nor the `as` of a node an earlier `add_node` \
                     of this transaction adds",
                    end.node
                ),
            )
        })?;
        Ok(StreamGraphDocumentLinkEnd {
            node,
            port: end.port.clone(),
        })
    };
    Ok(StreamGraphDocumentLink {
        source: the_end(source)?,
        target: the_end(target)?,
    })
}

/// Why `connect` refuses `link` when the stream's graph already holds it, in its words.
fn the_words_of_a_link_the_graph_already_holds(link: &StreamGraphDocumentLink) -> String {
    format!(
        "the graph already links {}/{} -> {}/{}",
        link.source.node, link.source.port, link.target.node, link.target.port
    )
}

/// The refusal `connect` answers for its one operation, the link `link_as_named`, coded and
/// placed by `code_and_place`: the link as its caller named it, and the link's target node as
/// its caller named it when the input takes no wire.
fn the_refusal_connect_answers(
    link_as_named: &RefusedGraphLinkOnTheWire,
    mut code_and_place: RefusalCodeAndPlace,
    runtime_words: String,
) -> ProjectActionRefusalOnTheWire {
    if code_and_place.refusal_code == ControlOperationRefusalCode::InputTakesNoWire {
        code_and_place.refused_node = Some(link_as_named.target.node.clone());
    }
    ProjectActionRefusalOnTheWire {
        refused_operation_index: Some(0),
        refused_link: Some(link_as_named.clone()),
        ..code_and_place.on_the_refusal_of(Connect::METHOD_NAME, runtime_words)
    }
}
