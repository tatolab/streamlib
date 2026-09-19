// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How this runtime applies a link another runtime asks it for.
//!
//! The mesh joins in `Runner::new()` before the runtime exists, so it never
//! holds one: it holds this, which the runtime records once it does — the shape
//! [`OutputPortsInThisRuntimesGraph`] already uses for the other direction.
//!
//! Applying goes through `connect` and `disconnect` themselves, not past them.
//! A request therefore meets every refusal a link this runtime wired itself
//! would meet, in the same words, and a destination spelled as this runtime's
//! own mesh address resolves to the local processor exactly as one an app
//! spelled would.
//!
//! The runtime is held weakly on purpose. It holds the mesh, which holds the
//! queryable, which holds this; a strong handle back would close the ring and
//! leave the runtime and its iceoryx2 node behind at exit.
//!
//! [`OutputPortsInThisRuntimesGraph`]: super::OutputPortsInThisRuntimesGraph

use std::sync::{Arc, Weak};

use crate::core::graph::{
    GraphEdgeWithComponents, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef,
    TheRequestThatAppliedThisLinkComponent,
};
use crate::core::json_schema::LinkOutput;
use crate::core::runtime::Runner;
use crate::core::runtime::mesh::{
    ALinkRequestOnTheMesh, WhatALinkRequestAsksFor, WhatALinkRequestWasAnswered,
    WhatThisRuntimeDoesWithALinkRequest,
};
use crate::core::runtime::operations::RuntimeOperations;

/// This runtime's graph, as a link request reaches it.
pub(crate) struct LinkRequestsAppliedIntoThisRuntimesGraph {
    runtime: Weak<Runner>,
}

impl LinkRequestsAppliedIntoThisRuntimesGraph {
    /// Apply requests into `runtime`'s graph.
    pub(crate) fn of(runtime: &Arc<Runner>) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::downgrade(runtime),
        })
    }
}

impl WhatThisRuntimeDoesWithALinkRequest for LinkRequestsAppliedIntoThisRuntimesGraph {
    fn answer_one_link_request(
        &self,
        request: &ALinkRequestOnTheMesh,
    ) -> std::result::Result<WhatALinkRequestWasAnswered, String> {
        let Some(runtime) = self.runtime.upgrade() else {
            return Err("this runtime is shutting down and is applying no more links".to_string());
        };
        let asked_for = request.what_it_asks_for(env!("CARGO_PKG_VERSION"))?;

        // A resend is answered with the link the first send made. Kept on the
        // link rather than in a registry beside the graph, so a link that goes
        // takes its request id with it and there is nothing to forget.
        if let Some(already_applied) =
            the_link_a_request_already_applied(&runtime, &request.link_request_id)
        {
            return Ok(already_applied);
        }

        match asked_for {
            WhatALinkRequestAsksFor::ApplyingALink {
                source_address,
                destination_address,
            } => {
                // Both ends go in as mesh addresses and `connect` resolves
                // them: the destination names this runtime, so it becomes the
                // local processor its display name labels, and the source stays
                // remote unless it names this runtime too.
                let link_id = runtime
                    .connect(
                        OutputLinkPortRef::on_another_runtime(source_address),
                        InputLinkPortRef::on_another_runtime(destination_address),
                    )
                    .map_err(|refusal| refusal.to_string())?;
                remember_which_request_applied_this_link(&runtime, &link_id, request);
                Ok(WhatALinkRequestWasAnswered {
                    state: how_this_runtime_reads_one_link(&runtime, &link_id),
                    link_id,
                })
            }
            WhatALinkRequestAsksFor::RemovingALink { link_id } => {
                runtime
                    .disconnect(&link_id)
                    .map_err(|refusal| refusal.to_string())?;
                Ok(WhatALinkRequestWasAnswered {
                    state: how_this_runtime_reads_one_link(&runtime, &link_id),
                    link_id,
                })
            }
        }
    }
}

/// The link an earlier send of this request already applied, if it is still
/// here.
fn the_link_a_request_already_applied(
    runtime: &Runner,
    link_request_id: &crate::core::graph::LinkRequestUniqueId,
) -> Option<WhatALinkRequestWasAnswered> {
    let link_id = runtime.compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .e(())
            .iter()
            .find(|link| {
                link.get::<TheRequestThatAppliedThisLinkComponent>()
                    .is_some_and(|applied| &applied.link_request_id == link_request_id)
            })
            .map(|link| link.id.clone())
    })?;
    Some(WhatALinkRequestWasAnswered {
        state: how_this_runtime_reads_one_link(runtime, &link_id),
        link_id,
    })
}

/// Write onto the link which request applied it and which runtime asked, so a
/// resend finds it and `graph` renders its creator.
fn remember_which_request_applied_this_link(
    runtime: &Runner,
    link_id: &LinkUniqueId,
    request: &ALinkRequestOnTheMesh,
) {
    runtime.compiler.scope(|graph, _tx| {
        if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
            link.insert_component_without_rendering_it(
                TheRequestThatAppliedThisLinkComponent::asked_for_by(
                    request.link_request_id.clone(),
                    request.requester_runtime_name.clone(),
                ),
            );
        }
    });
}

/// How this runtime's own `graph` reads one link right now.
///
/// Read off `graph`'s own renderer rather than the link's `state` field, so a
/// requester is told the word a reader of that runtime's `graph` would see.
/// A link the disconnect path has already taken out reads `disconnected`.
fn how_this_runtime_reads_one_link(
    runtime: &Runner,
    link_id: &LinkUniqueId,
) -> crate::core::json_schema::LinkStateOutput {
    runtime
        .compiler
        .scope(|graph, _tx| {
            graph
                .traversal()
                .e(link_id)
                .first()
                .map(|link| {
                    LinkOutput::of_a_link_on_the_runtime_named(link, runtime.runtime_name.as_str())
                        .state
                })
        })
        .unwrap_or(crate::core::json_schema::LinkStateOutput::Disconnected)
}
