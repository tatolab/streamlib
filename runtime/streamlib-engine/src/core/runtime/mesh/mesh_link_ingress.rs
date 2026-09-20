// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Carrying one port of another runtime into this one.
//!
//! One ingress per source address, shared by every local link from it. It says
//! on the mesh that this runtime is reading the port, subscribes to the port's
//! data key, and writes what arrives onto an engine-named local channel as that
//! channel's single publisher — so every local destination reads a remote link
//! exactly as it reads any other, and nothing downstream can tell.
//!
//! The Zenoh callback only hands off into a ring: it runs on the link's receive
//! loop, and blocking there stalls every key arriving from that peer and, past
//! the lease, expires the link. The ring evicts its oldest bag when it is full,
//! because no link ever blocks a producer — least of all one on another
//! machine.
//!
//! What the hop lost is counted here and nowhere else, off a jump in the
//! sequence number the sending runtime carried from the bag's own publisher.
//! Counted on the writing thread rather than in the callback, so that this
//! ring's own evictions are inside the jump: one gap then covers the sending
//! channel's ring, the bags the egress never sent, Zenoh's silent drop, the
//! network, and this ring.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{Condvar, Mutex};
use zenoh::Wait;

use crate::core::graph::MeshPortAddress;
use crate::core::runtime::mesh::a_frames_pixels_on_the_mesh::a_frames_pixels_off_the_mesh;
use crate::core::runtime::mesh::a_frames_pixels_written_into_a_local_surface::{
    WhyAFramesPixelsCannotLandHere, WritesAFramesPixelsIntoALocalSurface,
};
use crate::core::runtime::mesh::gpu_context_the_mesh_copies_frames_with::GpuContextTheMeshCopiesFramesWith;
use crate::core::runtime::mesh::machine_clock_a_remote_link_carries_from::{
    MachineClockARemoteLinkCarriesFrom, WhatNotingABagsClockDid,
};
use crate::core::runtime::mesh::mesh_data_message_attachment::{
    MeshDataMessageAttachment, PublisherGenerationOnTheMesh,
};
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::iceoryx2::{
    BagsAGapInTheNumberingSaysWereLost, ChannelEgressConfig, ChannelTrustTier,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DeliveryProfile, Iceoryx2Node, OutputWriterInner,
    RemoteInboundLinkMeshHopDroppedBagCounter, effective_channel_chunk_ceiling_bytes,
    mesh_ingress_channel_name,
};

/// How many bytes of arriving messages one ingress's ring may hold.
///
/// The depth alone bounded a ring of bags. A ring that can also hold a frame's
/// pixels needs a second bound: sixteen 1080p RGBA frames are 133 MB held
/// against a hiccup on one thread, and several remote video links multiply it.
/// The hop is not a second place to buffer — a consumer that cannot keep up
/// must lose bags here rather than grow a queue somebody is paying for.
///
/// Engine-chosen; nothing authorable. Two 4K RGBA frames, or sixteen of
/// anything smaller. A message over the ceiling on its own is still taken,
/// evicting everything behind it: a port whose every frame is over it would
/// otherwise carry nothing at all.
const HOW_MANY_BYTES_ONE_INGRESS_RING_MAY_HOLD: usize = 64 * 1024 * 1024;

/// The one output port name the ingress publishes under on its local channel.
///
/// Engine-derived like the channel itself: destinations route by the subscriber
/// they bound, never by the frame header's port key, so this names nothing a
/// reader has to know.
const THE_INGRESS_OUTPUT_PORT: &str = "bags";

/// One message as it arrives from the mesh, before the writing thread takes
/// it — the producer's bag, and a frame's description and pixels ahead of and
/// behind it when the bag names a surface.
///
/// Split on the writing thread rather than in the callback: the callback runs
/// on the link's receive loop and only hands off, and the split is free once
/// the bytes are owned either way.
struct ABagOffTheMesh {
    /// Owned rather than Zenoh's own buffer, which is what costs the one
    /// copy this hop makes. A received `ZSlice` points into the link's
    /// `RecyclingObjectPool` of MTU-sized buffers, so a ring holding sixteen
    /// of them — across every ingress — keeps that many out of circulation
    /// and can stall the very receive loop the callback must never block.
    /// Copying hands the pool its buffer straight back.
    payload_bytes: Vec<u8>,
    /// The record that rode beside it, whole: the stamp to write it under, the
    /// sending runtime's number for it and the run that number belongs to, and
    /// the clock its stamp was taken on.
    attached: MeshDataMessageAttachment,
}

