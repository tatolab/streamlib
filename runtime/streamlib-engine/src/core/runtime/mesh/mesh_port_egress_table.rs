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
use std::sync::{Arc, Weak};

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

/// What the reader-token subscriber, and an egress of this runtime's own,
/// tell the egress thread.
pub(super) enum WhatTheReadersDid {
    ARuntimeStartedReading(ReaderOfAnOutputPort),
    ARuntimeStoppedReading(ReaderOfAnOutputPort),
    /// One egress ended without ever sending anything — because it could not
    /// start, having said why on its own thread, or because it was cancelled
    /// and has nothing to say. Sent by the egress; nothing else sends it.
    AnEgressGaveUpOnItsPort {
        port: OutputPortOfferedOnTheMesh,
        which_egress_of_its_port_it_was: WhichEgressOfAPortThisIs,
    },
}

/// Which egress of one port an egress is — the table's own count, minted when
/// it starts it.
///
/// A port's egress is dropped and started again as its last reader leaves and
/// another arrives, and an egress being dropped can still say it gave up, since
/// the wait it is inside ends on the same flag the drop sets. Without a name for
/// which one is speaking, that message removes the egress that replaced it and
/// the port goes unsendable for the rest of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WhichEgressOfAPortThisIs(u64);

/// What an egress answers when asked which one of its port it is.
///
/// A trait so the table's bookkeeping is provable without a Zenoh session and
/// an iceoryx2 subscriber to own, the same reason
/// [`what_this_runtime_is_sending`] is generic.
pub(super) trait SaysWhichEgressOfItsPortItIs {
    fn which_egress_of_its_port_it_is(&self) -> WhichEgressOfAPortThisIs;
}

/// The table's running count of the egresses it has started, which is where
/// every [`WhichEgressOfAPortThisIs`] comes from.
#[derive(Default)]
struct HowManyEgressesThisTableHasStarted(u64);

impl HowManyEgressesThisTableHasStarted {
    fn the_next_egress(&mut self) -> WhichEgressOfAPortThisIs {
        self.0 += 1;
        WhichEgressOfAPortThisIs(self.0)
    }
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
        being_read_by_other_runtimes: &Arc<OutputPortsOtherRuntimesAreReading>,
    ) -> zenoh::Result<Self> {
        let (what_the_readers_did, what_the_egress_thread_reads) = crossbeam_channel::unbounded();
        // The subscriber owns the only sender that keeps the thread alive, and
        // an egress reports through a `Weak` of it: one that kept a sender
        // would be a sender the egress thread owns through its own map, and
        // that thread ends when every sender is dropped.
        let what_the_readers_did = Arc::new(what_the_readers_did);
        let how_an_egress_reports_giving_up = Arc::downgrade(&what_the_readers_did);
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
            Arc::clone(being_read_by_other_runtimes),
            what_the_egress_thread_reads,
            how_an_egress_reports_giving_up,
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
    what_the_readers_did: Arc<Sender<WhatTheReadersDid>>,
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
    being_read_by_other_runtimes: Arc<OutputPortsOtherRuntimesAreReading>,
    what_the_egress_thread_reads: Receiver<WhatTheReadersDid>,
    how_an_egress_reports_giving_up: Weak<Sender<WhatTheReadersDid>>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("streamlib-mesh-egress-table".to_string())
        .spawn(move || {
            let mut who_is_reading: BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>> =
                BTreeMap::new();
            let mut sending: BTreeMap<OutputPortOfferedOnTheMesh, MeshPortEgress> = BTreeMap::new();
            let mut how_many_egresses_this_table_has_started =
                HowManyEgressesThisTableHasStarted::default();

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
                                how_an_egress_reports_giving_up: &how_an_egress_reports_giving_up,
                            },
                            &mut who_is_reading,
                            &mut sending,
                            &mut how_many_egresses_this_table_has_started,
                            &reader,
                        );
                    }
                    WhatTheReadersDid::ARuntimeStoppedReading(reader) => {
                        a_runtime_stopped_reading(&mut who_is_reading, &mut sending, &reader);
                    }
                    WhatTheReadersDid::AnEgressGaveUpOnItsPort {
                        port,
                        which_egress_of_its_port_it_was,
                    } => {
                        an_egress_gave_up_on_its_port(
                            &mut sending,
                            &port,
                            which_egress_of_its_port_it_was,
                        );
                    }
                }
                being_read_by_other_runtimes.record_what_is_being_sent(
                    what_this_runtime_is_sending(&who_is_reading, &sending),
                );
            }
            // The thread ends with the session, so nothing is being sent any
            // more; leaving the last state behind would have `graph` report
            // egresses whose thread is gone.
            being_read_by_other_runtimes.record_what_is_being_sent(BTreeMap::new());
        })
}

