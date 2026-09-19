// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every link this runtime has asked another runtime to apply, and how far
//! each has got.
//!
//! Asking never waits on the mesh, so this is where the waiting happens: a
//! request lands here the moment it is made and is sent afterwards, when the
//! runtime it names turns up. `graph` reads each one's cell, which is written
//! here and never under the graph lock.
//!
//! A request leaves this table exactly once, when it is applied — the link is
//! then the runtime's to render, on the runtime that owns the input. One that
//! was refused stays, because a refusal is the only word an author who asked
//! from Python ever gets: `connect` returns before the request is sent and
//! never waits, so the refusal has nowhere else to land.
//!
//! Silence is not a refusal. A request sent at `Drop` can go missing, and so
//! can its reply, with nothing said either way — so an unanswered request keeps
//! its place and is sent again on a backoff.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use zenoh::Wait;

use crate::core::error::{Error, Result};
use crate::core::graph::LinkRequestUniqueId;
use crate::core::json_schema::{
    LinkRequestAwaitingARuntimeOutput, LinkRequestOperationOutput, LinkRequestStateOutput,
};
use crate::core::runtime::mesh::link_request_on_the_mesh::ALinkRequestOnTheMesh;
use crate::core::runtime::mesh::link_requests_from_other_runtimes::{
    HowARuntimeAnsweredALinkRequest, ask_a_runtime_to_apply_a_link_request,
};
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::core::runtime::mesh::runtime_mesh_peer_table::RuntimeMeshPeerTable;

/// How often every waiting request is looked at again.
///
/// The pass also runs the moment a runtime's token comes or goes, so this is
/// the floor under a token nobody delivered rather than the cadence sending
/// normally runs at. Engine-chosen; nothing authorable.
const HOW_OFTEN_EVERY_WAITING_REQUEST_IS_LOOKED_AT_AGAIN: Duration = Duration::from_secs(2);

/// How long after an unanswered send the next one is tried, and how far that
/// wait doubles.
///
/// A backoff rather than a fixed retry because an unanswered request is most
/// often a runtime that is busy applying somebody else's, and asking it again
/// immediately makes that worse. Engine-chosen; nothing authorable.
const HOW_SOON_AN_UNANSWERED_REQUEST_IS_SENT_AGAIN: Duration = Duration::from_secs(1);

/// The longest this runtime waits between two sends of one request.
const HOW_LONG_THE_WAIT_BETWEEN_SENDS_GROWS_TO: Duration = Duration::from_secs(30);

/// How many unapplied requests this runtime holds before it drops one.
///
/// The answering side caps itself for the same reason and in the same words: a
/// peer asking in a loop must not grow this runtime's memory, and until the
/// security pass any runtime may ask. The exposure here is an agent driving MCP
/// `connect` at a display name that does not exist, whose refusals are final
/// and would otherwise pile up in `graph` for the life of the runtime.
/// Engine-chosen; nothing authorable.
const HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS: usize = 256;

/// One request this runtime has made and no runtime has applied.
struct ARequestThisRuntimeHasSent {
    request: ALinkRequestOnTheMesh,
    /// Which request this was, counting from the runtime's first. The map is
    /// keyed by a cuid2, which says nothing about order, and the cap below
    /// drops the *oldest* dead record rather than an arbitrary one.
    noted: u64,
    /// The runtime being asked — the one that owns the input.
    input_runtime_name: String,
    how_far_it_has_got: HowFarARequestHasGot,
    /// When the next send is due. Now, for one that has never been sent.
    send_again_at: Instant,
    /// How long the next unanswered send waits before the one after it.
    how_long_to_wait_next: Duration,
}

impl ARequestThisRuntimeHasSent {
    /// Put the next send further out, up to the ceiling.
    fn wait_longer_before_the_next_send(&mut self) {
        self.send_again_at = Instant::now() + self.how_long_to_wait_next;
        self.how_long_to_wait_next =
            (self.how_long_to_wait_next * 2).min(HOW_LONG_THE_WAIT_BETWEEN_SENDS_GROWS_TO);
    }
}