/// The ring between the Zenoh callback and the writing thread.
///
/// As deep as an `ordered` port's own ring: the hop is not a second place to
/// buffer, and a consumer that cannot keep up must lose bags here rather than
/// grow a queue the producer is paying for.
#[derive(Default)]
struct WhatHasArrivedFromTheMesh {
    ring: VecDeque<ABagOffTheMesh>,
    /// What the ring currently holds, kept rather than summed: the callback
    /// runs on the link's receive loop, where walking the ring per arrival
    /// would be work done in the one place that may not do any.
    bytes_in_the_ring: usize,
    the_ingress_is_stopping: bool,
}

impl WhatHasArrivedFromTheMesh {
    /// Take one arriving message in, evicting the oldest until it fits both
    /// bounds.
    fn take_this_one_in(&mut self, arriving: ABagOffTheMesh) {
        while self.ring.len() >= DeliveryProfile::ORDERED_DEPTH
            || (!self.ring.is_empty()
                && self.bytes_in_the_ring + arriving.payload_bytes.len()
                    > HOW_MANY_BYTES_ONE_INGRESS_RING_MAY_HOLD)
        {
            let Some(evicted) = self.ring.pop_front() else {
                break;
            };
            self.bytes_in_the_ring -= evicted.payload_bytes.len();
        }
        self.bytes_in_the_ring += arriving.payload_bytes.len();
        self.ring.push_back(arriving);
    }

    /// Hand the writing thread the oldest message the ring holds.
    fn take_the_oldest_out(&mut self) -> Option<ABagOffTheMesh> {
        let taken = self.ring.pop_front()?;
        self.bytes_in_the_ring -= taken.payload_bytes.len();
        Some(taken)
    }
}

/// One local link an ingress feeds, and what it takes to count its hop loss.
struct OneLinkThisIngressFeeds {
    where_its_hop_loss_is_counted: RemoteInboundLinkMeshHopDroppedBagCounter,
    /// Its own view of the sending runtime's numbering, not the ingress's: a
    /// link wired onto an ingress that is already carrying must take its own
    /// first bag as its baseline, or its very first count would be a stretch
    /// of the port that went missing before the link existed.
    bags_the_hop_lost: BagsAGapInTheNumberingSaysWereLost<PublisherGenerationOnTheMesh>,
}

/// One port of another runtime, being carried into this one.
pub(super) struct MeshLinkIngress {
    address: MeshPortAddress,
    /// The publisher the writing thread writes through, shared so a destination
    /// wired later can add its notifier to it.
    writes_onto_the_local_channel: Arc<OutputWriterInner>,
    /// Set while the source runtime holds an egress token for this port, so a
    /// port that stops being sent is told apart from one that never started.
    the_source_is_sending: Arc<SourceSendingState>,
    arrived: Arc<(Mutex<WhatHasArrivedFromTheMesh>, Condvar)>,
    /// Where each local link this ingress feeds has its hop loss counted — on
    /// its own destination processor's node, so `graph` reads it beside what
    /// that processor's ports lost. Shared with the writing thread, which is
    /// what records into them.
    ///
    /// Keyed by link id so a destination that goes takes its counter with it,
    /// and so a link wired again over a surviving ingress replaces its counter
    /// rather than gaining a second one to be charged twice.
    every_link_it_feeds: Arc<Mutex<BTreeMap<String, OneLinkThisIngressFeeds>>>,
    held_on_the_mesh: Option<HeldOnTheMeshByOneIngress>,
    writing_thread: Option<std::thread::JoinHandle<()>>,
}

/// Whether the source runtime is sending this port, and whether it ever was.
#[derive(Default)]
pub(crate) struct SourceSendingState {
    sending_now: AtomicBool,
    has_ever_sent: AtomicBool,
}

impl SourceSendingState {
    fn note_that_it_started_sending(&self) {
        self.sending_now.store(true, Ordering::Release);
        self.has_ever_sent.store(true, Ordering::Release);
    }

    fn note_that_it_stopped_sending(&self) {
        self.sending_now.store(false, Ordering::Release);
    }

    /// Whether the source is sending this port right now.
    pub(crate) fn it_is_sending(&self) -> bool {
        self.sending_now.load(Ordering::Acquire)
    }

    /// Whether the source has stopped sending a port it was sending. A port
    /// that has never started is not "stopped": the ingress has only just
    /// declared its reader token and the egress is still coming up.
    pub(crate) fn it_was_sending_and_stopped(&self) -> bool {
        self.has_ever_sent.load(Ordering::Acquire) && !self.sending_now.load(Ordering::Acquire)
    }
}

