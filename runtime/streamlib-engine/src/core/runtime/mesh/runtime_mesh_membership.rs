// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One runtime's membership of one runtime mesh.
//!
//! Opened in `Runner::new()` and closed at the end of `Runner::stop()`. The
//! mesh never fails a runtime's start: a session that cannot open leaves the
//! runtime local-only, saying so once, and the runtime runs on.
//!
//! **No Zenoh call may run on a current-thread tokio runtime** — Zenoh resolves
//! its builders by blocking on its own pool, which panics there. `Runner::new()`
//! and `stop()` are called from whatever thread an app happens to own, so
//! neither assumes: both hand their Zenoh work to a thread of this module's own
//! through [`off_any_current_thread_tokio_runtime`]. Discovery already has one.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use zenoh::Wait;
use zenoh::sample::SampleKind;

use crate::core::json_schema::{RuntimeMeshOutput, RuntimeMeshSessionOutput};
use crate::core::runtime::RuntimeName;
use crate::core::runtime::mesh::hosted_control_plane_endpoint::HostedControlPlaneEndpointRegistry;
use crate::core::runtime::mesh::resolved_runtime_mesh_configuration::ResolvedRuntimeMeshConfiguration;
use crate::core::runtime::mesh::runtime_mesh_description::RuntimeMeshDescription;
use crate::core::runtime::mesh::runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};
use crate::core::runtime::mesh::runtime_mesh_peer_table::RuntimeMeshPeerTable;

/// How long a peer has to answer what it is before its description is left
/// unread until the next round. Engine-chosen; nothing authorable.
const HOW_LONG_A_PEER_HAS_TO_DESCRIBE_ITSELF: Duration = Duration::from_secs(2);

/// How often every known peer is asked again what it is.
///
/// Asking once when a token appears is not enough: a runtime hosts its control
/// plane after it is constructed, so the first answer a peer gets names no URL
/// at all. Engine-chosen; nothing authorable.
const HOW_OFTEN_EVERY_PEER_IS_ASKED_AGAIN: Duration = Duration::from_secs(5);

/// This runtime's place on its mesh.
pub struct RuntimeMeshMembership {
    mesh_name: String,
    announced_identity: AnnouncedRuntimeIdentity,
    peers: Arc<RuntimeMeshPeerTable>,
    hosted_control_plane: Arc<HostedControlPlaneEndpointRegistry>,
    session: Mutex<RuntimeMeshSessionState>,
}

/// Whether this runtime reached its mesh, and what it holds there if it did.
enum RuntimeMeshSessionState {
    Open(Box<AnnouncedOnTheMesh>),
    /// The session could not open, or has been closed. Either way this runtime
    /// reaches no other, which is the one thing `graph` has to say.
    NotOnTheMesh {
        reason: String,
    },
}

/// What an open session holds. Dropped in declaration order at close: the
/// subscriber first, which stops the discovery thread by dropping its sender.
struct AnnouncedOnTheMesh {
    session: zenoh::Session,
    liveliness_token: zenoh::liveliness::LivelinessToken,
    description_queryable: zenoh::query::Queryable<()>,
    liveliness_subscriber: zenoh::pubsub::Subscriber<()>,
    discovery_thread: std::thread::JoinHandle<()>,
}

/// What the liveliness subscriber tells the discovery thread.
enum WhatTheMeshSaw {
    APeerAppeared(AnnouncedRuntimeIdentity),
    APeerLeft(AnnouncedRuntimeIdentity),
}