/// How far one request has got.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HowFarARequestHasGot {
    /// The runtime it names is not on the mesh, so it has not been sent.
    AwaitingTheRuntime { reason: String },
    /// It was sent and nothing readable came back.
    Unanswered { reason: String },
    /// The runtime it names refused it, in its own words. Final.
    Refused { reason: String },
}

impl HowFarARequestHasGot {
    fn state(&self) -> LinkRequestStateOutput {
        match self {
            Self::AwaitingTheRuntime { .. } => LinkRequestStateOutput::AwaitingRuntime,
            Self::Unanswered { .. } => LinkRequestStateOutput::Unanswered,
            Self::Refused { .. } => LinkRequestStateOutput::Refused,
        }
    }

    fn reason(&self) -> &str {
        match self {
            Self::AwaitingTheRuntime { reason }
            | Self::Unanswered { reason }
            | Self::Refused { reason } => reason,
        }
    }

    /// Whether this request should be sent again at all. A refusal is final;
    /// a resend would be refused in the same words.
    fn it_is_worth_sending_again(&self) -> bool {
        !matches!(self, Self::Refused { .. })
    }
}

/// Every request this runtime has made and no runtime has applied.
#[derive(Default)]
pub struct LinkRequestsThisRuntimeHasSent {
    waiting: Arc<Mutex<BTreeMap<LinkRequestUniqueId, ARequestThisRuntimeHasSent>>>,
    /// How many requests this runtime has ever noted, which is what orders them.
    noted_so_far: Mutex<u64>,
    /// Set once this runtime is on a mesh and the sending thread is up.
    sending: Mutex<Option<SendingEveryWaitingRequest>>,
}

/// The thread that sends waiting requests, and what wakes it.
struct SendingEveryWaitingRequest {
    whether_to_keep_sending: Arc<AtomicBool>,
    wake_the_sender: Sender<()>,
    announcement_subscriber: zenoh::pubsub::Subscriber<()>,
    sending_thread: std::thread::JoinHandle<()>,
}

impl LinkRequestsThisRuntimeHasSent {
    /// Keep `request` until the runtime it names applies it.
    ///
    /// The reason says nothing about that runtime yet: whether it is absent,
    /// silent or perfectly healthy is not known until the mesh has looked, and
    /// this end may be the one that is off the mesh. The first pass overwrites
    /// it with what it actually found.
    pub fn note_a_request_waiting_to_be_sent(
        &self,
        request: ALinkRequestOnTheMesh,
        input_runtime_name: impl Into<String>,
        mesh_name: &str,
    ) -> Result<()> {
        let input_runtime_name = input_runtime_name.into();
        let reason =
            format!("this request has just been made and the {mesh_name} mesh has not sent it yet");
        {
            let mut waiting = self.waiting.lock();
            if waiting.len() >= HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS {
                forget_the_oldest_refused_request(&mut waiting)?;
            }
            let mut noted_so_far = self.noted_so_far.lock();
            *noted_so_far += 1;
            waiting.insert(
                request.link_request_id.clone(),
                ARequestThisRuntimeHasSent {
                    request,
                    noted: *noted_so_far,
                    input_runtime_name,
                    how_far_it_has_got: HowFarARequestHasGot::AwaitingTheRuntime { reason },
                    send_again_at: Instant::now(),
                    how_long_to_wait_next: HOW_SOON_AN_UNANSWERED_REQUEST_IS_SENT_AGAIN,
                },
            );
        }
        self.ask_the_sender_to_look_again();
        Ok(())
    }

    /// Say why a request will never be sent, for a runtime that is not on its
    /// mesh at all.
    pub fn note_that_this_runtime_reaches_nobody(
        &self,
        link_request_id: &LinkRequestUniqueId,
        reason: String,
    ) {
        if let Some(waiting) = self.waiting.lock().get_mut(link_request_id) {
            waiting.how_far_it_has_got = HowFarARequestHasGot::AwaitingTheRuntime { reason };
        }
    }