/// What one ingress holds on the mesh, dropped in declaration order.
struct HeldOnTheMeshByOneIngress {
    _data_subscriber: zenoh::pubsub::Subscriber<()>,
    _egress_token_subscriber: zenoh::pubsub::Subscriber<()>,
    reader_token: Option<zenoh::liveliness::LivelinessToken>,
}

impl MeshLinkIngress {
    /// Start carrying `address` into this runtime, or say why it could not.
    ///
    /// The reader token is declared last of the three: it is what makes the
    /// source create its egress, and a bag arriving before this side is
    /// subscribed would be lost for no reason.
    ///
    /// `machine_clock_it_carries_from` is the table's cell for this address,
    /// which the writing thread fills off each arriving bag. The table keeps
    /// it rather than this ingress, because the links wired with it outlive
    /// the source runtime leaving and this ingress with it.
    pub(super) fn start(
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        address: &MeshPortAddress,
        iceoryx2_node: &Iceoryx2Node,
        wake_the_resolver: crossbeam_channel::Sender<()>,
        gpu_context_the_mesh_copies_frames_with: &Arc<GpuContextTheMeshCopiesFramesWith>,
        machine_clock_it_carries_from: &Arc<MachineClockARemoteLinkCarriesFrom>,
    ) -> crate::core::Result<Self> {
        let local_channel = mesh_ingress_channel_name(&address.to_string()).into_string();
        let writes_onto_the_local_channel = Arc::new(OutputWriterInner::new());
        writes_onto_the_local_channel.declare_output_ports([THE_INGRESS_OUTPUT_PORT.to_string()]);

        // The channel the compiler already opened for this address's
        // destinations, opened rather than created: its depth is the one the
        // compiler derived from those destinations — deeper when one of them
        // windows — and iceoryx2 refuses an open that asks for another. A
        // channel that is not there yet is a link this runtime has not
        // committed the wiring of, which the next resolution pass picks up.
        let service = iceoryx2_node
            .open_existing_channel_service(&local_channel)?
            .ok_or_else(|| {
                crate::core::Error::Runtime(format!(
                    "the channel {local_channel} that {address}'s bags land on is not open yet, \
                     so this runtime has nothing to write them onto"
                ))
            })?;
        let publisher = service.create_publisher(DEFAULT_EXPECTED_PAYLOAD_BYTES)?;
        // Trusted: the writer is the engine itself, in the app process. What a
        // destination in a helper may take is the destination's own tier, which
        // its side of the wiring already applies.
        let trust_tier = ChannelTrustTier::Trusted;
        writes_onto_the_local_channel.set_channel_publisher(
            THE_INGRESS_OUTPUT_PORT,
            publisher,
            ChannelEgressConfig {
                service_name: local_channel.clone(),
                trust_tier,
                expected_payload_bytes: DEFAULT_EXPECTED_PAYLOAD_BYTES,
                chunk_ceiling_bytes: effective_channel_chunk_ceiling_bytes(trust_tier),
            },
        );

        let arrived = Arc::new((
            Mutex::new(WhatHasArrivedFromTheMesh::default()),
            Condvar::new(),
        ));
        let the_source_is_sending = Arc::new(SourceSendingState::default());

        let data_subscriber = declare_the_data_subscriber(session, key_space, address, &arrived)?;
        let egress_token_subscriber = declare_the_egress_token_subscriber(
            session,
            key_space,
            address,
            &the_source_is_sending,
            wake_the_resolver,
        )?;
        let reader_token = session
            .liveliness()
            .declare_token(key_space.reader_token_key(
                &address.runtime_name(),
                &address.processor_display_name(),
                &address.port_name(),
                this_runtimes_name,
            ))
            .wait()
            .map_err(|declare_failure| {
                crate::core::Error::Runtime(format!(
                    "this runtime could not say on the mesh that it is reading {address}, so \
                     nothing would ever be sent to it: {declare_failure}"
                ))
            })?;

        let every_link_it_feeds = Arc::new(Mutex::new(BTreeMap::new()));
        let writing_thread = spawn_the_writing_thread(
            address.clone(),
            Arc::clone(&arrived),
            Arc::clone(&writes_onto_the_local_channel),
            Arc::clone(&every_link_it_feeds),
            Arc::clone(machine_clock_it_carries_from),
            WritesAFramesPixelsIntoALocalSurface::minting_through(
                gpu_context_the_mesh_copies_frames_with,
            ),
        )?;

        tracing::info!("This runtime is reading {address} off the mesh into {local_channel}");
        Ok(Self {
            address: address.clone(),
            writes_onto_the_local_channel,
            the_source_is_sending,
            arrived,
            every_link_it_feeds,
            held_on_the_mesh: Some(HeldOnTheMeshByOneIngress {
                _data_subscriber: data_subscriber,
                _egress_token_subscriber: egress_token_subscriber,
                reader_token: Some(reader_token),
            }),
            writing_thread: Some(writing_thread),
        })
    }

