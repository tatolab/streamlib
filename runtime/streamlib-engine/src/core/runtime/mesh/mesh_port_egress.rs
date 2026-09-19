// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Sending one of this runtime's output ports to the mesh.
//!
//! A runtime does no network work and copies no frame for a port until a remote
//! link reads it: an egress exists only while at least one other runtime holds a
//! reader token for its port, and with none it holds no subscriber, no
//! publisher and no token of its own.
//!
//! It takes a subscriber slot on the port's channel and drains it FIFO on its
//! own OS thread. The slot is its own reservation rather than one of the
//! destination cap's: counting it there would make a port already feeding its
//! cap fail to send across the mesh, and fail on the *sending* machine, where
//! the runtime that asked for the link cannot see it. Its own thread, because an
//! iceoryx2 subscriber is `!Send` and because a Zenoh put blocks its caller
//! while a fragmented message queues. No producer ever waits on the network:
//! the put runs here, never on the thread that wrote the bag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use iceoryx2::identifiers::UniquePublisherId;
use zenoh::Wait;
use zenoh::qos::{CongestionControl, Priority};

use crate::core::graph::MeshPortAddress;
use crate::core::runtime::mesh::a_bags_top_level_surface_id::a_bag_carries_a_top_level_surface_id;
use crate::core::runtime::mesh::machine_clock_identity::MachineClockIdentity;
use crate::core::runtime::mesh::mesh_data_message_attachment::MeshDataMessageAttachment;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::HowToReadAnOfferedOutputPort;
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::iceoryx2::{ChannelIdlePollBackoff, FRAME_HEADER_SIZE, FrameHeader, Iceoryx2Node};

/// How many times the port's publisher has been replaced under one egress.
///
/// A publisher numbers its own sends from zero, so a replaced one restarts the
/// numbering and the next number the reading runtime sees is unrelated to the
/// last. The generation is what tells the two apart there: a bag whose
/// generation differs from the last one's is a baseline rather than a gap, so
/// a producer recreated mid-stream is never read as loss.
#[derive(Default)]
struct PublisherGenerationsOnePortHasHad {
    /// The publisher that numbered the last sample this egress sent; `None`
    /// until the first.
    numbering_publisher_id: Option<UniquePublisherId>,
    generation: u64,
}

impl PublisherGenerationsOnePortHasHad {
    /// The generation a sample numbered by `numbering_publisher_id` carries,
    /// bumping when that publisher is not the one that numbered the last.
    ///
    /// The first sample of all takes generation zero rather than bumping onto
    /// one: there is no earlier numbering for it to be told apart from.
    fn generation_of_a_sample_numbered_by(
        &mut self,
        numbering_publisher_id: UniquePublisherId,
    ) -> u64 {
        match self.numbering_publisher_id {
            Some(last) if last != numbering_publisher_id => {
                self.generation = self.generation.wrapping_add(1)
            }
            _ => {}
        }
        self.numbering_publisher_id = Some(numbering_publisher_id);
        self.generation
    }
}

/// One of this runtime's output ports, being sent to the mesh.
///
/// Dropping it stops the thread, drops the channel subscriber and undeclares
/// the egress token — which is what tells every reader the port stopped being
/// sent.
pub(super) struct MeshPortEgress {
    addressed: MeshPortAddress,
    stop: Arc<AtomicBool>,
    sending_thread: Option<std::thread::JoinHandle<()>>,
}

/// Everything one egress thread needs, gathered so the spawn reads as one
/// thing rather than eight arguments.
pub(super) struct WhatOneEgressSends {
    pub session: zenoh::Session,
    pub key_space: RuntimeMeshKeySpace,
    /// The port this egress sends, spelled the one way a port is addressed on
    /// the mesh — so a log line here and the address a reader `connect`ed with
    /// can never read differently.
    pub addressed: MeshPortAddress,
    pub how_to_read_the_port: HowToReadAnOfferedOutputPort,
    pub iceoryx2_node: Iceoryx2Node,
}

impl MeshPortEgress {
    /// Start sending one port, or say why it could not start.
    pub(super) fn start(sending: WhatOneEgressSends) -> std::io::Result<Self> {
        let addressed = sending.addressed.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_the_thread = Arc::clone(&stop);
        let sending_thread = std::thread::Builder::new()
            .name("streamlib-mesh-egress".to_string())
            .spawn(move || send_one_port_to_the_mesh(sending, stop_for_the_thread))?;
        Ok(Self {
            addressed,
            stop,
            sending_thread: Some(sending_thread),
        })
    }
}

impl Drop for MeshPortEgress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(sending_thread) = self.sending_thread.take() {
            if sending_thread.join().is_err() {
                tracing::warn!("the mesh egress thread for {} panicked", self.addressed);
            }
        }
    }
}