    /// Cancel a request, so it is never sent. Answers whether there was one.
    ///
    /// Taking it out of the table is the whole of the cancel: a send already in
    /// flight re-looks-up its own entry before writing anything down, finds it
    /// gone, and discards whatever came back.
    ///
    /// Stated residual: that send may already have reached the runtime that
    /// owns the input, which will have applied the link. A cancel stops this
    /// runtime asking again; it cannot unmake a link another runtime has
    /// already made. `disconnect` naming that link is how it goes.
    pub fn cancel_a_request(&self, link_request_id: &LinkRequestUniqueId) -> bool {
        self.waiting.lock().remove(link_request_id).is_some()
    }

    /// Put one request into `how_far_it_has_got`, so a test can drive a state
    /// the network would otherwise have to produce.
    #[cfg(test)]
    fn note_how_far_a_request_got_for_a_test(
        &self,
        link_request_id: &LinkRequestUniqueId,
        how_far_it_has_got: HowFarARequestHasGot,
    ) {
        if let Some(held) = self.waiting.lock().get_mut(link_request_id) {
            held.how_far_it_has_got = how_far_it_has_got;
        }
    }

    /// Every request still waiting, as `graph` renders them.
    pub fn render_for_graph(&self) -> Vec<LinkRequestAwaitingARuntimeOutput> {
        self.waiting
            .lock()
            .values()
            .map(|waiting| LinkRequestAwaitingARuntimeOutput {
                link_request_id: waiting.request.link_request_id.to_string(),
                operation: LinkRequestOperationOutput::from(waiting.request.operation),
                input_runtime_name: waiting.input_runtime_name.clone(),
                source: waiting
                    .request
                    .source_address
                    .as_ref()
                    .map(ToString::to_string),
                destination: waiting
                    .request
                    .destination_address
                    .as_ref()
                    .map(ToString::to_string),
                link_id: waiting.request.link_id.as_ref().map(ToString::to_string),
                state: waiting.how_far_it_has_got.state(),
                reason: waiting.how_far_it_has_got.reason().to_string(),
            })
            .collect()
    }

    /// Start sending waiting requests, now that this runtime is on a mesh.
    pub fn start_sending_every_waiting_request(
        &self,
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        peers: &Arc<RuntimeMeshPeerTable>,
    ) {
        self.stop_sending();

        let (wake_the_sender, when_to_look_again) = crossbeam_channel::unbounded();
        let whether_to_keep_sending = Arc::new(AtomicBool::new(true));

        // A runtime appearing is the event every waiting request is waiting
        // on, so the pass runs the moment one does rather than on the next tick.
        let wake_on_a_token = wake_the_sender.clone();
        let announcement_subscriber = match session
            .liveliness()
            .declare_subscriber(key_space.every_announcement_key())
            .history(false)
            .callback(move |_| {
                let _ = wake_on_a_token.send(());
            })
            .wait()
        {
            Ok(subscriber) => subscriber,
            Err(declare_failure) => {
                tracing::warn!(
                    "this runtime cannot watch the mesh for the runtimes its link requests wait \
                     on, so a waiting request is sent on the next pass rather than at once: \
                     {declare_failure}"
                );
                return;
            }
        };

        // The thread holds the shared map rather than the table itself: an
        // `Arc` back to the table would keep it alive for as long as the thread
        // ran, and the thread only ends when the table drops.
        let sending = SendingRequestsNeeds {
            session: session.clone(),
            key_space: key_space.clone(),
            peers: Arc::clone(peers),
            waiting: Arc::clone(&self.waiting),
        };
        let whether_this_thread_keeps_sending = Arc::clone(&whether_to_keep_sending);
        match std::thread::Builder::new()
            .name("streamlib-mesh-link-request-sender".to_string())
            .spawn(move || {
                send_every_waiting_request_until_told_to_stop(
                    sending,
                    when_to_look_again,
                    &whether_this_thread_keeps_sending,
                )
            }) {
            Ok(sending_thread) => {
                *self.sending.lock() = Some(SendingEveryWaitingRequest {
                    whether_to_keep_sending,
                    wake_the_sender,
                    announcement_subscriber,
                    sending_thread,
                });
            }
            Err(cannot_spawn) => tracing::warn!(
                "this runtime cannot send its link requests for want of a thread, so each stays \
                 waiting: {cannot_spawn}"
            ),
        }
    }