    /// Whether the source is sending this port right now — it holds an egress
    /// token for it.
    pub(super) fn the_source_is_sending(&self) -> bool {
        self.the_source_is_sending.it_is_sending()
    }

    /// Whether the source stopped sending a port it was sending.
    pub(super) fn the_source_stopped_sending(&self) -> bool {
        self.the_source_is_sending.it_was_sending_and_stopped()
    }

    /// Record one local destination of this address, with the notifier that
    /// wakes it and the counter its hop loss lands in.
    ///
    /// The ingress is the channel's publisher, so it is what holds every
    /// destination's notifier — the part a local source's output writer holds
    /// for a link between two processors here.
    ///
    /// The hop's loss is charged to every link this ingress feeds, and not
    /// shared out between them: one gap is one stretch of this port that
    /// reached none of them.
    pub(super) fn note_a_local_destination(
        &self,
        link_id: &str,
        notifier: Option<iceoryx2::port::notifier::Notifier<iceoryx2::service::ipc::Service>>,
        where_its_hop_loss_is_counted: RemoteInboundLinkMeshHopDroppedBagCounter,
    ) {
        self.writes_onto_the_local_channel.add_channel_link(
            THE_INGRESS_OUTPUT_PORT,
            link_id,
            notifier,
        );
        self.every_link_it_feeds.lock().insert(
            link_id.to_string(),
            OneLinkThisIngressFeeds {
                where_its_hop_loss_is_counted,
                bags_the_hop_lost: Default::default(),
            },
        );
    }

    /// Forget one local destination this ingress feeds, when its link is gone
    /// and other links keep the ingress alive.
    ///
    /// Everything [`Self::note_a_local_destination`] took, given back together:
    /// a destination that left must stop being notified as much as it must
    /// stop being charged for what the hop loses after it.
    pub(super) fn forget_a_local_destination(&self, link_id: &str) {
        // Keeping the channel: this ingress's publisher lives as long as the
        // ingress does, not as long as whichever links happen to be wired, and
        // releasing it here would leave the ingress writing into nothing while
        // still reporting itself as carrying the port.
        self.writes_onto_the_local_channel
            .remove_channel_link_keeping_the_channel(THE_INGRESS_OUTPUT_PORT, link_id);
        self.every_link_it_feeds.lock().remove(link_id);
    }
}

impl Drop for MeshLinkIngress {
    fn drop(&mut self) {
        // The reader token first: it is what tells the source to stop sending,
        // and undeclaring it before the subscriber goes means the last bags in
        // flight still land rather than arriving at a torn-down ring.
        if let Some(held) = self.held_on_the_mesh.as_mut() {
            if let Some(reader_token) = held.reader_token.take() {
                if let Err(undeclare_failure) = reader_token.undeclare().wait() {
                    tracing::debug!(
                        "this runtime's reader token for {} did not undeclare, so the source \
                         keeps sending until its lease runs out: {undeclare_failure}",
                        self.address
                    );
                }
            }
        }
        drop(self.held_on_the_mesh.take());

        {
            let (arrived, someone_is_waiting) = &*self.arrived;
            arrived.lock().the_ingress_is_stopping = true;
            someone_is_waiting.notify_all();
        }
        if let Some(writing_thread) = self.writing_thread.take() {
            if writing_thread.join().is_err() {
                tracing::warn!("the mesh ingress thread for {} panicked", self.address);
            }
        }
        tracing::info!("This runtime stopped reading {}", self.address);
    }
}

