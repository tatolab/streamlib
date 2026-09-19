// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Answering the runtimes that ask this one for a link, and asking another.
//!
//! The runtime that owns an input applies every link into it, so this is where
//! a push and a third-party wiring land: a query under this runtime's own
//! `@link-requests` key, applied through the same `connect` and the same
//! refusals a link this runtime wired itself would meet.
//!
//! The callback only hands off. It runs on the link's receive loop, and
//! applying takes the graph lock a compile holds — which waits for a helper to
//! report ready — so answering inline would stall every key arriving from that
//! peer and, past the lease, expire the link.
//!
//! **A refusal and silence are not the same answer and do not look different on
//! the wire.** Zenoh answers a query that timed out with an error reply carrying
//! the string `Timeout`, delivered through the same callback a real `reply_err`
//! uses, so `result().is_err()` cannot mean "refused". A refusal is therefore a
//! msgpack document sent at `APPLICATION_OCTET_STREAM`, and only an error reply
//! whose payload decodes as one is read as a refusal. Everything else is
//! silence, which the requester resends against.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use zenoh::Wait;
use zenoh::bytes::Encoding;
use zenoh::qos::{CongestionControl, Priority};
use zenoh::query::ConsolidationMode;

use crate::core::runtime::mesh::link_request_on_the_mesh::{
    ALinkRequestOnTheMesh, WhatALinkRequestWasAnswered, WhyALinkRequestWasRefused,
};
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;

/// How long a runtime has to answer a link request before the asking side
/// gives up and resends on its next pass.
///
/// Longer than the offered-ports bound because applying takes the graph lock a
/// compile holds, and a compile waits for a helper to report ready. Still far
/// under Zenoh's own ten-second default, which nobody here would have chosen.
/// Engine-chosen; nothing authorable.
const HOW_LONG_A_RUNTIME_HAS_TO_ANSWER_A_LINK_REQUEST: Duration = Duration::from_secs(5);

/// How many unanswered link requests this runtime holds before it drops one.
///
/// Deep enough that every runtime on a mesh may ask at once while a compile
/// holds the graph lock; shallow enough that a peer asking in a loop cannot
/// grow this runtime's memory. A dropped request is a request with no reply,
/// which the asking side already reads as silence and resends.
/// Engine-chosen; nothing authorable.
const HOW_MANY_UNANSWERED_LINK_REQUESTS_ARE_HELD: usize = 64;

/// The priority a link request and its reply ride at.
///
/// Above `Data` and `DataLow`, the two rungs the mesh's own egresses use, so a
/// 1080p frame never delays a link request. Zenoh's `Control` rung is not
/// public — it is reserved for Zenoh's own declarations — and `InteractiveLow`
/// is the lowest public rung above every data one.
const WHAT_A_LINK_REQUEST_RIDES_AT: Priority = Priority::InteractiveLow;

/// How this runtime answers another that asks it for a link.
///
/// The mesh joins in `Runner::new()` before the runtime exists to apply
/// anything, so this arrives afterwards — the shape
/// [`WhatThisRuntimeOffersOnTheMesh`] already uses for something the mesh
/// learns after it is on the network.
///
/// [`WhatThisRuntimeOffersOnTheMesh`]:
///     crate::core::runtime::mesh::WhatThisRuntimeOffersOnTheMesh
pub trait WhatThisRuntimeDoesWithALinkRequest: Send + Sync {
    /// Apply what `request` asks for, and say what this runtime now holds — or
    /// why it will not.
    ///
    /// The `Err` is the refusal a peer reads, so it is written for that peer
    /// rather than for a log.
    fn answer_one_link_request(
        &self,
        request: &ALinkRequestOnTheMesh,
    ) -> std::result::Result<WhatALinkRequestWasAnswered, String>;
}

/// The seam through which the mesh applies a link request, filled in once the
/// runtime exists to apply one.
///
/// Empty until then, which refuses a request saying so rather than dropping it
/// — a runtime still coming up genuinely cannot apply a link yet, and a
/// requester that is told so resends.
#[derive(Default)]
pub struct WhatThisRuntimeDoesWithALinkRequestRegistry {
    applies_them: Mutex<Option<Arc<dyn WhatThisRuntimeDoesWithALinkRequest>>>,
}

