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

use crate::core::graph::MeshPortAddress;
use crate::core::runtime::mesh::mesh_port_egress::{MeshPortEgress, WhatOneEgressSends};
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::{
    OutputPortOfferedOnTheMesh, WhatThisRuntimeOffersOnTheMeshRegistry,
};
use crate::core::runtime::mesh::output_ports_other_runtimes_are_reading::OutputPortsOtherRuntimesAreReading;
use crate::core::runtime::mesh::runtime_mesh_key::{ReaderOfAnOutputPort, RuntimeMeshKeySpace};
use crate::iceoryx2::Iceoryx2Node;

/// What the reader-token subscriber tells the egress thread.
enum WhatTheReadersDid {
    ARuntimeStartedReading(ReaderOfAnOutputPort),
    ARuntimeStoppedReading(ReaderOfAnOutputPort),
}

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
        being_read: &Arc<OutputPortsOtherRuntimesAreReading>,
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
            Arc::clone(being_read),
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
    being_read: Arc<OutputPortsOtherRuntimesAreReading>,
    what_the_egress_thread_reads: Receiver<WhatTheReadersDid>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("streamlib-mesh-egress-table".to_string())
        .spawn(move || {
            let mut who_is_reading: BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>> =
                BTreeMap::new();
            let mut sending: BTreeMap<OutputPortOfferedOnTheMesh, MeshPortEgress> = BTreeMap::new();

            // Ends when the subscriber is dropped, which drops the sender.
            while let Ok(did) = what_the_egress_thread_reads.recv() {
                match did {
                    WhatTheReadersDid::ARuntimeStartedReading(reader) => {
                        a_runtime_started_reading(
                            AnEgressTablesOwnState {
                                session: &session,
                                key_space: &key_space,
                                this_runtimes_name: &this_runtimes_name,
                                offered: &offered,
                                iceoryx2_node: &iceoryx2_node,
                            },
                            &mut who_is_reading,
                            &mut sending,
                            &reader,
                        );
                    }
                    WhatTheReadersDid::ARuntimeStoppedReading(reader) => {
                        a_runtime_stopped_reading(&mut who_is_reading, &mut sending, &reader);
                    }
                }
                being_read.record_what_is_being_sent(what_this_runtime_is_sending(
                    &who_is_reading,
                    &sending,
                ));
            }
            // The thread ends with the session, so nothing is being sent any
            // more; leaving the last state behind would have `graph` report
            // egresses whose thread is gone.
            being_read.record_what_is_being_sent(BTreeMap::new());
        })
}

/// What starting one egress reads, gathered so the thread body stays flat.
struct AnEgressTablesOwnState<'a> {
    session: &'a zenoh::Session,
    key_space: &'a RuntimeMeshKeySpace,
    this_runtimes_name: &'a str,
    offered: &'a Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
    iceoryx2_node: &'a Iceoryx2Node,
}

/// Note one more reader of a port, and start sending it if it is the first.
fn a_runtime_started_reading(
    table: AnEgressTablesOwnState<'_>,
    who_is_reading: &mut BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>,
    sending: &mut BTreeMap<OutputPortOfferedOnTheMesh, MeshPortEgress>,
    reader: &ReaderOfAnOutputPort,
) {
    let port = OutputPortOfferedOnTheMesh::from(reader);
    if !who_is_reading
        .entry(port.clone())
        .or_default()
        .insert(reader.reading_runtime_name.clone())
    {
        return;
    }
    if sending.contains_key(&port) {
        return;
    }
    // A port this runtime cannot send — one it does not have, or one whose
    // channel it cannot open. Said rather than passed over: the reader wired
    // against the offered-ports answer and will wait on an egress token that
    // never comes, and this log is where the reason is. Why it could not open
    // is said by the opener.
    let Some(how_to_read_the_port) = table
        .offered
        .how_to_read_an_offered_output_port(&port.processor_display_name, &port.port_name)
    else {
        tracing::warn!(
            "{} is reading {port} and this runtime cannot send it, so that link waits on an \
             egress that never starts",
            reader.reading_runtime_name
        );
        return;
    };
    let addressed = match MeshPortAddress::new(
        table.this_runtimes_name,
        &port.processor_display_name,
        &port.port_name,
    ) {
        Ok(addressed) => addressed,
        Err(not_an_address) => {
            tracing::warn!("the mesh cannot send {port}: {not_an_address}");
            return;
        }
    };
    match MeshPortEgress::start(WhatOneEgressSends {
        session: table.session.clone(),
        key_space: table.key_space.clone(),
        addressed,
        how_to_read_the_port,
        iceoryx2_node: table.iceoryx2_node.clone(),
    }) {
        Ok(egress) => {
            sending.insert(port, egress);
        }
        Err(cannot_spawn) => tracing::warn!(
            "the mesh could not start sending {port} for want of a thread: {cannot_spawn}"
        ),
    }
}

/// Note one fewer reader of a port, and stop sending it once the last leaves.
fn a_runtime_stopped_reading(
    who_is_reading: &mut BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>,
    sending: &mut BTreeMap<OutputPortOfferedOnTheMesh, MeshPortEgress>,
    reader: &ReaderOfAnOutputPort,
) {
    let port = OutputPortOfferedOnTheMesh::from(reader);
    let Some(readers) = who_is_reading.get_mut(&port) else {
        return;
    };
    readers.remove(&reader.reading_runtime_name);
    if readers.is_empty() {
        who_is_reading.remove(&port);
        // Dropping the egress stops its thread, drops its channel subscriber
        // and undeclares its token.
        sending.remove(&port);
    }
}

/// The readers of every port that actually has an egress.
///
/// Derived from both halves rather than kept as a third map: a port somebody
/// reads and this runtime cannot send has readers and no egress, and `graph`
/// must say this runtime sends nothing for it.
fn what_this_runtime_is_sending(
    who_is_reading: &BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>,
    sending: &BTreeMap<OutputPortOfferedOnTheMesh, MeshPortEgress>,
) -> BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>> {
    sending
        .keys()
        .map(|port| {
            (
                port.clone(),
                who_is_reading.get(port).cloned().unwrap_or_default(),
            )
        })
        .collect()
}
