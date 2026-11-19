// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link`: what `connect` would answer for one link, judged against the stream's running
//! graph by the engine's own judgement of a connect, and made nowhere.

use std::collections::HashMap;

use streamlib::sdk::runtime::WhatALiveConnectMakesOfALink;
use streamlib_control_contract::{CheckLink, CheckLinkRequest, Connect, ControlOperation};
use streamlib_project_repository::StreamGraphDocument;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, GraphTransactionNodeInsertedBecause, LinkCheckResult,
};

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::refusal_code_and_place::RefusalCodeAndPlace;
use crate::stream_graph_transaction_executor::{
    GraphRequestRefusal, GraphTransactionRefusal, StreamsTheRuntimeHolds, WhatAConnectAsks,
    WhereAnOperationEdits, the_level_holds_both_ends_by_uuid,
    what_a_connect_asks_of_the_running_stream,
};

impl ControlOperationDispatcher {
    /// What `connect` would answer for the link `request` names: the stream it names resolved
    /// as a graph edit resolves it, the link read as a transaction's `connect` reads it, and
    /// the rest judged by the engine's own judgement of a live connect. A link `connect` carries
    /// through the convert node it places answers that node. Neither the graph nor the project's
    /// history is changed, and no refusal is kept on the project's change feed.
    ///
    /// Three links are refused unchecked: one inside a placed stream, under a non-empty
    /// `level`; one closing a loop of function nodes no `PreviousValue` node is in, which
    /// `connect` closes through a `PreviousValue` node it adds; and one reaching a node that
    /// runs as no node of its own, which the live graph judges only as it is reconciled to a
    /// document holding the link.
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
        let refused_as_connect_refuses =
            |edits: Option<WhereAnOperationEdits<'_>>, refusal: GraphRequestRefusal| {
                LinkCheckResult::of_a_link_connect_refuses(
                    GraphTransactionRefusal::of_a_connect(edits, &source, &target, refusal)
                        .into_the_refusal_on_the_wire(Connect::METHOD_NAME),
                )
            };
        if level.is_some_and(|level| !level.is_empty()) {
            return Err(not_checked_yet(
                "the link is inside a placed stream; check one at the stream's own level, with \
                 `level` omitted",
            ));
        }

        let listings = self.operations_on_the_loaded_streams.list_streams();
        let stream_id = match StreamsTheRuntimeHolds::listed_in(&listings).the_stream_named(&stream)
        {
            Ok((_, stream_id)) => stream_id,
            Err(runtime_words) => {
                return Ok(refused_as_connect_refuses(
                    None,
                    GraphRequestRefusal::coded(
                        RefusalCodeAndPlace::coded(ControlOperationRefusalCode::StreamNotFound),
                        runtime_words,
                    ),
                ));
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
        let level_as_drawn = match StreamGraphDocument::from_value(rendered_graph.clone()) {
            Ok(level_as_drawn) => level_as_drawn,
            Err(not_read) => return Ok(refused_as_connect_refuses(None, not_read.into())),
        };
        let at_its_own_level = WhereAnOperationEdits {
            stream_id: &stream_id,
            level: &[],
            level_holds_both_link_ends: the_level_holds_both_ends_by_uuid(
                &level_as_drawn,
                &source,
                &target,
                &HashMap::new(),
            ),
        };
        let asked = match what_a_connect_asks_of_the_running_stream(
            source.clone(),
            target.clone(),
            &rendered_graph,
            &level_as_drawn,
            stream_operations.as_ref(),
        ) {
            Ok(asked) => asked,
            Err(refusal) => return Ok(refused_as_connect_refuses(Some(at_its_own_level), refusal)),
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
            } => {
                return Err(not_checked_yet(
                    "the link closes a loop of function nodes no `PreviousValue` node is in, \
                     which `connect` closes through a `PreviousValue` node it adds",
                ));
            }
        };
        match stream_operations.what_a_live_connect_makes_of_the_link(&link) {
            Ok(WhatALiveConnectMakesOfALink::Makes(verdict)) => {
                Ok(LinkCheckResult::of_a_link_connect_makes(verdict.as_deref()))
            }
            Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade) => Err(not_checked_yet(
                "the link reaches a node that runs as no node of its own, such as a fragment",
            )),
            Err(refusal) => Ok(refused_as_connect_refuses(
                Some(at_its_own_level),
                refusal.into(),
            )),
        }
    }
}
