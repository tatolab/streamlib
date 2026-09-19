// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One runtime's membership of one runtime mesh.
//!
//! Opened in `Runner::new()` and closed at the end of `Runner::stop()`. The
//! mesh never fails a runtime's start for want of a network: a session that
//! cannot open leaves the runtime local-only, saying so once, and the runtime
//! runs on. It fails one for exactly one reason — a name another live runtime
//! already holds, which is an address collision rather than a network failure.
//!
//! Every blocking Zenoh call here goes through
//! [`off_any_current_thread_tokio_runtime`], which is where the rule that none
//! of them may run on a current-thread tokio runtime is written down. Discovery
//! already has a thread of its own.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use parking_lot::Mutex;
use zenoh::Wait;
use zenoh::sample::SampleKind;

use crate::core::error::{Error, Result};
use crate::core::json_schema::{RuntimeMeshOutput, RuntimeMeshSessionOutput};
use crate::core::runtime::RuntimeName;
use crate::core::runtime::mesh::duplicate_runtime_name_on_the_mesh::refuse_this_runtime_if_its_name_is_already_live;
use crate::core::runtime::mesh::hosted_control_plane_endpoint::HostedControlPlaneEndpointRegistry;
use crate::core::runtime::mesh::mesh_link_ingress_table::MeshLinkIngressTable;
use crate::core::runtime::mesh::mesh_port_egress_table::MeshPortEgressTable;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::{
    OfferedOutputPortsQueryable, WhatThisRuntimeOffersOnTheMeshRegistry,
};
use crate::core::runtime::mesh::resolved_runtime_mesh_configuration::ResolvedRuntimeMeshConfiguration;
use crate::core::runtime::mesh::runtime_mesh_description::{
    RuntimeMeshDescription, ask_a_peer_what_it_is,
};
use crate::core::runtime::mesh::runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};
use crate::core::runtime::mesh::runtime_mesh_peer_table::RuntimeMeshPeerTable;
use crate::core::runtime::mesh::zenoh_work_off_any_tokio_runtime::off_any_current_thread_tokio_runtime;
use crate::iceoryx2::Iceoryx2Node;

/// How often every known peer is asked again what it is.
///
/// Asking once when a token appears is not enough: a runtime hosts its control
/// plane after it is constructed, so the first answer a peer gets names no URL
/// at all. Engine-chosen; nothing authorable.
const HOW_OFTEN_EVERY_PEER_IS_ASKED_AGAIN: Duration = Duration::from_secs(5);

/// This runtime's place on its mesh.
pub struct RuntimeMeshMembership {
    mesh_name: String,
    key_space: RuntimeMeshKeySpace,
    announced_identity: AnnouncedRuntimeIdentity,
    peers: Arc<RuntimeMeshPeerTable>,
    session: Mutex<RuntimeMeshSessionState>,
    /// What this runtime serves to the mesh, brought up once it has a graph and
    /// an iceoryx2 node — neither of which exists when the session opens.
    serving_this_runtimes_output_ports: Mutex<Option<ServingThisRuntimesOutputPorts>>,
    /// Every port on another runtime this one links from, brought up the same
    /// way. Held rather than owned: the runtime hands the same table to every
    /// `RuntimeContext`, through which the wiring op reaches it.
    carrying_links_from_other_runtimes: Mutex<Option<Arc<MeshLinkIngressTable>>>,
}