/// The subscriber that takes the port's bags off the mesh. Its callback only
/// hands off into the ring.
fn declare_the_data_subscriber(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    address: &MeshPortAddress,
    arrived: &Arc<(Mutex<WhatHasArrivedFromTheMesh>, Condvar)>,
) -> crate::core::Result<zenoh::pubsub::Subscriber<()>> {
    let arrived = Arc::clone(arrived);
    let addressed = address.to_string();
    session
        .declare_subscriber(key_space.data_key(
            &address.runtime_name(),
            &address.processor_display_name(),
            &address.port_name(),
        ))
        .callback(move |sample| {
            // A message this engine did not write, or one from a build whose
            // attachment this one cannot read, carries no stamp to pass on and
            // is read past rather than written with a stamp of zero.
            let Some(attached) = sample.attachment().and_then(|attached| {
                MeshDataMessageAttachment::from_wire_bytes(&attached.to_bytes())
            }) else {
                tracing::debug!(
                    "a message on {addressed} carried no readable attachment and was read past"
                );
                return;
            };
            let (arrived, someone_is_waiting) = &*arrived;
            let mut arrived = arrived.lock();
            arrived.take_this_one_in(ABagOffTheMesh {
                payload_bytes: sample.payload().to_bytes().into_owned(),
                attached,
            });
            someone_is_waiting.notify_one();
        })
        .wait()
        .map_err(|declare_failure| {
            crate::core::Error::Runtime(format!(
                "this runtime could not subscribe to {address} on the mesh: {declare_failure}"
            ))
        })
}

/// The subscriber that watches whether the source is sending this port.
fn declare_the_egress_token_subscriber(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    address: &MeshPortAddress,
    the_source_is_sending: &Arc<SourceSendingState>,
    wake_the_resolver: crossbeam_channel::Sender<()>,
) -> crate::core::Result<zenoh::pubsub::Subscriber<()>> {
    let the_source_is_sending = Arc::clone(the_source_is_sending);
    session
        .liveliness()
        .declare_subscriber(key_space.egress_token_key(
            &address.runtime_name(),
            &address.processor_display_name(),
            &address.port_name(),
        ))
        .history(true)
        .callback(move |token| {
            match token.kind() {
                zenoh::sample::SampleKind::Put => {
                    the_source_is_sending.note_that_it_started_sending()
                }
                zenoh::sample::SampleKind::Delete => {
                    the_source_is_sending.note_that_it_stopped_sending()
                }
            }
            // Whether the source is sending decides whether the link reads
            // `wired`, so the pass runs the moment that changes rather than on
            // the next tick. A hand-off only: this is the link's receive loop.
            let _ = wake_the_resolver.send(());
        })
        .wait()
        .map_err(|declare_failure| {
            crate::core::Error::Runtime(format!(
                "this runtime could not watch whether {address} is being sent: {declare_failure}"
            ))
        })
}

/// The one thread that writes what arrived onto the local channel.
///
/// Its own thread because the write takes an iceoryx2 loan and the Zenoh
/// callback may not block, and because the stamp it carries is the producer's,
/// which must cross unchanged.
fn spawn_the_writing_thread(
    address: MeshPortAddress,
    arrived: Arc<(Mutex<WhatHasArrivedFromTheMesh>, Condvar)>,
    writes_onto_the_local_channel: Arc<OutputWriterInner>,
    every_link_it_feeds: Arc<Mutex<BTreeMap<String, OneLinkThisIngressFeeds>>>,
    machine_clock_it_carries_from: Arc<MachineClockARemoteLinkCarriesFrom>,
    mut writes_a_frames_pixels_into_a_local_surface: WritesAFramesPixelsIntoALocalSurface,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("streamlib-mesh-ingress".to_string())
        .spawn(move || {
            // Each reason a frame did not land, said once for this source:
            // one arriving thirty times a second must not say the same thing
            // thirty times a second, and two different reasons must both be
            // said.
            let mut said_why_a_frame_did_not_land: BTreeSet<&'static str> = BTreeSet::new();
            loop {
                let taken = {
                    let (arrived_ring, someone_is_waiting) = &*arrived;
                    let mut arrived_ring = arrived_ring.lock();
                    while arrived_ring.ring.is_empty() && !arrived_ring.the_ingress_is_stopping {
                        someone_is_waiting.wait(&mut arrived_ring);
                    }
                    match arrived_ring.take_the_oldest_out() {
                        Some(taken) => taken,
                        // Drained and stopping: every bag that arrived before
                        // the ingress was torn down has been written.
                        None => return,
                    }
                };
                let the_machine_it_was_stamped_on = machine_clock_it_carries_from
                    .note_the_machine_a_bag_was_stamped_on(taken.attached.clock_identity);
                if let WhatNotingABagsClockDid::ItNamedAnotherMachineThanBefore { until_this_bag } =
                    the_machine_it_was_stamped_on
                {
                    tracing::info!(
                        "{address} is carrying from another machine than it was — its stamps were \
                         taken on {until_this_bag} and are now taken on {}. Every link from it is \
                         wired afresh and counts from zero.",
                        taken.attached.clock_identity
                    );
                }
                charge_every_link_for_what_the_hop_lost_before(
                    &mut every_link_it_feeds.lock(),
                    &taken.attached,
                    the_machine_it_was_stamped_on,
                );

                let Some(bag_bytes) = the_bag_to_hand_downstream(
                    &address,
                    &taken,
                    &mut writes_a_frames_pixels_into_a_local_surface,
                    &mut said_why_a_frame_did_not_land,
                ) else {
                    // A frame that did not land is a bag this hop lost, on top
                    // of whatever the gap above already said: it reached this
                    // runtime and reaches no destination of it.
                    for link in every_link_it_feeds.lock().values_mut() {
                        link.where_its_hop_loss_is_counted.record_dropped_bags(1);
                    }
                    continue;
                };

                if let Err(write_failure) = writes_onto_the_local_channel.write_raw(
                    THE_INGRESS_OUTPUT_PORT,
                    &bag_bytes,
                    taken.attached.timestamp_ns,
                ) {
                    tracing::warn!(
                        "a bag from {address} did not reach its local channel: {write_failure}"
                    );
                }
            }
        })
}

