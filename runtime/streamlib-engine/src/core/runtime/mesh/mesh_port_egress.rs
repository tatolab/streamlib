// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Sending one of this runtime's output ports to the mesh.
//!
//! A runtime does no network work and copies no frame for a port until a remote
//! link reads it: an egress exists only while at least one other runtime holds a
//! reader token for its port, and with none it holds no subscriber, no
//! publisher and no token of its own.
//!
//! It takes one ordinary destination slot on the port's channel — counted like
//! any other destination — and drains it FIFO on its own OS thread, because an
//! iceoryx2 subscriber is `!Send` and because a Zenoh put blocks its caller
//! while a fragmented message queues. No producer ever waits on the network:
//! the put runs here, never on the thread that wrote the bag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use zenoh::Wait;
use zenoh::qos::{CongestionControl, Priority};

use crate::core::runtime::mesh::a_bags_top_level_surface_id::a_bag_carries_a_top_level_surface_id;
use crate::core::runtime::mesh::mesh_data_message_attachment::MeshDataMessageAttachment;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::HowToReadAnOfferedOutputPort;
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::iceoryx2::{ChannelIdlePollBackoff, FRAME_HEADER_SIZE, FrameHeader, Iceoryx2Node};

/// One of this runtime's output ports, being sent to the mesh.
///
/// Dropping it stops the thread, drops the channel subscriber and undeclares
/// the egress token — which is what tells every reader the port stopped being
/// sent.
pub(super) struct MeshPortEgress {
    processor_display_name: String,
    port_name: String,
    stop: Arc<AtomicBool>,
    sending_thread: Option<std::thread::JoinHandle<()>>,
}

/// Everything one egress thread needs, gathered so the spawn reads as one
/// thing rather than eight arguments.
pub(super) struct WhatOneEgressSends {
    pub session: zenoh::Session,
    pub key_space: RuntimeMeshKeySpace,
    pub this_runtimes_name: String,
    pub processor_display_name: String,
    pub port_name: String,
    pub how_to_read_the_port: HowToReadAnOfferedOutputPort,
    pub iceoryx2_node: Iceoryx2Node,
}

impl MeshPortEgress {
    /// Start sending one port, or say why it could not start.
    pub(super) fn start(sending: WhatOneEgressSends) -> std::io::Result<Self> {
        let processor_display_name = sending.processor_display_name.clone();
        let port_name = sending.port_name.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_the_thread = Arc::clone(&stop);
        let sending_thread = std::thread::Builder::new()
            .name("streamlib-mesh-egress".to_string())
            .spawn(move || send_one_port_to_the_mesh(sending, stop_for_the_thread))?;
        Ok(Self {
            processor_display_name,
            port_name,
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
                tracing::warn!(
                    "the mesh egress thread for {}/{} panicked",
                    self.processor_display_name,
                    self.port_name
                );
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
        this_runtimes_name,
        processor_display_name,
        port_name,
        how_to_read_the_port,
        iceoryx2_node,
    } = sending;
    let addressed = format!("{this_runtimes_name}/{processor_display_name}/{port_name}");

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

    while !stop.load(Ordering::Acquire) {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                idle_poll_backoff.reset_after_a_bag_arrived();
                let framed = sample.payload();
                if framed.len() < FRAME_HEADER_SIZE {
                    continue;
                }
                let stamp = FrameHeader::read_from_slice(&framed[..FRAME_HEADER_SIZE]).timestamp_ns;
                let bag_bytes = &framed[FRAME_HEADER_SIZE..];
                let names_a_surface = a_bag_carries_a_top_level_surface_id(bag_bytes);

                // One priority for the egress's life, decided by its first bag:
                // two priorities are two QUIC streams, which would reorder one
                // port's sequence and read downstream as gaps.
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

                let attached = MeshDataMessageAttachment {
                    timestamp_ns: stamp,
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