/// The body of one egress thread: take a destination slot on the port's
/// channel, say on the mesh that the port is being sent, and put every bag.
fn send_one_port_to_the_mesh(sending: WhatOneEgressSends, stop: Arc<AtomicBool>) {
    let WhatOneEgressSends {
        session,
        key_space,
        addressed,
        how_to_read_the_port,
        iceoryx2_node,
    } = sending;
    let this_runtimes_name = addressed.runtime_name();
    let processor_display_name = addressed.processor_display_name();
    let port_name = addressed.port_name();

    let service = match iceoryx2_node.open_or_create_service(
        &how_to_read_the_port.channel_service_name,
        how_to_read_the_port.channel_sizing.max_subscribers,
        how_to_read_the_port
            .channel_sizing
            .channel_service_creation_depth,
    ) {
        Ok(service) => service,
        Err(open_failure) => {
            tracing::warn!(
                "the mesh cannot send {addressed}: its channel \
                 {} did not open: {open_failure}",
                how_to_read_the_port.channel_service_name
            );
            return;
        }
    };
    let subscriber = match service.create_subscriber(
        how_to_read_the_port
            .channel_sizing
            .channel_service_creation_depth,
    ) {
        Ok(subscriber) => subscriber,
        Err(subscribe_failure) => {
            tracing::warn!(
                "the mesh cannot send {addressed}: it could not take a destination slot on \
                 {}: {subscribe_failure}",
                how_to_read_the_port.channel_service_name
            );
            return;
        }
    };

    // Declared after the subscriber, so a reader that sees the token and starts
    // counting is never counting against a port nothing is draining yet.
    let egress_token = match session
        .liveliness()
        .declare_token(key_space.egress_token_key(
            &this_runtimes_name,
            &processor_display_name,
            &port_name,
        ))
        .wait()
    {
        Ok(token) => token,
        Err(declare_failure) => {
            tracing::warn!(
                "the mesh cannot say that {addressed} is being sent, so a reader would never \
                 learn it stopped: {declare_failure}"
            );
            return;
        }
    };

    tracing::info!("The mesh is sending {addressed}");
    let data_key = key_space.data_key(&this_runtimes_name, &processor_display_name, &port_name);
    let mut publisher: Option<zenoh::pubsub::Publisher<'_>> = None;
    let mut said_a_surface_will_not_cross = false;
    let mut idle_poll_backoff = ChannelIdlePollBackoff::starting_at_the_shortest_sleep();
    let mut publisher_generations = PublisherGenerationsOnePortHasHad::default();
    // Read once: a boot id cannot change without a reboot, which ends this
    // process, and this rides every bag.
    let clock_identity = MachineClockIdentity::of_this_machine();

    while !stop.load(Ordering::Acquire) {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                idle_poll_backoff.reset_after_a_bag_arrived();
                let framed = sample.payload();
                if framed.len() < FRAME_HEADER_SIZE {
                    continue;
                }
                let stamp = FrameHeader::read_from_slice(&framed[..FRAME_HEADER_SIZE]).timestamp_ns;
                // The engine's own number for this bag, carried end to end
                // rather than re-minted here: a gap the reading runtime counts
                // then covers this channel's ring and the bags this egress
                // never sent, and not only what the network lost.
                let sequence_number = sample.user_header().sequence_number;
                let publisher_generation =
                    publisher_generations.generation_of_a_sample_numbered_by(sample.origin());
                let bag_bytes = &framed[FRAME_HEADER_SIZE..];
                let names_a_surface = a_bag_carries_a_top_level_surface_id(bag_bytes);

                if names_a_surface {
                    if !said_a_surface_will_not_cross {
                        said_a_surface_will_not_cross = true;
                        tracing::warn!(
                            "{addressed} publishes bags naming a surface, and a surface id names \
                             a frame in this machine's own pools — nothing another runtime can \
                             resolve. Those bags are not sent, and are counted nowhere until the \
                             mesh carries the pixels themselves."
                        );
                    }
                    continue;
                }

                // One priority for the egress's life, decided by the first bag
                // it actually sends: two priorities are two QUIC streams, which
                // would reorder one port's sequence and read downstream as
                // gaps. The rule is the change file's — `DataLow` for a bag
                // naming a surface, `Data` otherwise — and until #2290 carries
                // a frame's pixels no surface bag crosses, so the `DataLow` arm
                // has no live input and this always declares `Data`. Declaring
                // it above the skip instead would read the surface bag that is
                // then thrown away, and put every ordinary bag behind it.
                let publisher = match publisher.as_ref() {
                    Some(publisher) => publisher,
                    None => match declare_the_publisher(&session, &data_key, names_a_surface) {
                        Ok(declared) => publisher.insert(declared),
                        Err(declare_failure) => {
                            tracing::warn!(
                                "the mesh cannot send {addressed}: its publisher did not \
                                 declare: {declare_failure}"
                            );
                            break;
                        }
                    },
                };

                let attached = MeshDataMessageAttachment {
                    timestamp_ns: stamp,
                    sequence_number,
                    publisher_generation,
                    clock_identity,
                }
                .to_wire_bytes();
                if let Err(put_failure) = publisher
                    .put(bag_bytes)
                    .attachment(attached.to_vec())
                    .wait()
                {
                    tracing::warn!("a bag on {addressed} did not reach the mesh: {put_failure}");
                }
            }
            Ok(None) => {
                std::thread::sleep(idle_poll_backoff.sleep_this_empty_poll_earns(Instant::now()))
            }
            Err(receive_failure) => {
                tracing::warn!(
                    "the mesh stopped sending {addressed}: its channel subscriber failed: \
                     {receive_failure:?}"
                );
                break;
            }
        }
    }

    // Undeclared explicitly rather than left to the drop, so a reader sees the
    // port stop the moment it does rather than when the session's lease runs
    // out.
    if let Err(undeclare_failure) = egress_token.undeclare().wait() {
        tracing::debug!(
            "the mesh's token for {addressed} did not undeclare, so a reader learns it stopped \
             when this runtime's connections close instead: {undeclare_failure}"
        );
    }
    tracing::info!("The mesh stopped sending {addressed}");
}