/// Charge every link this ingress feeds for the bags the hop lost before this
/// one.
///
/// Run before the write and on the writing thread: the ring this bag came off
/// evicts its oldest under pressure, and those evictions are inside the jump
/// only because the numbers are read here rather than as each bag arrived. One
/// unbroken run per publisher generation the sending runtime carried, so a
/// replaced producer's restart is a baseline rather than the gap it looks like.
///
/// Per link rather than once for the ingress: what a gap costs is the same for
/// every link past its own baseline, but a link wired onto an ingress already
/// carrying has no baseline yet and must not be charged for what it was never
/// going to get.
///
/// A bag stamped on another machine than the one before it is a peer that came
/// back on a fresh boot, or another machine that took the name. Its stamps are
/// a new clock, so every link is wired afresh here: what each had counted
/// described a run of the port that has ended, and carrying the numbering over
/// would charge this bag for every bag of that run it never had.
fn charge_every_link_for_what_the_hop_lost_before(
    every_link_it_feeds: &mut BTreeMap<String, OneLinkThisIngressFeeds>,
    attached: &MeshDataMessageAttachment,
    the_machine_it_was_stamped_on: WhatNotingABagsClockDid,
) {
    let wired_afresh = matches!(
        the_machine_it_was_stamped_on,
        WhatNotingABagsClockDid::ItNamedAnotherMachineThanBefore { .. }
    );
    for link in every_link_it_feeds.values_mut() {
        if wired_afresh {
            link.bags_the_hop_lost = Default::default();
        }
        let lost_before_it = link
            .bags_the_hop_lost
            .how_many_were_lost_before(attached.publisher_generation, attached.sequence_number);
        if lost_before_it > 0 {
            link.where_its_hop_loss_is_counted
                .record_dropped_bags(lost_before_it);
        }
    }
}

