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

        // A resend of a `connect` is answered with the link the first send
        // made. Kept on the link rather than in a registry beside the graph, so
        // a link that goes takes its request id with it and there is nothing to
        // forget. A `disconnect` needs no such record — it is idempotent by what
        // it asks for, which the arm below relies on.
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
                // A link this runtime does not hold is the answer, not a
                // refusal: what was asked for is already true. That is what
                // makes a resend safe here — the reply to the first send can go
                // missing exactly as a `connect`'s can, and refusing the second
                // would latch `refused` forever on a disconnect that worked.
                if !this_runtime_holds_the_link(&runtime, &link_id) {
                    return Ok(WhatALinkRequestWasAnswered {
                        link_id,
                        state: crate::core::json_schema::LinkStateOutput::Disconnected,
                    });
                }
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

/// Whether this runtime's graph still holds the link `link_id` names.
fn this_runtime_holds_the_link(runtime: &Runner, link_id: &LinkUniqueId) -> bool {
    runtime
        .compiler
        .scope(|graph, _tx| graph.traversal().e(link_id).first().is_some())
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
            graph.traversal().e(link_id).first().map(|link| {
                LinkOutput::of_a_link_on_the_runtime_named(link, runtime.runtime_name.as_str())
                    .state
            })
        })
        .unwrap_or(crate::core::json_schema::LinkStateOutput::Disconnected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{
        PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };
    use crate::core::graph::{LinkRequestUniqueId, MeshPortAddress};
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
    use crate::core::runtime::RuntimeMeshConfiguration;
    use serial_test::serial;

    const THE_TEST_TYPE: &str = "link_requests_applied_tests:ADestination";
    const THE_DESTINATIONS_DISPLAY_NAME: &str = "DisplayWindow";
    const THE_INPUT_PORT: &str = "frames_from_upstream";

    fn register_the_destination_type() -> ProcessorClassImportPath {
        static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
        let import_path = ProcessorClassImportPath::new(THE_TEST_TYPE).expect("a legal path");
        REGISTERED_ONCE_PER_PROCESS.call_once(|| {
            let mut descriptor = ProcessorDescriptor::new(
                ProcessorClassShortName::new("ADestination").expect("a legal short name"),
                import_path.clone(),
                "a destination for a link request to land on",
            );
            descriptor.inputs.push(
                PortDescriptor::new(THE_INPUT_PORT, "input", true).with_delivery_profile("newest"),
            );
            let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
        });
        import_path
    }

    /// A runtime isolated on a mesh of its own, holding one processor to push
    /// into.
    ///
    /// Isolated rather than off the network — every runtime is on a mesh — and
    /// on its own one with discovery off, so it reaches no peer and no peer
    /// reaches it. That matters because applying a request needs a graph and
    /// this runtime's own `connect`, and nothing here should depend on what
    /// else happens to be running on the machine.
    fn a_runtime_holding_a_destination() -> Arc<Runner> {
        let import_path = register_the_destination_type();
        let runtime = Runner::new_with_runtime_mesh_configuration(RuntimeMeshConfiguration {
            runtime_name: Some("link-request-apply-under-test".to_string()),
            mesh_name: Some(format!("lr-apply-{}", std::process::id())),
            mesh_multicast_discovery: Some(false),
            ..Default::default()
        })
        .expect("a runtime is constructed");
        let mut spec = ProcessorSpec::new(import_path, serde_json::Value::Null);
        spec.display_name = Some(THE_DESTINATIONS_DISPLAY_NAME.to_string());
        runtime
            .add_processor(spec)
            .expect("the destination is added");
        runtime
    }

    fn a_request_from(link_request_id: &str) -> ALinkRequestOnTheMesh {
        ALinkRequestOnTheMesh::asking_for_a_link(
            LinkRequestUniqueId::from(link_request_id),
            MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video")
                .expect("a legal address"),
            MeshPortAddress::new(
                "link-request-apply-under-test",
                THE_DESTINATIONS_DISPLAY_NAME,
                THE_INPUT_PORT,
            )
            .expect("a legal address"),
            "bench-cam-a1b2",
        )
    }

    fn how_many_links(runtime: &Runner) -> usize {
        runtime
            .compiler
            .scope(|graph, _tx| graph.traversal().e(()).iter().count())
    }

    /// A request sent twice makes one link, and the second send is answered
    /// with the one the first made.
    ///
    /// This is what makes a resend safe, and a resend is not optional: a
    /// request rides at `Drop` and so does its reply, so either can go missing
    /// with nothing said and the requester has no way to tell that from a
    /// runtime that never got it.
    ///
    /// Driven by calling the seam twice rather than by losing a real reply:
    /// Zenoh offers no way to drop one, and what has to hold is that the
    /// *second arrival* of one id makes no second link.
    #[test]
    #[serial]
    fn a_request_that_arrives_twice_makes_one_link_and_answers_with_it_both_times() {
        let runtime = a_runtime_holding_a_destination();
        let applies_them = LinkRequestsAppliedIntoThisRuntimesGraph::of(&runtime);
        let request = a_request_from("LRarrives-twice");

        let first = applies_them
            .answer_one_link_request(&request)
            .expect("the first arrival applies the link");
        assert_eq!(how_many_links(&runtime), 1);

        let second = applies_them
            .answer_one_link_request(&request)
            .expect("the second arrival is answered rather than refused");
        assert_eq!(
            how_many_links(&runtime),
            1,
            "a resend must not make a second link"
        );
        assert_eq!(
            first.link_id, second.link_id,
            "a resend is answered with the link the first send made"
        );

        runtime.stop().expect("the runtime stops");
    }

    /// A `disconnect` that arrives twice is answered both times, because what
    /// it asks for is already true the second time.
    ///
    /// Mental-revert: let the second arrival fall through to `disconnect`, which
    /// answers `Link 'X' not found`. That is a decodable refusal, so the
    /// requester latches `refused` forever — on a disconnect that worked. A
    /// lost reply is the design's own premise, so this is reachable rather than
    /// theoretical.
    #[test]
    #[serial]
    fn a_disconnect_that_arrives_twice_is_answered_both_times() {
        let runtime = a_runtime_holding_a_destination();
        let applies_them = LinkRequestsAppliedIntoThisRuntimesGraph::of(&runtime);
        let applied = applies_them
            .answer_one_link_request(&a_request_from("LRto-be-removed"))
            .expect("the link is applied");

        let removing = ALinkRequestOnTheMesh::asking_for_a_link_to_go(
            LinkRequestUniqueId::from("LRremoving"),
            applied.link_id.clone(),
            "bench-cam-a1b2",
        );
        applies_them
            .answer_one_link_request(&removing)
            .expect("the first arrival removes the link");

        // The link is marked for deletion rather than gone — this runtime was
        // never started, so no compile has run — which is exactly the state a
        // resend must still be answered in.
        let second = applies_them
            .answer_one_link_request(&removing)
            .expect("a resend of a disconnect is answered, never refused");
        assert_eq!(second.link_id, applied.link_id);

        runtime.stop().expect("the runtime stops");
    }

    /// A `disconnect` naming a link this runtime never held is answered too:
    /// the asked-for state is reached, whoever reached it.
    #[test]
    #[serial]
    fn a_disconnect_naming_a_link_this_runtime_does_not_hold_is_answered() {
        let runtime = a_runtime_holding_a_destination();
        let applies_them = LinkRequestsAppliedIntoThisRuntimesGraph::of(&runtime);

        let answered = applies_them
            .answer_one_link_request(&ALinkRequestOnTheMesh::asking_for_a_link_to_go(
                LinkRequestUniqueId::from("LRnever-here"),
                LinkUniqueId::from("Lnot-a-link-here".to_string()),
                "bench-cam-a1b2",
            ))
            .expect("a link that is not here is the answer, not a refusal");
        assert_eq!(
            answered.state,
            crate::core::json_schema::LinkStateOutput::Disconnected
        );

        runtime.stop().expect("the runtime stops");
    }

    /// Two different requests for the same pair of ports are two links: the id
    /// is what makes a resend idempotent, not the ports it names.
    ///
    /// Mental-revert: key the idempotence on the addresses and a runtime that
    /// genuinely wants a second link from one port to another cannot have one.
    #[test]
    #[serial]
    fn two_requests_naming_the_same_ports_are_two_links() {
        let runtime = a_runtime_holding_a_destination();
        let applies_them = LinkRequestsAppliedIntoThisRuntimesGraph::of(&runtime);

        applies_them
            .answer_one_link_request(&a_request_from("LRone"))
            .expect("the first request applies");
        applies_them
            .answer_one_link_request(&a_request_from("LRanother"))
            .expect("the second request applies");

        assert_eq!(how_many_links(&runtime), 2);
        runtime.stop().expect("the runtime stops");
    }

    /// The applied link names the runtime that asked, not the one that applied
    /// it — which is what `graph` renders as `created_by_runtime_name`.
    #[test]
    #[serial]
    fn an_applied_link_names_the_runtime_that_asked_for_it() {
        let runtime = a_runtime_holding_a_destination();
        let applies_them = LinkRequestsAppliedIntoThisRuntimesGraph::of(&runtime);
        let applied = applies_them
            .answer_one_link_request(&a_request_from("LRnames-its-asker"))
            .expect("the request applies");

        let rendered = runtime.compiler.scope(|graph, _tx| {
            LinkOutput::of_a_link_on_the_runtime_named(
                graph
                    .traversal()
                    .e(&applied.link_id)
                    .first()
                    .expect("the link is in the graph"),
                runtime.runtime_name.as_str(),
            )
        });
        assert_eq!(rendered.created_by_runtime_name, "bench-cam-a1b2");

        runtime.stop().expect("the runtime stops");
    }
}