/// What a runtime holds on the mesh to serve its own output ports: the
/// queryable that answers which it offers, and the egresses the readers of
/// those ports create.
struct ServingThisRuntimesOutputPorts {
    _offered_output_ports_queryable: OfferedOutputPortsQueryable,
    _egress_table: MeshPortEgressTable,
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
    /// Errs for one reason only: another live runtime already holds this
    /// runtime's name. A mesh that cannot be reached is never one.
    pub fn join(
        resolved: &ResolvedRuntimeMeshConfiguration,
        runtime_name: &Arc<RuntimeName>,
        runtime_id: &str,
        host_name: &str,
        hosted_control_plane: &Arc<HostedControlPlaneEndpointRegistry>,
    ) -> Result<Self> {
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
                hosted_control_plane,
            )
        })
        .unwrap_or_else(|cannot_spawn| Err(cannot_spawn.into()))
        {
            Ok(announced) => {
                tracing::info!(
                    "Runtime {runtime_name} is on the {} mesh",
                    resolved.mesh_name
                );
                RuntimeMeshSessionState::Open(Box::new(announced))
            }
            Err(WhyThisRuntimeIsNotAnnounced::ItsNameIsAlreadyLive(refusal)) => {
                return Err(refusal);
            }
            Err(WhyThisRuntimeIsNotAnnounced::ItsMeshCouldNotBeReached(reason)) => {
                tracing::warn!(
                    "Runtime {runtime_name} could not join the {} mesh and is running \
                     local-only: {reason}",
                    resolved.mesh_name
                );
                RuntimeMeshSessionState::NotOnTheMesh { reason }
            }
        };

        Ok(Self {
            mesh_name: resolved.mesh_name.to_string(),
            key_space,
            announced_identity,
            peers,
            session: Mutex::new(session),
            serving_this_runtimes_output_ports: Mutex::new(None),
            carrying_links_from_other_runtimes: Mutex::new(None),
        })
    }

    /// The session this runtime is announced on, or `None` while it is not on
    /// its mesh.
    ///
    /// Cloned out from under the lock rather than borrowed: every caller goes
    /// on to declare something, which talks to the network, and `leave` wants
    /// this lock.
    fn the_session_it_is_announced_on(&self) -> Option<zenoh::Session> {
        match &*self.session.lock() {
            RuntimeMeshSessionState::Open(announced) => Some(announced.session.clone()),
            RuntimeMeshSessionState::NotOnTheMesh { .. } => None,
        }
    }

    /// Start serving this runtime's own output ports: answer a peer asking
    /// which it offers, and send one the moment another runtime reads it.
    ///
    /// Called once the runtime has a graph and an iceoryx2 node, which
    /// `Runner::new()` builds after the session is open. A runtime that never
    /// reached its mesh serves nothing and says nothing about it: it has no
    /// session to declare on, and `graph` already renders it local-only.
    pub fn start_serving_this_runtimes_output_ports(
        &self,
        offered: &Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
        iceoryx2_node: &Iceoryx2Node,
    ) {
        let Some(session) = self.the_session_it_is_announced_on() else {
            return;
        };
        let key_space = self.key_space.clone();
        let this_runtimes_name = self.announced_identity.runtime_name.clone();

        let served = off_any_current_thread_tokio_runtime("serve", || {
            let offered_output_ports_queryable = OfferedOutputPortsQueryable::declare(
                &session,
                &key_space,
                &this_runtimes_name,
                offered,
            )?;
            let egress_table = MeshPortEgressTable::watching_the_readers_of_this_runtimes_ports(
                &session,
                &key_space,
                &this_runtimes_name,
                offered,
                iceoryx2_node,
            )?;
            Ok::<_, zenoh::Error>(ServingThisRuntimesOutputPorts {
                _offered_output_ports_queryable: offered_output_ports_queryable,
                _egress_table: egress_table,
            })
        });

        match served {
            Ok(Ok(serving)) => {
                *self.serving_this_runtimes_output_ports.lock() = Some(serving);
            }
            Ok(Err(declare_failure)) => tracing::warn!(
                "this runtime is on the mesh but cannot serve its own output ports, so no other \
                 runtime can pull one: {declare_failure}"
            ),
            Err(cannot_spawn) => tracing::warn!(
                "this runtime cannot serve its own output ports for want of a thread: \
                 {cannot_spawn}"
            ),
        }
    }

    /// Start resolving this runtime's links from other runtimes, now that it
    /// has an iceoryx2 node to carry them onto.
    ///
    /// A runtime that never reached its mesh resolves nothing: every link from
    /// another runtime stays waiting, saying so, which is what `graph` already
    /// renders beside a local-only session.
    pub fn start_carrying_links_from_other_runtimes(
        &self,
        ingress_table: &Arc<MeshLinkIngressTable>,
    ) {
        let Some(session) = self.the_session_it_is_announced_on() else {
            return;
        };
        *self.carrying_links_from_other_runtimes.lock() = Some(Arc::clone(ingress_table));
        let key_space = self.key_space.clone();
        let this_runtimes_name = self.announced_identity.runtime_name.clone();
        let peers = Arc::clone(&self.peers);
        let ingress_table = Arc::clone(ingress_table);
        if let Err(cannot_spawn) = off_any_current_thread_tokio_runtime("carry", move || {
            ingress_table.start_resolving_every_waiting_link(
                &session,
                &key_space,
                &this_runtimes_name,
                &peers,
            );
        }) {
            tracing::warn!(
                "this runtime cannot resolve its links from other runtimes for want of a thread: \
                 {cannot_spawn}"
            );
        }
    }

    /// Note a link `connect` has just applied against a port on another
    /// runtime, so the mesh resolves it.
    ///
    /// A runtime that never reached its mesh resolves nothing, so the link is
    /// told that here and never asked about again — blaming the runtime it
    /// names would report a healthy peer as absent when it is this end that
    /// cannot see it.
    pub fn note_a_link_from_another_runtime(
        &self,
        address: crate::core::graph::MeshPortAddress,
        link_id: crate::core::graph::LinkUniqueId,
        how_far_it_has_got: Arc<Mutex<crate::core::graph::RemoteLinkResolution>>,
    ) {
        let carrying = self.carrying_links_from_other_runtimes.lock().clone();
        let Some(ingress_table) = carrying else {
            *how_far_it_has_got.lock() = crate::core::graph::RemoteLinkResolution::AwaitingRemote {
                reason: format!(
                    "this runtime is not on the {} mesh, so it reads nothing from it: {}",
                    self.mesh_name,
                    self.why_it_is_not_on_its_mesh()
                        .unwrap_or_else(|| "no reason was recorded".to_string()),
                ),
            };
            return;
        };
        ingress_table.note_a_link_waiting_on(address, link_id, how_far_it_has_got);
    }

    /// Why this runtime is not on its mesh, or `None` while it is.
    fn why_it_is_not_on_its_mesh(&self) -> Option<String> {
        match &*self.session.lock() {
            RuntimeMeshSessionState::Open(_) => None,
            RuntimeMeshSessionState::NotOnTheMesh { reason } => Some(reason.clone()),
        }
    }

    /// Leave the mesh: undeclare the token first, so peers see this runtime go
    /// at once, then close the session.
    ///
    /// Idempotent, because `stop()` is.
    pub fn leave(&self, why: &str) {
        // Before the session: dropping the egresses undeclares their tokens and
        // releases their channel slots while there is still a session to say so
        // on, so a reader sees every port stop rather than inferring it from a
        // lease running out. Taken out under the lock and dropped outside it,
        // because that teardown joins threads and talks to the network.
        let stopped_serving = self.serving_this_runtimes_output_ports.lock().take();
        drop(stopped_serving);
        let stopped_carrying = self.carrying_links_from_other_runtimes.lock().take();
        if let Some(carrying) = stopped_carrying {
            carrying.stop();
        }

        let previous = std::mem::replace(
            &mut *self.session.lock(),
            RuntimeMeshSessionState::NotOnTheMesh {
                reason: why.to_string(),
            },
        );
        let RuntimeMeshSessionState::Open(announced) = previous else {
            return;
        };
        // Beside the session: a runtime that has left reaches nobody, and a
        // `graph` rendering `local_only` next to a list of peers would say two
        // things at once.
        self.peers.forget_every_peer();

        let left = off_any_current_thread_tokio_runtime("leave", move || {
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
        if let Err(cannot_spawn) = left {
            tracing::warn!(
                "this runtime's mesh session could not be closed for want of a thread, so the \
                 kernel closes it at process exit instead: {cannot_spawn}"
            );
        }
    }

    /// A membership that never reached a mesh, for a test that needs a
    /// runtime's name and mesh name and no network.
    ///
    /// The same state `join` lands in when a session cannot open, reached
    /// without opening one.
    #[cfg(test)]
    pub(crate) fn that_never_reached_its_mesh(runtime_name: &str, mesh_name: &str) -> Self {
        Self {
            mesh_name: mesh_name.to_string(),
            key_space: RuntimeMeshKeySpace::of(
                crate::core::runtime::mesh::runtime_mesh_name::RuntimeMeshName::
                    from_configuration_environment_or_default(Some(mesh_name.to_string()))
                    .expect("a test names a legal mesh"),
            ),
            announced_identity: AnnouncedRuntimeIdentity {
                runtime_name: runtime_name.to_string(),
                host_identity: crate::core::runtime::mesh::HostIdentity::of_this_host(),
                process_id: std::process::id(),
            },
            peers: Arc::new(RuntimeMeshPeerTable::default()),
            session: Mutex::new(RuntimeMeshSessionState::NotOnTheMesh {
                reason: "this membership was built for a test and opened no session".to_string(),
            }),
            serving_this_runtimes_output_ports: Mutex::new(None),
            carrying_links_from_other_runtimes: Mutex::new(None),
        }
    }

    /// The name this runtime is addressed by on its mesh.
    pub fn runtime_name(&self) -> &str {
        &self.announced_identity.runtime_name
    }

    /// The mesh this runtime is on.
    pub fn mesh_name(&self) -> &str {
        &self.mesh_name
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

/// Why this runtime is not announced on its mesh — which decides whether it
/// runs local-only or does not run at all.
enum WhyThisRuntimeIsNotAnnounced {
    /// The session did not open, or something it declares did not. The runtime
    /// runs local-only and says so once.
    ItsMeshCouldNotBeReached(String),
    /// Another live runtime holds this runtime's name. The runtime refuses.
    ItsNameIsAlreadyLive(Error),
}

impl From<Box<dyn std::error::Error + Send + Sync>> for WhyThisRuntimeIsNotAnnounced {
    fn from(zenoh_failure: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::ItsMeshCouldNotBeReached(zenoh_failure.to_string())
    }
}

impl From<std::io::Error> for WhyThisRuntimeIsNotAnnounced {
    fn from(cannot_spawn: std::io::Error) -> Self {
        Self::ItsMeshCouldNotBeReached(cannot_spawn.to_string())
    }
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
) -> std::result::Result<AnnouncedOnTheMesh, WhyThisRuntimeIsNotAnnounced> {
    let session = zenoh::open(resolved.as_a_zenoh_configuration()?).wait()?;

    // Before anything is declared, so that a local `get` does not answer with
    // this session's own token and a refused runtime leaves nothing behind.
    if let Err(refusal) = refuse_this_runtime_if_its_name_is_already_live(
        &session,
        key_space,
        &resolved.mesh_name,
        announced_identity,
    ) {
        if let Err(close_failure) = session.close().wait() {
            tracing::debug!(
                "the session of a runtime refused its name did not close cleanly: {close_failure}"
            );
        }
        return Err(WhyThisRuntimeIsNotAnnounced::ItsNameIsAlreadyLive(refusal));
    }

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
        announced_identity.clone(),
        what_the_discovery_thread_reads,
    )?;

    let liveliness_token = session
        .liveliness()
        .declare_token(key_space.announcement_key_for(announced_identity))
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
    let announcement_key = key_space.announcement_key_for(announced_identity);
    let answered_key = announcement_key.clone();
    let runtime_id = runtime_id.to_string();
    let host_name = host_name.to_string();
    let hosted_control_plane = Arc::clone(hosted_control_plane);

    session
        .declare_queryable(announcement_key)
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
        .declare_subscriber(key_space.every_announcement_key())
        .history(true)
        .callback(move |token| {
            let Some(announced) = key_space.read_an_announcement_key(token.key_expr().as_str())
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
    this_runtime: AnnouncedRuntimeIdentity,
    what_the_discovery_thread_reads: Receiver<WhatTheMeshSaw>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("streamlib-mesh-discovery".to_string())
        .spawn(move || {
            let mut said_about = SameNamedPeersAlreadySaidOnce::default();
            loop {
                // Ends when the subscriber is dropped, which drops the sender;
                // a timeout is a round of asking every peer again.
                match what_the_discovery_thread_reads
                    .recv_timeout(HOW_OFTEN_EVERY_PEER_IS_ASKED_AGAIN)
                {
                    Ok(WhatTheMeshSaw::APeerAppeared(announced)) => {
                        said_about.say_it_once(&this_runtime, &announced);
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
                            // A round is serial and each ask waits out
                            // `HOW_LONG_A_PEER_HAS_TO_DESCRIBE_ITSELF` for a
                            // peer whose token is live and whose process is
                            // not, so a round can outlast its own cadence.
                            // `stop()` joins this thread, and a teardown that
                            // waited out a whole round would eat the shutdown
                            // budget — so the round gives up the moment the
                            // subscriber feeding it is gone, and takes whatever
                            // the mesh said in the meantime on its way past.
                            if !the_subscriber_feeding_this_thread_is_still_there(
                                &peers,
                                &this_runtime,
                                &mut said_about,
                                &what_the_discovery_thread_reads,
                            ) {
                                return;
                            }
                            ask_a_peer_what_it_is_and_record_it(
                                &session, &key_space, &peers, &announced,
                            );
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
}

/// Apply whatever the mesh has said since the last look, and say whether the
/// subscriber feeding this thread is still there.
///
/// A peer that appears here is recorded but not asked: the round it interrupted
/// already holds the list it is walking, and the next round asks it. Its name
/// renders meanwhile, which is what an unanswered peer renders anyway.
fn the_subscriber_feeding_this_thread_is_still_there(
    peers: &RuntimeMeshPeerTable,
    this_runtime: &AnnouncedRuntimeIdentity,
    said_about: &mut SameNamedPeersAlreadySaidOnce,
    what_the_discovery_thread_reads: &Receiver<WhatTheMeshSaw>,
) -> bool {
    loop {
        match what_the_discovery_thread_reads.try_recv() {
            Ok(WhatTheMeshSaw::APeerAppeared(announced)) => {
                said_about.say_it_once(this_runtime, &announced);
                peers.record_that_a_peer_appeared(announced);
            }
            Ok(WhatTheMeshSaw::APeerLeft(announced)) => peers.record_that_a_peer_left(&announced),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// The same-named peers this runtime has already said something about.
///
/// The stated residual: two runtimes that start inside one discovery window, or
/// that meet when a partition heals, are not refused — neither saw the other's
/// token in time. Both keep running and `graph` lists both, so the collision
/// has to be *said* or it is invisible. Said once per peer rather than on every
/// re-ask round, and the set is never pruned: a peer that leaves and returns is
/// the same collision, not a new one. A namesake restarting in a loop presents
/// a new pid each time and is therefore said again each time — by design, since
/// each of those really is a fresh process holding the name.
#[derive(Default)]
struct SameNamedPeersAlreadySaidOnce(BTreeSet<AnnouncedRuntimeIdentity>);

impl SameNamedPeersAlreadySaidOnce {
    /// Say that `announced` shares this runtime's name, the first time it does.
    fn say_it_once(
        &mut self,
        this_runtime: &AnnouncedRuntimeIdentity,
        announced: &AnnouncedRuntimeIdentity,
    ) {
        if let Some(collision) = self.what_to_say_about(this_runtime, announced) {
            tracing::warn!("{collision}");
        }
    }

    /// What there is to say about `announced`, or `None` when it does not share
    /// this runtime's name or has already been said.
    fn what_to_say_about(
        &mut self,
        this_runtime: &AnnouncedRuntimeIdentity,
        announced: &AnnouncedRuntimeIdentity,
    ) -> Option<String> {
        if announced == this_runtime || announced.runtime_name != this_runtime.runtime_name {
            return None;
        }
        if !self.0.insert(announced.clone()) {
            return None;
        }
        Some(format!(
            "Another runtime on this mesh is also named {}, on host {} as pid {}. Both are \
             running and both are in `graph`; a port address naming {} is ambiguous until one of \
             them restarts under another name.",
            announced.runtime_name,
            announced.host_identity.as_one_key_chunk(),
            announced.process_id,
            announced.runtime_name
        ))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh::HostIdentity;

    fn announced(runtime_name: &str, process_id: u32) -> AnnouncedRuntimeIdentity {
        AnnouncedRuntimeIdentity {
            runtime_name: runtime_name.to_string(),
            host_identity: HostIdentity::ThisKernelBootAndPidNamespace {
                kernel_boot_id: "2f1c8a30".to_string(),
                pid_namespace_inode: 4_026_531_836,
            },
            process_id,
        }
    }

    /// The residual is said once per peer and names the peer's host and pid, so
    /// a reader can go and find the other runtime.
    #[test]
    fn a_peer_sharing_this_runtimes_name_is_said_once_and_names_where_it_is() {
        let this_runtime = announced("rig-desk-a1b2", 100);
        let namesake = announced("rig-desk-a1b2", 4321);
        let mut said_about = SameNamedPeersAlreadySaidOnce::default();

        let collision = said_about
            .what_to_say_about(&this_runtime, &namesake)
            .expect("a peer sharing this runtime's name is worth saying");
        assert!(collision.contains("rig-desk-a1b2"), "{collision}");
        assert!(collision.contains("4321"), "{collision}");
        assert!(
            collision.contains(&namesake.host_identity.as_one_key_chunk()),
            "{collision}"
        );

        assert_eq!(said_about.what_to_say_about(&this_runtime, &namesake), None);
    }

    /// A peer under another name is not a collision, and this runtime's own
    /// announcement is never one either — a local liveliness subscription is
    /// delivered this runtime's own token.
    #[test]
    fn neither_another_name_nor_this_runtimes_own_announcement_is_ever_said() {
        let this_runtime = announced("rig-desk-a1b2", 100);
        let mut said_about = SameNamedPeersAlreadySaidOnce::default();

        assert_eq!(
            said_about.what_to_say_about(&this_runtime, &announced("rig-desk-c3d4", 4321)),
            None
        );
        assert_eq!(
            said_about.what_to_say_about(&this_runtime, &this_runtime.clone()),
            None
        );
    }

    /// A second runtime under the same name is its own collision — a partition
    /// healing onto two namesakes says both.
    #[test]
    fn a_second_namesake_is_said_as_well_as_the_first() {
        let this_runtime = announced("rig-desk-a1b2", 100);
        let mut said_about = SameNamedPeersAlreadySaidOnce::default();

        assert!(
            said_about
                .what_to_say_about(&this_runtime, &announced("rig-desk-a1b2", 4321))
                .is_some()
        );
        assert!(
            said_about
                .what_to_say_about(&this_runtime, &announced("rig-desk-a1b2", 9999))
                .is_some()
        );
    }
}
