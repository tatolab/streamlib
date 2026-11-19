// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link`: what `connect` would answer for one link, judged against the stream's running
//! graph, or the placed stream's level it is inside, by the engine's own judgement of a
//! connect, and made nowhere.

use std::sync::Arc;

use streamlib::sdk::runtime::WhatALiveConnectMakesOfALink;
use streamlib_control_contract::{CheckLink, CheckLinkRequest, Connect, ControlOperation};
use streamlib_project_repository::StreamGraphOperation;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, LinkCheckResult,
};

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::refusal_code_and_place::RefusalCodeAndPlace;
use crate::stream_graph_transaction_executor::{
    GraphRequestRefusal, GraphTransactionRefusal, StreamsTheRuntimeHolds,
    the_operation_a_connect_asks_inside_a_placed_stream,
    the_operation_a_connect_asks_of_the_running_stream,
};

impl ControlOperationDispatcher {
    /// What `connect` would answer for the link `request` names: the stream it names resolved
    /// as a graph edit resolves it, the link read as a transaction's `connect` reads it, and
    /// the rest judged by the engine's own judgement of a live connect. Neither the graph nor
    /// the project's history is changed, and no refusal is kept on the project's change feed.
    ///
    /// A link inside a placed stream, under a non-empty `level`, is read from the project's
    /// documents as a transaction's `connect` reads it, and judged among that level's nodes by
    /// the engine's own judgement of a link there.
    ///
    /// Links `connect` makes without judging them are refused unchecked, in the words of why:
    /// one reaching a node that runs as no node of its own, which the live graph judges only as
    /// it is reconciled to a document holding the link, and, inside a placed stream, one
    /// reaching a node not built to judge it, such as one whose type is not described. So is one
    /// closing a loop of function nodes no `PreviousValue` node is in, which `connect` closes
    /// through a `PreviousValue` node it adds.
    pub(crate) async fn check_link(
        &self,
        CheckLinkRequest {
            stream,
            level,
            source,
            target,
        }: CheckLinkRequest,
    ) -> Result<LinkCheckResult, ControlOperationRefusal> {
        let not_checked_yet = |why: &str| {
            ControlOperationRefusal::failed(format!(
                "{} failed: {why}, and such a link is not checked yet",
                CheckLink::METHOD_NAME
            ))
        };
        let of_an_engine_error = |refusal: streamlib::sdk::error::Error| {
            ControlOperationRefusal::of_an_engine_error(CheckLink::METHOD_NAME, &refusal)
        };
        let refused_as_connect_refuses = |refusal: GraphRequestRefusal| {
            LinkCheckResult::of_a_link_connect_refuses(
                GraphTransactionRefusal::of_a_connect(&source, &target, refusal)
                    .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
            )
        };
        let listings = self.operations_on_the_loaded_streams.list_streams();
        let streams_the_runtime_holds = StreamsTheRuntimeHolds::listed_in(&listings);
        let (stream_asset, stream_id) = match streams_the_runtime_holds.the_stream_named(&stream) {
            Ok(named) => named,
            Err(runtime_words) => {
                return Ok(refused_as_connect_refuses(GraphRequestRefusal::coded(
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
        let level = level.unwrap_or_default();
        let placed_level_and_operation = if level.is_empty() {
            the_operation_a_connect_asks_of_the_running_stream(
                source.clone(),
                target.clone(),
                &rendered_graph,
                stream_operations.as_ref(),
            )
            .map(|operation| (None, operation))
        } else {
            let project_directory = streams_the_runtime_holds
                .the_location_of(&stream_id)
                .map(|stream_location| stream_location.project_directory.clone());
            let project_repositories = Arc::clone(&self.project_repositories);
            let stream_operations = Arc::clone(&stream_operations);
            let (source, target) = (source.clone(), target.clone());
            tokio::task::spawn_blocking(move || {
                let project_directory = project_directory.ok_or_else(|| {
                    GraphRequestRefusal::from(format!(
                        "stream `{stream_id}` is of no project, so no placed stream inside it is \
                         read from a document"
                    ))
                })?;
                let project_repository =
                    project_repositories.the_project_repository_at(&project_directory)?;
                the_operation_a_connect_asks_inside_a_placed_stream(
                    source,
                    target,
                    stream_asset,
                    &level,
                    &rendered_graph,
                    stream_operations.as_ref(),
                    &project_repository.lock(),
                )
                .map(|(level_document, operation)| (Some(level_document), operation))
            })
            .await
            .map_err(ControlOperationRefusal::the_task_did_not_finish(format!(
                "{} failed: the read of the placed stream",
                CheckLink::METHOD_NAME
            )))?
        };
        let (placed_level, operation) = match placed_level_and_operation {
            Ok(placed_level_and_operation) => placed_level_and_operation,
            Err(refusal) => return Ok(refused_as_connect_refuses(refusal)),
        };
        let StreamGraphOperation::Connect { link } = operation else {
            return Err(not_checked_yet(
                "the link closes a loop of function nodes no `PreviousValue` node is in, which \
                 `connect` closes through a `PreviousValue` node it adds",
            ));
        };
        let what_a_connect_makes = match &placed_level {
            None => stream_operations.what_a_live_connect_makes_of_the_link(&link),
            Some(level_document) => stream_operations
                .what_a_connect_makes_of_a_link_at_a_placed_level(level_document, &link),
        };
        match what_a_connect_makes {
            Ok(WhatALiveConnectMakesOfALink::Makes(verdict)) => {
                Ok(LinkCheckResult::of_a_link_connect_makes(verdict.as_deref()))
            }
            Ok(made_unjudged) => Err(not_checked_yet(
                &made_unjudged
                    .why_it_makes_the_link_unjudged()
                    .unwrap_or_default(),
            )),
            Err(refusal) => Ok(refused_as_connect_refuses(refusal.into())),
        }
    }
}