/// Declare the publisher one egress puts on, at the priority its first bag
/// earns and at `Drop`, because no link ever blocks a producer.
fn declare_the_publisher<'a>(
    session: &'a zenoh::Session,
    data_key: &str,
    its_first_bag_named_a_surface: bool,
) -> zenoh::Result<zenoh::pubsub::Publisher<'a>> {
    // Raw frames ride below every other bag, and requests, queries and tokens
    // ride above both, so a 1080p frame never delays a link request or an audio
    // block.
    let priority = if its_first_bag_named_a_surface {
        Priority::DataLow
    } else {
        Priority::Data
    };
    session
        .declare_publisher(data_key.to_string())
        .priority(priority)
        .congestion_control(CongestionControl::Drop)
        .wait()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One port's publisher and its replacement, which is the only way to get
    /// two `UniquePublisherId`s — iceoryx2 mints them and nothing else can.
    ///
    /// One after the other rather than both at once: a channel carries a
    /// single publisher, which is exactly why a replacement's numbering
    /// restarts and the generation has to say so.
    fn a_ports_publisher_and_its_replacement(arm: &str) -> (UniquePublisherId, UniquePublisherId) {
        let channel = Iceoryx2Node::for_this_test_process()
            .open_or_create_service(
                &format!("egress-generations-{arm}-{}", std::process::id()),
                2,
                4,
            )
            .expect("a test channel");
        let first = channel.create_publisher(64).expect("a publisher");
        let first_id = first.id();
        drop(first);
        let replacement = channel
            .create_publisher(64)
            .expect("a replacement publisher");
        let replacement_id = replacement.id();
        assert_ne!(
            first_id, replacement_id,
            "a replacement must be told apart from what it replaced, or the generation says \
             nothing"
        );
        (first_id, replacement_id)
    }

    /// One publisher's whole run is one generation: bumping inside it would
    /// make the reading runtime treat every bag as a baseline and count no
    /// loss at all.
    #[test]
    fn one_publishers_run_is_one_generation_and_the_first_bag_is_generation_zero() {
        let (numbering_publisher_id, _) = a_ports_publisher_and_its_replacement("one-run");
        let mut generations = PublisherGenerationsOnePortHasHad::default();

        let carried: Vec<u64> = (0..4)
            .map(|_| generations.generation_of_a_sample_numbered_by(numbering_publisher_id))
            .collect();

        assert_eq!(carried, [0, 0, 0, 0]);
    }

    /// A replaced publisher is a new generation, which is what tells the
    /// reading runtime that the numbering restarted rather than jumped.
    #[test]
    fn a_replaced_publisher_is_a_new_generation() {
        let (first, second) = a_ports_publisher_and_its_replacement("replaced");
        let mut generations = PublisherGenerationsOnePortHasHad::default();

        assert_eq!(generations.generation_of_a_sample_numbered_by(first), 0);
        assert_eq!(generations.generation_of_a_sample_numbered_by(first), 0);
        assert_eq!(generations.generation_of_a_sample_numbered_by(second), 1);
        assert_eq!(generations.generation_of_a_sample_numbered_by(second), 1);
        assert_eq!(
            generations.generation_of_a_sample_numbered_by(first),
            2,
            "a publisher coming back is a third generation, never the first again: its \
             numbering restarted at zero the second time too"
        );
    }
}