/// What starting one egress reads, gathered so the thread body stays flat.
struct AnEgressTablesOwnState<'a> {
    session: &'a zenoh::Session,
    key_space: &'a RuntimeMeshKeySpace,
    this_runtimes_name: &'a str,
    offered: &'a Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
    iceoryx2_node: &'a Iceoryx2Node,
    how_an_egress_reports_giving_up: &'a Weak<Sender<WhatTheReadersDid>>,
}

/// Note one more reader of a port, and start sending it if it is the first.
fn a_runtime_started_reading(
    table: AnEgressTablesOwnState<'_>,
    who_is_reading: &mut BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>,
    sending: &mut BTreeMap<OutputPortOfferedOnTheMesh, MeshPortEgress>,
    how_many_egresses_this_table_has_started: &mut HowManyEgressesThisTableHasStarted,
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
        where_this_egress_says_it_gave_up: table.how_an_egress_reports_giving_up.clone(),
        which_egress_of_this_port_this_is: how_many_egresses_this_table_has_started
            .the_next_egress(),
    }) {
        Ok(egress) => {
            sending.insert(port, egress);
        }
        Err(cannot_spawn) => tracing::warn!(
            "the mesh could not start sending {port} for want of a thread: {cannot_spawn}"
        ),
    }
}

/// Forget an egress that stopped before it ever sent anything, so `graph` stops
/// claiming the port is being sent and a later reader starts a fresh one.
///
/// Only if the port still holds the egress that spoke: one that was cancelled
/// reaches the same message, its wait ending on the flag its drop set, and by
/// then the port may hold the egress that replaced it.
///
/// The egress said on its own thread why it gave up, so nothing is logged here.
/// The readers are left alone: they are still reading, and what changed is only
/// that this runtime is not answering them — the next reader token to arrive
/// starts a fresh egress, which is the whole of the recovery. A reader already
/// in the table when the egress it waited on gave up waits for that, rather than
/// this retrying against a source that just refused.
fn an_egress_gave_up_on_its_port<AnEgress: SaysWhichEgressOfItsPortItIs>(
    sending: &mut BTreeMap<OutputPortOfferedOnTheMesh, AnEgress>,
    port: &OutputPortOfferedOnTheMesh,
    which_egress_of_its_port_it_was: WhichEgressOfAPortThisIs,
) {
    let it_is_still_the_ports_egress = sending.get(port).is_some_and(|egress| {
        egress.which_egress_of_its_port_it_is() == which_egress_of_its_port_it_was
    });
    if it_is_still_the_ports_egress {
        sending.remove(port);
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
///
/// Generic over what an egress *is* because this reads only which ports have
/// one — which is also what lets the divergence be tested without standing up
/// a Zenoh session and an iceoryx2 subscriber to own.
fn what_this_runtime_is_sending<AnEgress>(
    who_is_reading: &BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>,
    sending: &BTreeMap<OutputPortOfferedOnTheMesh, AnEgress>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn a_port(processor_display_name: &str, port_name: &str) -> OutputPortOfferedOnTheMesh {
        OutputPortOfferedOnTheMesh {
            processor_display_name: processor_display_name.to_string(),
            port_name: port_name.to_string(),
        }
    }

    fn reading(runtime_names: &[&str]) -> BTreeSet<String> {
        runtime_names.iter().map(|it| it.to_string()).collect()
    }

    /// An egress that is only its identity, which is all the table's
    /// bookkeeping ever reads off one.
    struct AnEgressThatIsOnlyItsIdentity(WhichEgressOfAPortThisIs);

    impl SaysWhichEgressOfItsPortItIs for AnEgressThatIsOnlyItsIdentity {
        fn which_egress_of_its_port_it_is(&self) -> WhichEgressOfAPortThisIs {
            self.0
        }
    }

    /// The egresses a table hands out, in the order it starts them.
    fn the_first_two_egresses_a_table_starts()
    -> (AnEgressThatIsOnlyItsIdentity, AnEgressThatIsOnlyItsIdentity) {
        let mut started = HowManyEgressesThisTableHasStarted::default();
        (
            AnEgressThatIsOnlyItsIdentity(started.the_next_egress()),
            AnEgressThatIsOnlyItsIdentity(started.the_next_egress()),
        )
    }

    /// A port somebody reads and this runtime cannot send renders nothing.
    ///
    /// The two maps really do diverge: `a_runtime_started_reading` notes the
    /// reader before it discovers the port is not one this runtime offers, and
    /// then starts no egress. `graph` must not read that as a send.
    ///
    /// Mental-revert: derive from `who_is_reading` instead and this runtime
    /// claims to be sending a port it has no publisher for.
    #[test]
    fn a_port_with_readers_and_no_egress_is_not_being_sent() {
        let who_is_reading = BTreeMap::from([
            (a_port("CameraSource", "video"), reading(&["bench-fx-c3d4"])),
            (
                a_port("NoSuchProcessor", "video"),
                reading(&["bench-rec-e5f6"]),
            ),
        ]);
        let sending = BTreeMap::from([(a_port("CameraSource", "video"), ())]);

        assert_eq!(
            what_this_runtime_is_sending(&who_is_reading, &sending),
            BTreeMap::from([(a_port("CameraSource", "video"), reading(&["bench-fx-c3d4"]))]),
        );
    }

    /// An egress that gave up stops being rendered as a send, and its port is
    /// free for a later reader to start a fresh one.
    ///
    /// What it catches: an egress discovers on its own thread that it cannot
    /// send — a helper-placed source's publisher refused, or never opened — and
    /// the table holds it either way, because `MeshPortEgress::start` succeeds
    /// the moment the thread spawns. Left in `sending`, `graph.mesh.egress_ports`
    /// asserts this runtime is sending a port nothing publishes to, and
    /// `a_runtime_started_reading` returns early on the port it already has, so
    /// no later reader can replace it.
    #[test]
    fn an_egress_that_gave_up_stops_being_rendered_and_frees_its_port() {
        let port = a_port("KnownAudioSignalSource", "audio");
        let who_is_reading = BTreeMap::from([(port.clone(), reading(&["bench-rec-e5f6"]))]);
        let (the_one_that_gave_up, _) = the_first_two_egresses_a_table_starts();
        let which_one_it_was = the_one_that_gave_up.which_egress_of_its_port_it_is();
        let mut sending = BTreeMap::from([(port.clone(), the_one_that_gave_up)]);

        an_egress_gave_up_on_its_port(&mut sending, &port, which_one_it_was);

        assert!(
            what_this_runtime_is_sending(&who_is_reading, &sending).is_empty(),
            "the reader is still reading, and this runtime must say it sends nothing for it"
        );
        assert!(
            !sending.contains_key(&port),
            "a port still in `sending` is one no later reader can start an egress for"
        );
    }

    /// A cancelled egress's message never reaches the egress that replaced it.
    ///
    /// What it catches: a reader reconnecting puts Delete then Put on one
    /// liveliness key, so the table drops the port's egress and starts a fresh
    /// one; the dropped one's wait ends on the flag that drop set, and it says
    /// it gave up behind them both. Removed by port alone, that message takes
    /// down the healthy egress and the port is unsendable for the rest of the
    /// run — #2344's own symptom, restored for the reconnect case.
    ///
    /// Mental-revert: drop the identity check in `an_egress_gave_up_on_its_port`
    /// and this goes red.
    #[test]
    fn a_cancelled_egresss_give_up_never_removes_the_one_that_replaced_it() {
        let port = a_port("KnownAudioSignalSource", "audio");
        let (the_cancelled_one, the_one_that_replaced_it) = the_first_two_egresses_a_table_starts();
        let which_one_was_cancelled = the_cancelled_one.which_egress_of_its_port_it_is();
        let mut sending = BTreeMap::from([(port.clone(), the_one_that_replaced_it)]);

        an_egress_gave_up_on_its_port(&mut sending, &port, which_one_was_cancelled);

        assert!(
            sending.contains_key(&port),
            "the port's egress is a different one, and it is sending"
        );
    }

    /// An egress whose readers have all gone still renders, with none — the
    /// window between the last reader leaving and the egress being dropped.
    #[test]
    fn an_egress_whose_readers_have_gone_renders_with_no_readers() {
        let sending = BTreeMap::from([(a_port("CameraSource", "video"), ())]);

        assert_eq!(
            what_this_runtime_is_sending(&BTreeMap::new(), &sending),
            BTreeMap::from([(a_port("CameraSource", "video"), BTreeSet::new())]),
        );
    }

    /// Every reader of one port arrives together, so `graph` names all of them.
    #[test]
    fn a_port_two_runtimes_read_names_both() {
        let port = a_port("CameraSource", "video");
        let who_is_reading =
            BTreeMap::from([(port.clone(), reading(&["bench-fx-c3d4", "bench-rec-e5f6"]))]);
        let sending = BTreeMap::from([(port.clone(), ())]);

        assert_eq!(
            what_this_runtime_is_sending(&who_is_reading, &sending)[&port],
            reading(&["bench-fx-c3d4", "bench-rec-e5f6"]),
        );
    }
}