    /// Stop sending — a runtime leaving the mesh asks nobody for anything.
    pub fn stop(&self) {
        self.stop_sending();
        for waiting in self.waiting.lock().values_mut() {
            if waiting.how_far_it_has_got.it_is_worth_sending_again() {
                waiting.how_far_it_has_got = HowFarARequestHasGot::AwaitingTheRuntime {
                    reason: "this runtime has left the mesh".to_string(),
                };
            }
        }
    }

    /// End the sending thread, if one is running. Idempotent.
    fn stop_sending(&self) {
        let sending = self.sending.lock().take();
        if let Some(sending) = sending {
            drop(sending.announcement_subscriber);
            sending
                .whether_to_keep_sending
                .store(false, Ordering::Release);
            let _ = sending.wake_the_sender.send(());
            if sending.sending_thread.join().is_err() {
                tracing::warn!("the mesh link-request sending thread panicked");
            }
        }
    }

    fn ask_the_sender_to_look_again(&self) {
        if let Some(sending) = self.sending.lock().as_ref() {
            let _ = sending.wake_the_sender.send(());
        }
    }
}

impl Drop for LinkRequestsThisRuntimeHasSent {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Make room for one more request by forgetting the oldest one that will never
/// be sent again, or refuse by name when every one this runtime holds is live.
///
/// A refused request is a record kept for reading, and the oldest is the one
/// its author is least likely to still be looking at. A request still awaiting
/// its runtime, or still unanswered, is live work and is never dropped for a
/// newer one — so a runtime that fills up on live requests refuses the next,
/// which is the caller's to see rather than the log's.
fn forget_the_oldest_refused_request(
    waiting: &mut BTreeMap<LinkRequestUniqueId, ARequestThisRuntimeHasSent>,
) -> Result<()> {
    let oldest_refused = waiting
        .values()
        .filter(|held| !held.how_far_it_has_got.it_is_worth_sending_again())
        .min_by_key(|held| held.noted)
        .map(|held| held.request.link_request_id.clone());
    let Some(oldest_refused) = oldest_refused else {
        return Err(Error::Runtime(format!(
            "this runtime is already holding {HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS} \
             link requests no runtime has applied, and every one of them is still waiting or \
             still being sent. Read `graph`'s `mesh.link_requests_awaiting_runtime` and cancel \
             the ones that are no longer wanted."
        )));
    };
    tracing::warn!(
        "this runtime is holding {HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS} unapplied link \
         requests, so the oldest refused one ({oldest_refused}) is forgotten to make room"
    );
    waiting.remove(&oldest_refused);
    Ok(())
}

/// Everything one sending pass reads.
struct SendingRequestsNeeds {
    session: zenoh::Session,
    key_space: RuntimeMeshKeySpace,
    peers: Arc<RuntimeMeshPeerTable>,
    waiting: Arc<Mutex<BTreeMap<LinkRequestUniqueId, ARequestThisRuntimeHasSent>>>,
}

/// The sending thread's body.
fn send_every_waiting_request_until_told_to_stop(
    sending: SendingRequestsNeeds,
    when_to_look_again: Receiver<()>,
    whether_to_keep_sending: &AtomicBool,
) {
    while whether_to_keep_sending.load(Ordering::Acquire) {
        run_one_sending_pass(&sending, whether_to_keep_sending);
        match when_to_look_again.recv_timeout(HOW_OFTEN_EVERY_WAITING_REQUEST_IS_LOOKED_AT_AGAIN) {
            Ok(()) => {
                // Take every other wake-up queued behind this one: one pass
                // answers them all.
                while when_to_look_again.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Look at every waiting request and move each one on.
///
/// The stop flag is read between requests as well as between passes: one
/// request whose runtime is present but not answering costs the whole answer
/// timeout, so a pass over several would eat a shutdown budget.
fn run_one_sending_pass(sending: &SendingRequestsNeeds, whether_to_keep_sending: &AtomicBool) {
    let every_request: Vec<LinkRequestUniqueId> = sending.waiting.lock().keys().cloned().collect();
    for link_request_id in every_request {
        if !whether_to_keep_sending.load(Ordering::Acquire) {
            return;
        }
        send_one_request(sending, &link_request_id, whether_to_keep_sending);
    }
}

/// Send one request if it is due and its runtime is here, and write down what
/// came back.
fn send_one_request(
    sending: &SendingRequestsNeeds,
    link_request_id: &LinkRequestUniqueId,
    whether_to_keep_sending: &AtomicBool,
) {
    // Nothing here is held while the mesh is asked: the ask waits out a
    // timeout, and holding the table across it would block every `connect`.
    let (request, input_runtime_name) = {
        let mut waiting = sending.waiting.lock();
        let Some(due) = waiting.get_mut(link_request_id) else {
            return;
        };
        if !due.how_far_it_has_got.it_is_worth_sending_again() {
            return;
        }
        if Instant::now() < due.send_again_at {
            return;
        }
        // Presence comes from the peer table, never from a reply count: a
        // query that reaches nobody and one a runtime dropped both come back
        // with no replies at all.
        if sending
            .peers
            .every_peer_holding_the_name(&due.input_runtime_name)
            .is_empty()
        {
            due.how_far_it_has_got = HowFarARequestHasGot::AwaitingTheRuntime {
                reason: format!("the runtime {} is not on the mesh", due.input_runtime_name),
            };
            return;
        }
        (due.request.clone(), due.input_runtime_name.clone())
    };

    let answered = ask_a_runtime_to_apply_a_link_request(
        &sending.session,
        &sending.key_space,
        &input_runtime_name,
        &request,
        whether_to_keep_sending,
    );

    // Looked up again rather than held across the ask: a cancel that landed
    // while this was in flight took the entry with it, and finding it gone is
    // how this send learns to write nothing down.
    let mut waiting = sending.waiting.lock();
    let Some(sent) = waiting.get_mut(link_request_id) else {
        return;
    };
    match answered {
        HowARuntimeAnsweredALinkRequest::ItAppliedTheRequest(applied) => {
            tracing::info!(
                "the runtime {input_runtime_name} applied the link request {link_request_id} as \
                 link {} ({:?})",
                applied.link_id,
                applied.state
            );
            // Its outcome is that runtime's to render now, on the link itself.
            waiting.remove(link_request_id);
        }
        HowARuntimeAnsweredALinkRequest::ItRefusedTheRequest(refused) => {
            tracing::warn!(
                "the runtime {} refused the link request {link_request_id}: {}",
                refused.refused_by_runtime_name,
                refused.reason
            );
            sent.how_far_it_has_got = HowFarARequestHasGot::Refused {
                reason: format!(
                    "the runtime {} refused it: {}",
                    refused.refused_by_runtime_name, refused.reason
                ),
            };
        }
        // Said in its own words, and not final: it will be able to apply this
        // once it has finished starting, so the request keeps its backoff slot
        // exactly as silence does.
        HowARuntimeAnsweredALinkRequest::ItIsNotReadyYet(not_yet) => {
            sent.how_far_it_has_got = HowFarARequestHasGot::Unanswered {
                reason: format!(
                    "the runtime {} cannot apply it yet: {}",
                    not_yet.refused_by_runtime_name, not_yet.reason
                ),
            };
            sent.wait_longer_before_the_next_send();
        }
        HowARuntimeAnsweredALinkRequest::ItSaidNothing { reason } => {
            sent.how_far_it_has_got = HowFarARequestHasGot::Unanswered { reason };
            sent.wait_longer_before_the_next_send();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::{LinkUniqueId, MeshPortAddress};

    fn a_connect_request(link_request_id: &str) -> ALinkRequestOnTheMesh {
        ALinkRequestOnTheMesh::asking_for_a_link(
            LinkRequestUniqueId::from(link_request_id),
            MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video")
                .expect("a legal address"),
            MeshPortAddress::new("studio-display-9f3c", "DisplayWindow", "video")
                .expect("a legal address"),
            "bench-cam-a1b2",
        )
    }

    /// A request waits under its own id, naming the runtime it is waiting on
    /// and both ports — which is everything the author who asked needs to find
    /// the typo.
    #[test]
    fn a_waiting_request_renders_its_id_its_runtime_and_both_ends() {
        let table = LinkRequestsThisRuntimeHasSent::default();
        table
            .note_a_request_waiting_to_be_sent(
                a_connect_request("LRabc123"),
                "studio-display-9f3c",
                "default",
            )
            .expect("an empty table takes a request");

        let [rendered] = table.render_for_graph().try_into().expect("exactly one");
        assert_eq!(rendered.link_request_id, "LRabc123");
        assert_eq!(rendered.operation, LinkRequestOperationOutput::Connect);
        assert_eq!(rendered.input_runtime_name, "studio-display-9f3c");
        assert_eq!(
            rendered.source.as_deref(),
            Some("bench-cam-a1b2/CameraSource/video")
        );
        assert_eq!(
            rendered.destination.as_deref(),
            Some("studio-display-9f3c/DisplayWindow/video")
        );
        assert_eq!(rendered.state, LinkRequestStateOutput::AwaitingRuntime);
        assert!(rendered.reason.contains("default"), "{}", rendered.reason);
    }

    /// A cancelled request is gone from `graph` and never sent.
    #[test]
    fn a_cancelled_request_leaves_the_table() {
        let table = LinkRequestsThisRuntimeHasSent::default();
        table
            .note_a_request_waiting_to_be_sent(
                a_connect_request("LRabc123"),
                "studio-display-9f3c",
                "default",
            )
            .expect("an empty table takes a request");

        assert!(table.cancel_a_request(&LinkRequestUniqueId::from("LRabc123")));
        assert!(table.render_for_graph().is_empty());
        assert!(
            !table.cancel_a_request(&LinkRequestUniqueId::from("LRabc123")),
            "cancelling a request that is not there says so rather than pretending"
        );
    }

    /// A request asking for a link to go renders the link it names and no
    /// ports, because it has none to name.
    #[test]
    fn a_request_asking_for_a_link_to_go_renders_the_link_it_names() {
        let table = LinkRequestsThisRuntimeHasSent::default();
        table
            .note_a_request_waiting_to_be_sent(
                ALinkRequestOnTheMesh::asking_for_a_link_to_go(
                    LinkRequestUniqueId::from("LRxyz789"),
                    LinkUniqueId::from("Labc".to_string()),
                    "agent-wiring-e5f6",
                ),
                "studio-display-9f3c",
                "default",
            )
            .expect("an empty table takes a request");

        let [rendered] = table.render_for_graph().try_into().expect("exactly one");
        assert_eq!(rendered.operation, LinkRequestOperationOutput::Disconnect);
        assert_eq!(rendered.link_id.as_deref(), Some("Labc"));
        assert_eq!(rendered.source, None);
        assert_eq!(rendered.destination, None);
    }

    /// A runtime that reaches no mesh says so on the request rather than
    /// blaming the runtime it names — which may be perfectly healthy.
    #[test]
    fn a_runtime_off_its_mesh_says_so_rather_than_blaming_the_runtime_it_names() {
        let table = LinkRequestsThisRuntimeHasSent::default();
        table
            .note_a_request_waiting_to_be_sent(
                a_connect_request("LRabc123"),
                "studio-display-9f3c",
                "default",
            )
            .expect("an empty table takes a request");
        table.note_that_this_runtime_reaches_nobody(
            &LinkRequestUniqueId::from("LRabc123"),
            "this runtime is not on the default mesh".to_string(),
        );

        let [rendered] = table.render_for_graph().try_into().expect("exactly one");
        assert_eq!(rendered.state, LinkRequestStateOutput::AwaitingRuntime);
        assert!(
            rendered.reason.contains("this runtime is not on"),
            "{}",
            rendered.reason
        );
    }

    /// A runtime asked for more requests than it holds forgets its oldest
    /// refused one rather than growing without bound.
    ///
    /// The exposure is real rather than theoretical: until the security pass
    /// any runtime may ask, and an agent driving MCP `connect` at a display
    /// name that does not exist makes a refusal — which is final — every time.
    #[test]
    fn a_runtime_asked_for_more_than_it_holds_forgets_its_oldest_refused_request() {
        let table = LinkRequestsThisRuntimeHasSent::default();
        for which in 0..HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS {
            table
                .note_a_request_waiting_to_be_sent(
                    a_connect_request(&format!("LR{which}")),
                    "studio-display-9f3c",
                    "default",
                )
                .expect("a table under its cap takes a request");
        }
        // The first two were refused; everything after is still live.
        for refused in ["LR0", "LR1"] {
            table.note_how_far_a_request_got_for_a_test(
                &LinkRequestUniqueId::from(refused),
                HowFarARequestHasGot::Refused {
                    reason: "no such processor".to_string(),
                },
            );
        }

        table
            .note_a_request_waiting_to_be_sent(
                a_connect_request("LRone-too-many"),
                "studio-display-9f3c",
                "default",
            )
            .expect("a full table forgets a refused request to make room");

        let held: Vec<String> = table
            .render_for_graph()
            .into_iter()
            .map(|request| request.link_request_id)
            .collect();
        assert_eq!(held.len(), HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS);
        assert!(
            !held.contains(&"LR0".to_string()),
            "the oldest refused went"
        );
        assert!(held.contains(&"LR1".to_string()), "the next refused stayed");
        assert!(held.contains(&"LRone-too-many".to_string()));
    }

    /// A runtime whose every held request is still live refuses the next by
    /// name rather than dropping live work for it.
    #[test]
    fn a_runtime_holding_only_live_requests_refuses_the_next_by_name() {
        let table = LinkRequestsThisRuntimeHasSent::default();
        for which in 0..HOW_MANY_UNAPPLIED_REQUESTS_THIS_RUNTIME_HOLDS {
            table
                .note_a_request_waiting_to_be_sent(
                    a_connect_request(&format!("LR{which}")),
                    "studio-display-9f3c",
                    "default",
                )
                .expect("a table under its cap takes a request");
        }

        let refusal = table
            .note_a_request_waiting_to_be_sent(
                a_connect_request("LRone-too-many"),
                "studio-display-9f3c",
                "default",
            )
            .expect_err("every held request is live, so none can be forgotten")
            .to_string();
        assert!(
            refusal.contains("link_requests_awaiting_runtime"),
            "{refusal}"
        );
        assert!(refusal.contains("cancel"), "{refusal}");
    }

    /// A refusal is final and an unanswered send is not — which is what makes
    /// "silence is not a refusal" true of the pass rather than only stated.
    #[test]
    fn a_refusal_stops_the_resends_and_silence_does_not() {
        assert!(
            !HowFarARequestHasGot::Refused {
                reason: "no such processor".to_string()
            }
            .it_is_worth_sending_again()
        );
        for still_worth_it in [
            HowFarARequestHasGot::Unanswered {
                reason: "nothing came back".to_string(),
            },
            HowFarARequestHasGot::AwaitingTheRuntime {
                reason: "not on the mesh".to_string(),
            },
        ] {
            assert!(still_worth_it.it_is_worth_sending_again());
        }
    }
}
