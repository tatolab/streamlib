// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which of this runtime's output ports other runtimes are reading.
//!
//! A runtime watches the reader tokens under its own name. The first reader of
//! an output port it actually has creates that port's egress; the last reader
//! leaving removes it. Nothing else creates or removes one, which is what makes
//! "a sending runtime does no network work for a port until a remote link reads
//! it" true rather than merely intended.
//!
//! The liveliness callback only hands off: it runs on the link's receive loop,
//! and blocking there would stall every key arriving from that peer. The work —
//! opening an iceoryx2 subscriber, declaring a token — happens on this module's
//! own thread.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use zenoh::Wait;
use zenoh::sample::SampleKind;

use crate::core::runtime::mesh::mesh_port_egress::{MeshPortEgress, WhatOneEgressSends};
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::WhatThisRuntimeOffersOnTheMeshRegistry;
use crate::core::runtime::mesh::runtime_mesh_key::{ReaderOfAnOutputPort, RuntimeMeshKeySpace};
use crate::iceoryx2::Iceoryx2Node;

/// What the reader-token subscriber tells the egress thread.
enum WhatTheReadersDid {
    ARuntimeStartedReading(ReaderOfAnOutputPort),
    ARuntimeStoppedReading(ReaderOfAnOutputPort),
}

/// One of this runtime's output ports, as the readers address it.
type OnePortsAddress = (String, String);

/// This runtime's egresses, and the subscriber that decides which exist.
///
/// Dropping it drops the subscriber, which ends the thread, which drops every
/// egress — so a runtime leaving the mesh stops sending everything at once.
pub(super) struct MeshPortEgressTable {
    reader_token_subscriber: Option<zenoh::pubsub::Subscriber<()>>,
    egress_thread: Option<std::thread::JoinHandle<()>>,
}

impl MeshPortEgressTable {
    /// Watch the reader tokens under `this_runtimes_name` and keep one egress
    /// per port somebody is reading.
    pub(super) fn watching_the_readers_of_this_runtimes_ports(
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        offered: &Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
        iceoryx2_node: &Iceoryx2Node,
    ) -> zenoh::Result<Self> {
        let (what_the_readers_did, what_the_egress_thread_reads) = crossbeam_channel::unbounded();
        let reader_token_subscriber = declare_the_reader_token_subscriber(
            session,
            key_space,
            this_runtimes_name,
            what_the_readers_did,
        )?;
        let egress_thread = spawn_the_egress_thread(
            session.clone(),
            key_space.clone(),
            this_runtimes_name.to_string(),
            Arc::clone(offered),
            iceoryx2_node.clone(),
            what_the_egress_thread_reads,
        )?;
        Ok(Self {
            reader_token_subscriber: Some(reader_token_subscriber),
            egress_thread: Some(egress_thread),
        })
    }
}

impl Drop for MeshPortEgressTable {
    fn drop(&mut self) {
        // The subscriber first: dropping it drops the callback that holds the
        // egress thread's sender, which is what ends that thread.
        drop(self.reader_token_subscriber.take());
        if let Some(egress_thread) = self.egress_thread.take() {
            if egress_thread.join().is_err() {
                tracing::warn!("the mesh egress thread panicked; its ports may still be sending");
            }
        }
    }
}

/// The subscriber that hands every reader token arriving and leaving to the
/// egress thread. It runs on Zenoh's own threads, so it only sends.
fn declare_the_reader_token_subscriber(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    this_runtimes_name: &str,
    what_the_readers_did: Sender<WhatTheReadersDid>,
) -> zenoh::Result<zenoh::pubsub::Subscriber<()>> {
    let key_space = key_space.clone();
    session
        .liveliness()
        .declare_subscriber(key_space.every_reader_token_of(this_runtimes_name))
        .history(true)
        .callback(move |token| {
            let Some(reader) = key_space.read_a_reader_token_key(token.key_expr().as_str()) else {
                return;
            };
            let did = match token.kind() {
                SampleKind::Put => WhatTheReadersDid::ARuntimeStartedReading(reader),
                SampleKind::Delete => WhatTheReadersDid::ARuntimeStoppedReading(reader),
            };
            let _ = what_the_readers_did.send(did);
        })
        .wait()
}

/// The one thread that owns this runtime's egresses.
fn spawn_the_egress_thread(
    session: zenoh::Session,
    key_space: RuntimeMeshKeySpace,
    this_runtimes_name: String,
    offered: Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
    iceoryx2_node: Iceoryx2Node,
    what_the_egress_thread_reads: Receiver<WhatTheReadersDid>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("streamlib-mesh-egress-table".to_string())
        .spawn(move || {
            let mut who_is_reading: BTreeMap<OnePortsAddress, BTreeSet<String>> = BTreeMap::new();
            let mut sending: BTreeMap<OnePortsAddress, MeshPortEgress> = BTreeMap::new();

            // Ends when the subscriber is dropped, which drops the sender.
            while let Ok(did) = what_the_egress_thread_reads.recv() {
                match did {
                    WhatTheReadersDid::ARuntimeStartedReading(reader) => {
                        let address = (
                            reader.processor_display_name.clone(),
                            reader.port_name.clone(),
                        );
                        if !who_is_reading
                            .entry(address.clone())
                            .or_default()
                            .insert(reader.reading_runtime_name.clone())
                        {
                            continue;
                        }
                        if sending.contains_key(&address) {
                            continue;
                        }
                        // Only a port this runtime actually has: a reader
                        // naming one it does not gets its refusal from the
                        // offered-ports query it asked before it wired, and
                        // nothing is created for it here.
                        let Some(how_to_read_the_port) =
                            offered.how_to_read_an_offered_output_port(&address.0, &address.1)
                        else {
                            tracing::debug!(
                                "{} is reading {}/{}, which this runtime does not offer; nothing \
                                 is sent for it",
                                reader.reading_runtime_name,
                                address.0,
                                address.1
                            );
                            continue;
                        };
                        match MeshPortEgress::start(WhatOneEgressSends {
                            session: session.clone(),
                            key_space: key_space.clone(),
                            this_runtimes_name: this_runtimes_name.clone(),
                            processor_display_name: address.0.clone(),
                            port_name: address.1.clone(),
                            how_to_read_the_port,
                            iceoryx2_node: iceoryx2_node.clone(),
                        }) {
                            Ok(egress) => {
                                sending.insert(address, egress);
                            }
                            Err(cannot_spawn) => tracing::warn!(
                                "the mesh could not start sending {}/{} for want of a thread: \
                                 {cannot_spawn}",
                                address.0,
                                address.1
                            ),
                        }
                    }
                    WhatTheReadersDid::ARuntimeStoppedReading(reader) => {
                        let address = (
                            reader.processor_display_name.clone(),
                            reader.port_name.clone(),
                        );
                        let Some(readers) = who_is_reading.get_mut(&address) else {
                            continue;
                        };
                        readers.remove(&reader.reading_runtime_name);
                        if readers.is_empty() {
                            who_is_reading.remove(&address);
                            // Dropping the egress stops its thread, drops its
                            // channel subscriber and undeclares its token.
                            sending.remove(&address);
                        }
                    }
                }
            }
        })
}
