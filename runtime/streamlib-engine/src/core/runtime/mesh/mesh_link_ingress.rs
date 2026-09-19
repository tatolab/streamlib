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

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{Condvar, Mutex};
use zenoh::Wait;

use crate::core::graph::MeshPortAddress;
use crate::core::runtime::mesh::mesh_data_message_attachment::MeshDataMessageAttachment;
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::iceoryx2::{
    ChannelEgressConfig, ChannelTrustTier, DEFAULT_EXPECTED_PAYLOAD_BYTES, DeliveryProfile,
    Iceoryx2Node, OutputWriterInner, RemoteInboundLinkMeshHopDroppedBagCounter,
    effective_channel_chunk_ceiling_bytes, mesh_ingress_channel_name,
};

/// The one output port name the ingress publishes under on its local channel.
///
/// Engine-derived like the channel itself: destinations route by the subscriber
/// they bound, never by the frame header's port key, so this names nothing a
/// reader has to know.
const THE_INGRESS_OUTPUT_PORT: &str = "bags";

/// A bag as it arrives from the mesh, before the writing thread takes it.
struct ABagOffTheMesh {
    bag_bytes: Vec<u8>,
    timestamp_ns: i64,
    /// The number the producing publisher gave this bag on the sending
    /// runtime, and which of that port's publishers gave it — carried this far
    /// so the writing thread can see what the hop lost ahead of it.
    sequence_number: u64,
    publisher_generation: u64,
}

/// The last bag the writing thread took off the ring, as the sending runtime
/// numbered it.
#[derive(Clone, Copy)]
struct LastBagTakenOffTheMesh {
    publisher_generation: u64,
    sequence_number: u64,
}

/// What the hop lost, read off jumps in the sending runtime's own numbering.
///
/// One slot rather than one per generation: an egress sends one port's samples
/// in the order it received them, so a bag of any other generation is a new
/// baseline either way. The same shape the local subscriber's ring-overwrite
/// count already uses, for the same reason.
#[derive(Default)]
struct BagsTheHopLostBeforeEachArrival {
    last: Option<LastBagTakenOffTheMesh>,
}

impl BagsTheHopLostBeforeEachArrival {
    /// How many bags the hop lost before `arriving`, remembering it as the
    /// last.
    ///
    /// The first bag of all, and the first of a generation this ingress has not
    /// seen, is a baseline and never a gap: a publisher numbers its own sends
    /// from zero, so a recreated producer's numbering says nothing about the
    /// one before it.
    fn how_many_the_hop_lost_before(&mut self, arriving: &ABagOffTheMesh) -> u64 {
        let lost = match self.last {
            Some(last) if last.publisher_generation == arriving.publisher_generation => arriving
                .sequence_number
                .saturating_sub(last.sequence_number)
                .saturating_sub(1),
            _ => 0,
        };
        self.last = Some(LastBagTakenOffTheMesh {
            publisher_generation: arriving.publisher_generation,
            sequence_number: arriving.sequence_number,
        });
        lost
    }
}