impl WhatThisRuntimeDoesWithALinkRequestRegistry {
    /// Record how this runtime applies a link request.
    pub fn record_how_this_runtime_applies_them(
        &self,
        applies_them: Arc<dyn WhatThisRuntimeDoesWithALinkRequest>,
    ) {
        *self.applies_them.lock() = Some(applies_them);
    }

    /// Apply one request, or say why not.
    ///
    /// The applier is cloned out from under the lock before it is called: it
    /// takes the graph lock a compile holds, and holding this one across that
    /// would queue every other request behind a compile.
    fn answer_one_link_request(
        &self,
        request: &ALinkRequestOnTheMesh,
    ) -> std::result::Result<WhatALinkRequestWasAnswered, String> {
        let applies_them = self.applies_them.lock().clone();
        match applies_them {
            Some(applies_them) => applies_them.answer_one_link_request(request),
            None => Err(
                "this runtime is still starting and has no graph to apply a link into yet"
                    .to_string(),
            ),
        }
    }
}

/// The queryable that answers link requests, and the thread that answers them.
pub(super) struct LinkRequestsFromOtherRuntimesQueryable {
    queryable: Option<zenoh::query::Queryable<()>>,
    answering_thread: Option<std::thread::JoinHandle<()>>,
}

impl LinkRequestsFromOtherRuntimesQueryable {
    /// Declare the queryable and start the thread that answers it.
    pub(super) fn declare(
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        applies_them: &Arc<WhatThisRuntimeDoesWithALinkRequestRegistry>,
    ) -> zenoh::Result<Self> {
        let answered_key = key_space.link_requests_key_of(this_runtimes_name);
        let (asked, what_the_answering_thread_reads) =
            crossbeam_channel::bounded::<zenoh::query::Query>(
                HOW_MANY_UNANSWERED_LINK_REQUESTS_ARE_HELD,
            );
        let queryable = session
            .declare_queryable(answered_key.clone())
            .callback(move |query| {
                if asked.try_send(query).is_err() {
                    tracing::warn!(
                        "this runtime is being asked for links faster than it can apply them; \
                         the asking runtime reads no reply as silence and asks again"
                    );
                }
            })
            .wait()?;

        let applies_them = Arc::clone(applies_them);
        let this_runtimes_name = this_runtimes_name.to_string();
        let answering_thread = std::thread::Builder::new()
            .name("streamlib-mesh-link-requests".to_string())
            .spawn(move || {
                // Ends when the queryable is dropped, which drops the sender.
                while let Ok(query) = what_the_answering_thread_reads.recv() {
                    answer_one_query(&query, &answered_key, &this_runtimes_name, &applies_them);
                }
            })
            .map_err(|cannot_spawn| -> zenoh::Error { Box::new(cannot_spawn) })?;

        Ok(Self {
            queryable: Some(queryable),
            answering_thread: Some(answering_thread),
        })
    }
}

impl Drop for LinkRequestsFromOtherRuntimesQueryable {
    fn drop(&mut self) {
        // The queryable first: dropping it drops the callback that holds the
        // answering thread's sender, which is what ends that thread.
        drop(self.queryable.take());
        if let Some(answering_thread) = self.answering_thread.take()
            && answering_thread.join().is_err()
        {
            tracing::warn!(
                "the thread answering this runtime's link requests panicked; no other runtime \
                 can wire a link into this one until it restarts"
            );
        }
    }
}