/// The bag one arriving message is handed downstream as, or `None` when its
/// frame did not land here.
///
/// A message carrying no frame is its producer's bag, untouched. One carrying
/// a frame is that bag with its `surface_id` replaced by the local surface the
/// pixels were just written into — no surface id, lease or lifetime state
/// crosses, so the only id a destination here can resolve is one this runtime
/// minted.
fn the_bag_to_hand_downstream<'a>(
    address: &MeshPortAddress,
    taken: &'a ABagOffTheMesh,
    writes_a_frames_pixels_into_a_local_surface: &mut WritesAFramesPixelsIntoALocalSurface,
    said_why_a_frame_did_not_land: &mut BTreeSet<&'static str>,
) -> Option<std::borrow::Cow<'a, [u8]>> {
    let payload_bytes = &taken.payload_bytes;
    if taken.attached.frame_pixel_description_bytes == 0 {
        return Some(std::borrow::Cow::Borrowed(payload_bytes));
    }
    let landed = match a_frames_pixels_off_the_mesh(
        payload_bytes,
        taken.attached.frame_pixel_description_bytes,
    ) {
        Some(arrived) => writes_a_frames_pixels_into_a_local_surface
            .a_bag_naming_the_local_surface_this_frame_landed_in(&arrived),
        None => Err(WhyAFramesPixelsCannotLandHere::ItsMessageCouldNotBeRead {
            payload_bytes: payload_bytes.len(),
        }),
    };
    match landed {
        Ok(bag_bytes) => Some(std::borrow::Cow::Owned(bag_bytes)),
        Err(why_it_cannot_land) => {
            if said_why_a_frame_did_not_land.insert(why_it_cannot_land.which_refusal_this_is()) {
                tracing::warn!(
                    "a frame from {address} did not land on this runtime, and each one is \
                     counted against this link: {why_it_cannot_land}"
                );
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_message_of(payload_bytes: usize) -> ABagOffTheMesh {
        ABagOffTheMesh {
            payload_bytes: vec![0u8; payload_bytes],
            attached: MeshDataMessageAttachment {
                timestamp_ns: 0,
                sequence_number: 0,
                publisher_generation: PublisherGenerationOnTheMesh(0),
                clock_identity: crate::core::runtime::mesh::MachineClockIdentity::UNIDENTIFIED,
                frame_pixel_description_bytes: 0,
            },
        }
    }

    /// Bags are bounded by the depth, as they always were.
    #[test]
    fn the_ring_holds_no_more_messages_than_its_depth() {
        let mut arrived = WhatHasArrivedFromTheMesh::default();
        for _ in 0..DeliveryProfile::ORDERED_DEPTH * 3 {
            arrived.take_this_one_in(a_message_of(512));
        }
        assert_eq!(arrived.ring.len(), DeliveryProfile::ORDERED_DEPTH);
        assert_eq!(
            arrived.bytes_in_the_ring,
            512 * DeliveryProfile::ORDERED_DEPTH
        );
    }

    /// Frames are bounded by the bytes long before the depth: sixteen 1080p
    /// RGBA frames would be 133 MB, and the ring must never hold them.
    #[test]
    fn a_ring_of_frames_is_bounded_by_its_bytes_rather_than_by_its_depth() {
        const A_1080P_RGBA_FRAME: usize = 1920 * 1080 * 4;
        let mut arrived = WhatHasArrivedFromTheMesh::default();
        for _ in 0..DeliveryProfile::ORDERED_DEPTH * 2 {
            arrived.take_this_one_in(a_message_of(A_1080P_RGBA_FRAME));
        }
        assert!(
            arrived.ring.len() < DeliveryProfile::ORDERED_DEPTH,
            "frames must be evicted by the byte ceiling well before the depth is reached"
        );
        assert!(
            arrived.bytes_in_the_ring <= HOW_MANY_BYTES_ONE_INGRESS_RING_MAY_HOLD,
            "the ring holds {} bytes, over its {HOW_MANY_BYTES_ONE_INGRESS_RING_MAY_HOLD}-byte \
             ceiling",
            arrived.bytes_in_the_ring
        );
    }

    // ------------------------------------------------------------------------
    // What one arriving bag costs the links this ingress feeds
    // ------------------------------------------------------------------------

    const ONE_MACHINE: &str = "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93";
    const ANOTHER_MACHINE: &str = "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c";

    fn a_machine(boot_session_uuid: &str) -> crate::core::runtime::mesh::MachineClockIdentity {
        crate::core::runtime::mesh::MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
            boot_session_uuid,
        )
    }

    fn a_bag_numbered(
        sequence_number: u64,
        stamped_on: crate::core::runtime::mesh::MachineClockIdentity,
    ) -> MeshDataMessageAttachment {
        MeshDataMessageAttachment {
            timestamp_ns: 0,
            sequence_number,
            publisher_generation: PublisherGenerationOnTheMesh(0),
            clock_identity: stamped_on,
            frame_pixel_description_bytes: 0,
        }
    }

    /// One link an ingress feeds, and the counter its hop loss lands in.
    fn one_link_it_feeds() -> (
        BTreeMap<String, OneLinkThisIngressFeeds>,
        RemoteInboundLinkMeshHopDroppedBagCounter,
    ) {
        let where_its_hop_loss_is_counted = RemoteInboundLinkMeshHopDroppedBagCounter::default();
        let every_link_it_feeds = BTreeMap::from([(
            "the-link".to_string(),
            OneLinkThisIngressFeeds {
                where_its_hop_loss_is_counted: where_its_hop_loss_is_counted.clone(),
                bags_the_hop_lost: Default::default(),
            },
        )]);
        (every_link_it_feeds, where_its_hop_loss_is_counted)
    }

    /// The ordinary run: a gap in the sending runtime's numbering is what the
    /// hop lost, counted once on the link.
    #[test]
    fn a_gap_in_the_numbering_is_charged_to_the_link() {
        let (mut every_link_it_feeds, counted) = one_link_it_feeds();

        for sequence_number in [1, 2, 7] {
            charge_every_link_for_what_the_hop_lost_before(
                &mut every_link_it_feeds,
                &a_bag_numbered(sequence_number, a_machine(ONE_MACHINE)),
                WhatNotingABagsClockDid::ItNamedTheSameMachineAgain,
            );
        }

        assert_eq!(counted.dropped_bag_count(), 4, "bags 3, 4, 5 and 6");
    }

    /// The peer came back on another boot, so its numbering restarted with a
    /// clock that has nothing to do with the one before it. The bag that says
    /// so is a baseline: charging the difference would bill this link for a
    /// whole run of a port it never had.
    ///
    /// Fail-without-fix: drop the re-wire and this counts 1_000_000 lost bags
    /// the moment a rebooted peer's first bag lands.
    #[test]
    fn a_bag_from_another_machine_is_a_baseline_and_never_a_gap() {
        let (mut every_link_it_feeds, counted) = one_link_it_feeds();
        for sequence_number in [1_000_000, 1_000_001] {
            charge_every_link_for_what_the_hop_lost_before(
                &mut every_link_it_feeds,
                &a_bag_numbered(sequence_number, a_machine(ONE_MACHINE)),
                WhatNotingABagsClockDid::ItNamedTheSameMachineAgain,
            );
        }
        assert_eq!(counted.dropped_bag_count(), 0);

        charge_every_link_for_what_the_hop_lost_before(
            &mut every_link_it_feeds,
            &a_bag_numbered(1, a_machine(ANOTHER_MACHINE)),
            WhatNotingABagsClockDid::ItNamedAnotherMachineThanBefore {
                until_this_bag: a_machine(ONE_MACHINE),
            },
        );

        assert_eq!(
            counted.dropped_bag_count(),
            0,
            "the first bag of a new machine's run is where this link starts counting again"
        );
    }

    /// Counting picks up from the new machine's own numbering, so a gap after
    /// the re-wire is still a gap.
    #[test]
    fn the_hop_counts_the_new_machines_own_gaps_after_it_is_wired_afresh() {
        let (mut every_link_it_feeds, counted) = one_link_it_feeds();
        charge_every_link_for_what_the_hop_lost_before(
            &mut every_link_it_feeds,
            &a_bag_numbered(500, a_machine(ONE_MACHINE)),
            WhatNotingABagsClockDid::ItNamedTheMachineForTheFirstTime,
        );
        charge_every_link_for_what_the_hop_lost_before(
            &mut every_link_it_feeds,
            &a_bag_numbered(1, a_machine(ANOTHER_MACHINE)),
            WhatNotingABagsClockDid::ItNamedAnotherMachineThanBefore {
                until_this_bag: a_machine(ONE_MACHINE),
            },
        );

        charge_every_link_for_what_the_hop_lost_before(
            &mut every_link_it_feeds,
            &a_bag_numbered(4, a_machine(ANOTHER_MACHINE)),
            WhatNotingABagsClockDid::ItNamedTheSameMachineAgain,
        );

        assert_eq!(counted.dropped_bag_count(), 2, "bags 2 and 3");
    }

    /// A message over the ceiling on its own is still taken: a port whose every
    /// frame is over it must carry its frames rather than none of them.
    #[test]
    fn a_message_over_the_ceiling_on_its_own_is_still_taken() {
        let mut arrived = WhatHasArrivedFromTheMesh::default();
        arrived.take_this_one_in(a_message_of(HOW_MANY_BYTES_ONE_INGRESS_RING_MAY_HOLD * 2));
        assert_eq!(arrived.ring.len(), 1);

        arrived.take_this_one_in(a_message_of(HOW_MANY_BYTES_ONE_INGRESS_RING_MAY_HOLD * 2));
        assert_eq!(
            arrived.ring.len(),
            1,
            "the one before it is evicted rather than held beside it"
        );
    }

    /// What the ring says it holds is what it holds, across every eviction —
    /// a count that drifted would either wedge the ring shut or stop bounding
    /// it at all.
    #[test]
    fn the_rings_byte_count_follows_every_take_in_and_every_take_out() {
        let mut arrived = WhatHasArrivedFromTheMesh::default();
        for payload_bytes in [1, 1024, 4 * 1024 * 1024, 16, 8 * 1024 * 1024] {
            arrived.take_this_one_in(a_message_of(payload_bytes));
        }
        let held: usize = arrived.ring.iter().map(|one| one.payload_bytes.len()).sum();
        assert_eq!(arrived.bytes_in_the_ring, held);

        while arrived.take_the_oldest_out().is_some() {
            let still_held: usize = arrived.ring.iter().map(|one| one.payload_bytes.len()).sum();
            assert_eq!(arrived.bytes_in_the_ring, still_held);
        }
        assert_eq!(arrived.bytes_in_the_ring, 0);
    }
}