/// The ring between the Zenoh callback and the writing thread.
///
/// As deep as an `ordered` port's own ring: the hop is not a second place to
/// buffer, and a consumer that cannot keep up must lose bags here rather than
/// grow a queue the producer is paying for.
#[derive(Default)]
struct WhatHasArrivedFromTheMesh {
    ring: VecDeque<ABagOffTheMesh>,
    the_ingress_is_stopping: bool,
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
    where_every_link_it_feeds_counts_hop_loss:
        Arc<Mutex<Vec<RemoteInboundLinkMeshHopDroppedBagCounter>>>,
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
    pub(super) fn start(
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        address: &MeshPortAddress,
        iceoryx2_node: &Iceoryx2Node,
        wake_the_resolver: crossbeam_channel::Sender<()>,
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

        let where_every_link_it_feeds_counts_hop_loss = Arc::new(Mutex::new(Vec::new()));
        let writing_thread = spawn_the_writing_thread(
            address.clone(),
            Arc::clone(&arrived),
            Arc::clone(&writes_onto_the_local_channel),
            Arc::clone(&where_every_link_it_feeds_counts_hop_loss),
        )?;

        tracing::info!("This runtime is reading {address} off the mesh into {local_channel}");
        Ok(Self {
            address: address.clone(),
            writes_onto_the_local_channel,
            the_source_is_sending,
            arrived,
            where_every_link_it_feeds_counts_hop_loss,
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
        self.where_every_link_it_feeds_counts_hop_loss
            .lock()
            .push(where_its_hop_loss_is_counted);
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
            if arrived.ring.len() >= DeliveryProfile::ORDERED_DEPTH {
                arrived.ring.pop_front();
            }
            arrived.ring.push_back(ABagOffTheMesh {
                bag_bytes: sample.payload().to_bytes().into_owned(),
                timestamp_ns: attached.timestamp_ns,
                sequence_number: attached.sequence_number,
                publisher_generation: attached.publisher_generation,
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
    where_every_link_it_feeds_counts_hop_loss: Arc<
        Mutex<Vec<RemoteInboundLinkMeshHopDroppedBagCounter>>,
    >,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("streamlib-mesh-ingress".to_string())
        .spawn(move || {
            let mut bags_the_hop_lost = BagsTheHopLostBeforeEachArrival::default();
            loop {
                let taken = {
                    let (arrived_ring, someone_is_waiting) = &*arrived;
                    let mut arrived_ring = arrived_ring.lock();
                    while arrived_ring.ring.is_empty() && !arrived_ring.the_ingress_is_stopping {
                        someone_is_waiting.wait(&mut arrived_ring);
                    }
                    match arrived_ring.ring.pop_front() {
                        Some(taken) => taken,
                        // Drained and stopping: every bag that arrived before
                        // the ingress was torn down has been written.
                        None => return,
                    }
                };
                // Before the write, and on this thread: the ring this bag
                // came off evicts its oldest under pressure, and those
                // evictions are inside the jump only because the numbers are
                // read here rather than as each bag arrived.
                let lost_before_it = bags_the_hop_lost.how_many_the_hop_lost_before(&taken);
                if lost_before_it > 0 {
                    for counter in where_every_link_it_feeds_counts_hop_loss.lock().iter() {
                        counter.record_dropped_bags(lost_before_it);
                    }
                }

                if let Err(write_failure) = writes_onto_the_local_channel.write_raw(
                    THE_INGRESS_OUTPUT_PORT,
                    &taken.bag_bytes,
                    taken.timestamp_ns,
                ) {
                    tracing::warn!(
                        "a bag from {address} did not reach its local channel: {write_failure}"
                    );
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_bag_numbered(sequence_number: u64, publisher_generation: u64) -> ABagOffTheMesh {
        ABagOffTheMesh {
            bag_bytes: Vec::new(),
            timestamp_ns: 0,
            sequence_number,
            publisher_generation,
        }
    }

    /// What the sequence number a run of bags skipped says the hop lost.
    fn what_the_hop_lost_carrying(numbered: &[(u64, u64)]) -> Vec<u64> {
        let mut bags_the_hop_lost = BagsTheHopLostBeforeEachArrival::default();
        numbered
            .iter()
            .map(|(sequence_number, publisher_generation)| {
                bags_the_hop_lost.how_many_the_hop_lost_before(&a_bag_numbered(
                    *sequence_number,
                    *publisher_generation,
                ))
            })
            .collect()
    }

    /// An unbroken run loses nothing, and the first bag of all is a baseline
    /// rather than everything the producer sent before this link existed.
    #[test]
    fn an_unbroken_run_loses_nothing_and_its_first_bag_is_a_baseline() {
        assert_eq!(
            what_the_hop_lost_carrying(&[(500, 0), (501, 0), (502, 0)]),
            [0, 0, 0]
        );
    }

    /// A jump names exactly the bags between the two that arrived — the
    /// arithmetic the whole count rests on.
    #[test]
    fn a_jump_counts_exactly_the_bags_between_the_two_that_arrived() {
        assert_eq!(
            what_the_hop_lost_carrying(&[(0, 0), (1, 0), (5, 0), (6, 0), (100, 0)]),
            [0, 0, 3, 0, 93]
        );
    }

    /// A recreated producer numbers from zero, and its first bag is a baseline
    /// rather than a gap — even when its numbering has already overtaken the
    /// one before it, which is the case the generation exists for. Without it
    /// the jump from 4 to 7 would read as two lost bags that were never sent.
    #[test]
    fn a_recreated_producer_is_a_baseline_even_once_its_numbering_has_overtaken() {
        assert_eq!(
            what_the_hop_lost_carrying(&[(3, 0), (4, 0), (7, 1), (8, 1)]),
            [0, 0, 0, 0]
        );
        assert_eq!(
            what_the_hop_lost_carrying(&[(3, 0), (4, 0), (0, 1), (1, 1)]),
            [0, 0, 0, 0]
        );
    }

    /// A gap inside the replacement's own run still counts: a new generation
    /// resets the baseline, it does not stop the counting.
    #[test]
    fn a_gap_after_a_recreated_producer_is_still_counted() {
        assert_eq!(
            what_the_hop_lost_carrying(&[(9, 0), (0, 1), (4, 1)]),
            [0, 0, 3]
        );
    }

    /// A number that does not advance — a duplicate, or one arriving behind
    /// its successor — reads as no loss rather than as an enormous one out of
    /// an unsigned subtraction that went below zero.
    #[test]
    fn a_number_that_does_not_advance_reads_as_no_loss() {
        assert_eq!(
            what_the_hop_lost_carrying(&[(7, 0), (7, 0), (3, 0), (4, 0)]),
            [0, 0, 0, 0]
        );
    }

    /// The counting survives the numbering wrapping: a publisher's number is a
    /// `u64` that wraps, and the wrap must not read as the whole range lost.
    #[test]
    fn the_numbering_wrapping_is_not_read_as_the_whole_range_lost() {
        assert_eq!(
            what_the_hop_lost_carrying(&[(u64::MAX - 1, 0), (u64::MAX, 0), (0, 0), (1, 0)]),
            [0, 0, 0, 0]
        );
    }
}