impl RuntimeMeshMembership {
    /// Join the mesh `resolved` names, or run local-only saying why once.
    ///
    /// Never returns an error: a mesh that cannot be reached is not a reason a
    /// runtime fails to start.
    pub fn join(
        resolved: &ResolvedRuntimeMeshConfiguration,
        runtime_name: &Arc<RuntimeName>,
        runtime_id: &str,
        host_name: &str,
        hosted_control_plane: Arc<HostedControlPlaneEndpointRegistry>,
    ) -> Self {
        let key_space = RuntimeMeshKeySpace::of(resolved.mesh_name.clone());
        let announced_identity = AnnouncedRuntimeIdentity::of_this_runtime(runtime_name);
        let peers = Arc::new(RuntimeMeshPeerTable::default());

        let session = match off_any_current_thread_tokio_runtime("join", || {
            announce_on_the_mesh(
                resolved,
                &key_space,
                &announced_identity,
                &peers,
                runtime_id,
                host_name,
                &hosted_control_plane,
            )
        }) {
            Ok(announced) => {
                tracing::info!(
                    "Runtime {runtime_name} is on the {} mesh",
                    resolved.mesh_name
                );
                RuntimeMeshSessionState::Open(Box::new(announced))
            }
            Err(why_it_could_not_join) => {
                let reason = why_it_could_not_join.to_string();
                tracing::warn!(
                    "Runtime {runtime_name} could not join the {} mesh and is running \
                     local-only: {reason}",
                    resolved.mesh_name
                );
                RuntimeMeshSessionState::NotOnTheMesh { reason }
            }
        };

        Self {
            mesh_name: resolved.mesh_name.to_string(),
            announced_identity,
            peers,
            hosted_control_plane,
            session: Mutex::new(session),
        }
    }

    /// Where the control plane this runtime hosts can be reached, for the
    /// control plane itself to fill in once it has bound.
    pub fn hosted_control_plane(&self) -> &Arc<HostedControlPlaneEndpointRegistry> {
        &self.hosted_control_plane
    }

    /// Leave the mesh: undeclare the token first, so peers see this runtime go
    /// at once, then close the session.
    ///
    /// Idempotent, because `stop()` is.
    pub fn leave(&self, why: &str) {
        let previous = std::mem::replace(
            &mut *self.session.lock(),
            RuntimeMeshSessionState::NotOnTheMesh {
                reason: why.to_string(),
            },
        );
        let RuntimeMeshSessionState::Open(announced) = previous else {
            return;
        };
        off_any_current_thread_tokio_runtime("leave", move || {
            let AnnouncedOnTheMesh {
                session,
                liveliness_token,
                description_queryable,
                liveliness_subscriber,
                discovery_thread,
            } = *announced;

            // The subscriber first: dropping it drops the callback that holds
            // the discovery thread's sender, which is what ends that thread.
            drop(liveliness_subscriber);
            if discovery_thread.join().is_err() {
                tracing::warn!("the mesh discovery thread panicked; its peers are stale");
            }

            if let Err(undeclare_failure) = liveliness_token.undeclare().wait() {
                tracing::warn!(
                    "this runtime's mesh token could not be undeclared, so peers see it leave \
                     when its connections close instead: {undeclare_failure}"
                );
            }
            drop(description_queryable);

            if let Err(close_failure) = session.close().wait() {
                tracing::warn!(
                    "this runtime's mesh session did not close cleanly: {close_failure}"
                );
            }
        });
    }

    /// This runtime's place on the mesh, as `graph` renders it.
    pub fn render_for_graph(&self) -> RuntimeMeshOutput {
        let (session, local_only_reason) = match &*self.session.lock() {
            RuntimeMeshSessionState::Open(_) => (RuntimeMeshSessionOutput::Open, None),
            RuntimeMeshSessionState::NotOnTheMesh { reason } => {
                (RuntimeMeshSessionOutput::LocalOnly, Some(reason.clone()))
            }
        };
        RuntimeMeshOutput {
            mesh_name: self.mesh_name.clone(),
            runtime_name: self.announced_identity.runtime_name.clone(),
            session,
            local_only_reason,
            peers: self.peers.render_for_graph(),
        }
    }
}