/// Answer one peer asking this runtime for a link.
fn answer_one_query(
    query: &zenoh::query::Query,
    answered_key: &str,
    this_runtimes_name: &str,
    applies_them: &WhatThisRuntimeDoesWithALinkRequestRegistry,
) {
    let Some(asked) = query.payload() else {
        refuse(
            query,
            this_runtimes_name,
            "a link request carries its document as the query's payload, and this query carried \
             none",
        );
        return;
    };
    let request = match ALinkRequestOnTheMesh::decode(&asked.to_bytes()) {
        Ok(request) => request,
        Err(unreadable) => {
            refuse(
                query,
                this_runtimes_name,
                format!("this engine cannot read that link request: {unreadable}"),
            );
            return;
        }
    };

    match applies_them.answer_one_link_request(&request) {
        Ok(answered) => match answered.encode() {
            Ok(wire_bytes) => {
                if let Err(reply_failure) = query.reply(answered_key, wire_bytes).wait() {
                    tracing::debug!(
                        "this runtime applied the link request {} and the answer did not reach \
                         the runtime that asked: {reply_failure}",
                        request.link_request_id
                    );
                }
            }
            Err(encode_failure) => refuse(
                query,
                this_runtimes_name,
                format!(
                    "this runtime applied the link and could not say so: {encode_failure}. Read \
                     its `graph` to find the link."
                ),
            ),
        },
        Err(reason) => refuse(query, this_runtimes_name, reason),
    }
}

/// Refuse one request in this runtime's own words.
///
/// At `APPLICATION_OCTET_STREAM` because the payload is msgpack — and because
/// the timed-out query Zenoh synthesises carries `ZENOH_STRING`, so the
/// encoding is the cheap half of telling a refusal from silence.
fn refuse(query: &zenoh::query::Query, this_runtimes_name: &str, reason: impl Into<String>) {
    let refused = WhyALinkRequestWasRefused::from_the_runtime_named(this_runtimes_name, reason);
    let Ok(wire_bytes) = refused.encode() else {
        tracing::warn!(
            "this runtime refused a link request and could not encode the refusal, so the \
             runtime that asked reads silence: {}",
            refused.reason
        );
        return;
    };
    if let Err(reply_failure) = query
        .reply_err(wire_bytes)
        .encoding(Encoding::APPLICATION_OCTET_STREAM)
        .wait()
    {
        tracing::debug!(
            "this runtime refused a link request and the refusal did not reach the runtime that \
             asked: {reply_failure}"
        );
    }
}

/// What came back from asking a runtime for a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HowARuntimeAnsweredALinkRequest {
    /// It applied the request and named the link it now holds.
    ItAppliedTheRequest(WhatALinkRequestWasAnswered),
    /// It refused, in its own words. Final: a resend would be refused again.
    ItRefusedTheRequest(WhyALinkRequestWasRefused),
    /// Nothing readable came back. Not a refusal — the request, the reply, or
    /// both may simply have been dropped — so the requester keeps it and
    /// resends.
    ItSaidNothing {
        /// What silence looked like from here.
        reason: String,
    },
}