/// Run `zenoh_work` on a thread that is nobody's tokio runtime.
///
/// A scoped thread rather than a detached one: the caller has to have the
/// result before it goes on, and a panic inside comes back out unchanged rather
/// than being reported as a join failure.
fn off_any_current_thread_tokio_runtime<T: Send>(
    mesh_step: &str,
    zenoh_work: impl FnOnce() -> T + Send,
) -> T {
    std::thread::scope(|threads| {
        match std::thread::Builder::new()
            .name(format!("streamlib-mesh-{mesh_step}"))
            .spawn_scoped(threads, zenoh_work)
        {
            Ok(thread) => match thread.join() {
                Ok(done) => done,
                Err(panicked) => std::panic::resume_unwind(panicked),
            },
            Err(cannot_spawn) => panic!(
                "the thread this runtime's mesh {mesh_step} needs could not be spawned: \
                 {cannot_spawn}"
            ),
        }
    })
}

/// Open the session and take everything this runtime holds on the mesh.
fn announce_on_the_mesh(
    resolved: &ResolvedRuntimeMeshConfiguration,
    key_space: &RuntimeMeshKeySpace,
    announced_identity: &AnnouncedRuntimeIdentity,
    peers: &Arc<RuntimeMeshPeerTable>,
    runtime_id: &str,
    host_name: &str,
    hosted_control_plane: &Arc<HostedControlPlaneEndpointRegistry>,
) -> zenoh::Result<AnnouncedOnTheMesh> {
    let session = zenoh::open(resolved.as_a_zenoh_configuration()?).wait()?;

    // The queryable before the token: a peer that sees the token and asks at
    // once must find somebody to answer.
    let description_queryable = declare_the_description_queryable(
        &session,
        key_space,
        announced_identity,
        runtime_id,
        host_name,
        hosted_control_plane,
    )?;

    let (what_the_mesh_saw, what_the_discovery_thread_reads) = crossbeam_channel::unbounded();
    let liveliness_subscriber = declare_the_liveliness_subscriber(
        &session,
        key_space,
        announced_identity,
        what_the_mesh_saw,
    )?;
    let discovery_thread = spawn_the_discovery_thread(
        session.clone(),
        key_space.clone(),
        Arc::clone(peers),
        what_the_discovery_thread_reads,
    );

    let liveliness_token = session
        .liveliness()
        .declare_token(key_space.liveliness_token_key_for(announced_identity))
        .wait()?;

    Ok(AnnouncedOnTheMesh {
        session,
        liveliness_token,
        description_queryable,
        liveliness_subscriber,
        discovery_thread,
    })
}

/// The queryable that answers what this runtime is, built from state at query
/// time so a control plane hosted later shows up.
fn declare_the_description_queryable(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    announced_identity: &AnnouncedRuntimeIdentity,
    runtime_id: &str,
    host_name: &str,
    hosted_control_plane: &Arc<HostedControlPlaneEndpointRegistry>,
) -> zenoh::Result<zenoh::query::Queryable<()>> {
    let description_key = key_space.description_key_for(announced_identity);
    let answered_key = description_key.clone();
    let runtime_id = runtime_id.to_string();
    let host_name = host_name.to_string();
    let hosted_control_plane = Arc::clone(hosted_control_plane);

    session
        .declare_queryable(description_key)
        .callback(move |asked| {
            let described = RuntimeMeshDescription::of_this_runtime_right_now(
                &runtime_id,
                &host_name,
                &hosted_control_plane,
            );
            match described.encode() {
                Ok(wire_bytes) => {
                    if let Err(reply_failure) = asked.reply(answered_key.clone(), wire_bytes).wait()
                    {
                        tracing::debug!(
                            "a mesh peer asked what this runtime is and the answer did not \
                             reach it: {reply_failure}"
                        );
                    }
                }
                Err(encode_failure) => {
                    tracing::warn!(
                        "this runtime could not describe itself to a mesh peer: {encode_failure}"
                    );
                }
            }
        })
        .wait()
}

/// The subscriber that hands every token arriving and leaving to the discovery
/// thread. The callback runs on Zenoh's own threads, so it only sends.
fn declare_the_liveliness_subscriber(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    announced_identity: &AnnouncedRuntimeIdentity,
    what_the_mesh_saw: Sender<WhatTheMeshSaw>,
) -> zenoh::Result<zenoh::pubsub::Subscriber<()>> {
    let key_space = key_space.clone();
    let this_runtime = announced_identity.clone();

    session
        .liveliness()
        .declare_subscriber(key_space.every_liveliness_token_key())
        .history(true)
        .callback(move |token| {
            let Some(announced) = key_space.read_a_liveliness_token_key(token.key_expr().as_str())
            else {
                return;
            };
            // A local liveliness subscription sees this runtime's own token.
            if announced == this_runtime {
                return;
            }
            let saw = match token.kind() {
                SampleKind::Put => WhatTheMeshSaw::APeerAppeared(announced),
                SampleKind::Delete => WhatTheMeshSaw::APeerLeft(announced),
            };
            let _ = what_the_mesh_saw.send(saw);
        })
        .wait()
}

/// The one thread that queries peers and writes the table.
///
/// Its own thread rather than the subscriber's callback because a description
/// query blocks on the network, and a callback that blocks would hold up every
/// other token Zenoh is delivering.
fn spawn_the_discovery_thread(
    session: zenoh::Session,
    key_space: RuntimeMeshKeySpace,
    peers: Arc<RuntimeMeshPeerTable>,
    what_the_discovery_thread_reads: Receiver<WhatTheMeshSaw>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("streamlib-mesh-discovery".to_string())
        .spawn(move || {
            loop {
                // Ends when the subscriber is dropped, which drops the sender;
                // a timeout is a round of asking every peer again.
                match what_the_discovery_thread_reads
                    .recv_timeout(HOW_OFTEN_EVERY_PEER_IS_ASKED_AGAIN)
                {
                    Ok(WhatTheMeshSaw::APeerAppeared(announced)) => {
                        peers.record_that_a_peer_appeared(announced.clone());
                        ask_a_peer_what_it_is_and_record_it(
                            &session, &key_space, &peers, &announced,
                        );
                    }
                    Ok(WhatTheMeshSaw::APeerLeft(announced)) => {
                        peers.record_that_a_peer_left(&announced);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        for announced in peers.every_peer_it_sees() {
                            ask_a_peer_what_it_is_and_record_it(
                                &session, &key_space, &peers, &announced,
                            );
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawning the mesh discovery thread")
}

/// Ask a peer what it is and record the answer, leaving what it last said in
/// place when it does not answer this time.
fn ask_a_peer_what_it_is_and_record_it(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    peers: &RuntimeMeshPeerTable,
    announced: &AnnouncedRuntimeIdentity,
) {
    if let Some(described) = ask_a_peer_what_it_is(session, key_space, announced) {
        peers.record_what_a_peer_answered(announced, described);
    }
}

/// What a peer says it is, or `None` when it did not answer in time or
/// answered something this engine cannot read.
fn ask_a_peer_what_it_is(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    announced: &AnnouncedRuntimeIdentity,
) -> Option<RuntimeMeshDescription> {
    let replies = session
        .get(key_space.description_key_for(announced))
        .timeout(HOW_LONG_A_PEER_HAS_TO_DESCRIBE_ITSELF)
        .wait()
        .inspect_err(|query_failure| {
            tracing::debug!(
                "could not ask the mesh peer {} what it is: {query_failure}",
                announced.runtime_name
            );
        })
        .ok()?;

    for reply in replies {
        let Ok(answered) = reply.result() else {
            continue;
        };
        match RuntimeMeshDescription::decode(&answered.payload().to_bytes()) {
            Ok(described) => return Some(described),
            Err(unreadable) => {
                tracing::debug!(
                    "the mesh peer {} answered something this engine cannot read: {unreadable}",
                    announced.runtime_name
                );
            }
        }
    }
    None
}