/// Ask the runtime named `answering_runtime_name` to apply `request`.
///
/// Sent at `Drop` so a full queue never parks the caller, which means the
/// request or its reply can go missing with no error — the resend above the
/// caller is what covers that. `ConsolidationMode::None` because the default
/// buffers a successful reply until the query closes, which would add a round
/// trip to every link that worked.
///
/// Blocks on the network while it waits for the reply, so it runs on a thread
/// the mesh owns and never on an app's or a Zenoh callback's.
pub(super) fn ask_a_runtime_to_apply_a_link_request(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    answering_runtime_name: &str,
    request: &ALinkRequestOnTheMesh,
) -> HowARuntimeAnsweredALinkRequest {
    let wire_bytes = match request.encode() {
        Ok(wire_bytes) => wire_bytes,
        Err(encode_failure) => {
            return HowARuntimeAnsweredALinkRequest::ItSaidNothing {
                reason: format!(
                    "this runtime could not encode its own link request: {encode_failure}"
                ),
            };
        }
    };
    let answers = match session
        .get(key_space.link_requests_key_of(answering_runtime_name))
        .payload(wire_bytes)
        .encoding(Encoding::APPLICATION_OCTET_STREAM)
        .congestion_control(CongestionControl::Drop)
        .priority(WHAT_A_LINK_REQUEST_RIDES_AT)
        .consolidation(ConsolidationMode::None)
        .timeout(HOW_LONG_A_RUNTIME_HAS_TO_ANSWER_A_LINK_REQUEST)
        .wait()
    {
        Ok(answers) => answers,
        Err(query_failure) => {
            return HowARuntimeAnsweredALinkRequest::ItSaidNothing {
                reason: format!(
                    "the link request could not be sent to {answering_runtime_name}: \
                     {query_failure}"
                ),
            };
        }
    };

    let mut what_silence_looked_like =
        format!("the runtime {answering_runtime_name} did not answer");
    for reply in answers {
        match reply.result() {
            Ok(answered) => {
                match WhatALinkRequestWasAnswered::decode(&answered.payload().to_bytes()) {
                    Ok(answered) => {
                        return HowARuntimeAnsweredALinkRequest::ItAppliedTheRequest(answered);
                    }
                    Err(unreadable) => {
                        what_silence_looked_like = format!(
                            "the runtime {answering_runtime_name} answered in a form this engine \
                             cannot read: {unreadable}"
                        );
                    }
                }
            }
            // An error reply is a refusal only if it carries one. Zenoh
            // synthesises a timed-out query as an error reply too, and reading
            // that as a refusal would make a network hiccup permanent.
            Err(reply_error) => {
                match WhyALinkRequestWasRefused::decode(&reply_error.payload().to_bytes()) {
                    Some(refused) => {
                        return HowARuntimeAnsweredALinkRequest::ItRefusedTheRequest(refused);
                    }
                    None => {
                        what_silence_looked_like = format!(
                            "the runtime {answering_runtime_name} did not answer within \
                             {HOW_LONG_A_RUNTIME_HAS_TO_ANSWER_A_LINK_REQUEST:?}"
                        );
                    }
                }
            }
        }
    }
    HowARuntimeAnsweredALinkRequest::ItSaidNothing {
        reason: what_silence_looked_like,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::{LinkRequestUniqueId, LinkUniqueId, MeshPortAddress};
    use crate::core::json_schema::LinkStateOutput;

    fn a_request() -> ALinkRequestOnTheMesh {
        ALinkRequestOnTheMesh::asking_for_a_link(
            LinkRequestUniqueId::from("LRabc123"),
            MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video")
                .expect("a legal address"),
            MeshPortAddress::new("studio-display-9f3c", "DisplayWindow", "video")
                .expect("a legal address"),
            "bench-cam-a1b2",
        )
    }

    /// A runtime that has not finished starting refuses rather than dropping
    /// the request, so the runtime that asked learns to ask again.
    #[test]
    fn a_registry_nobody_has_filled_refuses_saying_the_runtime_is_still_starting() {
        let refusal = WhatThisRuntimeDoesWithALinkRequestRegistry::default()
            .answer_one_link_request(&a_request())
            .expect_err("a runtime with no graph cannot apply a link");
        assert!(refusal.contains("still starting"), "{refusal}");
    }

    /// Whatever the applier answers is what the peer gets, in both directions.
    #[test]
    fn a_filled_registry_answers_with_whatever_this_runtime_did() {
        struct ApplyingEveryRequestAsOneLink(LinkUniqueId);
        impl WhatThisRuntimeDoesWithALinkRequest for ApplyingEveryRequestAsOneLink {
            fn answer_one_link_request(
                &self,
                _request: &ALinkRequestOnTheMesh,
            ) -> std::result::Result<WhatALinkRequestWasAnswered, String> {
                Ok(WhatALinkRequestWasAnswered {
                    link_id: self.0.clone(),
                    state: LinkStateOutput::AwaitingRemote,
                })
            }
        }

        let registry = WhatThisRuntimeDoesWithALinkRequestRegistry::default();
        registry.record_how_this_runtime_applies_them(Arc::new(ApplyingEveryRequestAsOneLink(
            LinkUniqueId::from("Labc".to_string()),
        )));
        assert_eq!(
            registry
                .answer_one_link_request(&a_request())
                .expect("the applier applied it")
                .link_id,
            LinkUniqueId::from("Labc".to_string())
        );
    }
}
