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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use iceoryx2::identifiers::UniquePublisherId;
use zenoh::Wait;
use zenoh::qos::{CongestionControl, Priority};

use crate::core::graph::MeshPortAddress;
use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::runtime::mesh::a_bags_top_level_surface_id::a_bag_carries_a_top_level_surface_id;
use crate::core::runtime::mesh::machine_clock_identity::MachineClockIdentity;
use crate::core::runtime::mesh::mesh_data_message_attachment::{
    MeshDataMessageAttachment, PublisherGenerationOnTheMesh,
};
use crate::core::runtime::mesh::mesh_port_egress_table::{
    SaysWhichEgressOfItsPortItIs, WhatTheReadersDid, WhichEgressOfAPortThisIs,
};
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::{
    HowToReadAnOfferedOutputPort, OutputPortOfferedOnTheMesh,
};
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::iceoryx2::{
    ChannelDataServiceSubscriber, ChannelIdlePollBackoff, FRAME_HEADER_SIZE, FrameHeader,
    Iceoryx2Node,
};

/// How long a helper-placed source has to open the publisher its parent asked
/// it for before this egress gives up on the port. Engine-chosen; nothing
/// authorable.
///
/// The helper's own registration budget, because that is what this can be
/// waiting on: a reader arriving while the child is still inside `setup()` is
/// waiting out a user module's import and its `setup()` hook, which the spawn
/// host already allows sixty seconds for. A shorter budget here would give up
/// on helpers the engine itself has not given up on. It bounds nothing else —
/// a helper that dies has every answer it owed refused at once, and one past
/// setup answers between callbacks.
const HOW_LONG_A_HELPER_HAS_TO_OPEN_ITS_PUBLISHER: Duration = Duration::from_secs(60);

/// How often that wait looks at the answer cell, which the helper's bridge
/// reader thread fills.
const HOW_OFTEN_A_HELPERS_ANSWER_IS_LOOKED_AT: Duration = Duration::from_millis(10);

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
        if let Some(last) = self.numbering_publisher_id
            && last != numbering_publisher_id
        {
            self.generation = self.generation.wrapping_add(1);
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
    which_egress_of_its_port_it_is: WhichEgressOfAPortThisIs,
    stop: Arc<AtomicBool>,
    sending_thread: Option<std::thread::JoinHandle<()>>,
}

impl SaysWhichEgressOfItsPortItIs for MeshPortEgress {
    fn which_egress_of_its_port_it_is(&self) -> WhichEgressOfAPortThisIs {
        self.which_egress_of_its_port_it_is
    }
}

/// Everything one egress thread needs, gathered so the spawn reads as one
/// thing rather than eight arguments.
pub(super) struct WhatOneEgressSends {
    /// Where this egress says it stopped before it ever sent anything, so its
    /// table stops claiming the port is being sent.
    ///
    /// Weak because the table's own sender is what decides its thread's life,
    /// and this end is owned by that same thread through its map of egresses.
    pub where_this_egress_says_it_gave_up: Weak<crossbeam_channel::Sender<WhatTheReadersDid>>,
    /// Which egress of this port this one is, so what it says about itself
    /// never reaches the egress that replaced it.
    pub which_egress_of_this_port_this_is: WhichEgressOfAPortThisIs,
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
        let which_egress_of_its_port_it_is = sending.which_egress_of_this_port_this_is;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_the_thread = Arc::clone(&stop);
        let sending_thread = std::thread::Builder::new()
            .name("streamlib-mesh-egress".to_string())
            .spawn(move || send_one_port_to_the_mesh(sending, stop_for_the_thread))?;
        Ok(Self {
            addressed,
            which_egress_of_its_port_it_is,
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

/// Wait for a helper-placed source to say it opened the publisher its parent
/// asked it for, reporting whether it did.
///
/// A refusal and a silence are both said by name here, because this is the one
/// side that knows a reader is waiting on the port: with no egress the reader's
/// link stays `awaiting_remote`, and without this line nothing would say why.
fn a_helper_opened_its_publisher(
    the_helpers_answer: &OutOfProcessLinkWireReply,
    addressed: &MeshPortAddress,
    stop: &AtomicBool,
) -> bool {
    let gave_up_at = Instant::now() + HOW_LONG_A_HELPER_HAS_TO_OPEN_ITS_PUBLISHER;
    while !stop.load(Ordering::Acquire) {
        match the_helpers_answer.the_far_sides_answer() {
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide) => return true,
            Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason }) => {
                tracing::warn!(
                    "the mesh cannot send {addressed}: the helper process its processor runs in \
                     could not open the port's publisher: {reason}"
                );
                return false;
            }
            None => {
                if Instant::now() >= gave_up_at {
                    tracing::warn!(
                        "the mesh cannot send {addressed}: the helper process its processor runs \
                         in did not open the port's publisher within \
                         {HOW_LONG_A_HELPER_HAS_TO_OPEN_ITS_PUBLISHER:?}"
                    );
                    return false;
                }
                std::thread::sleep(HOW_OFTEN_A_HELPERS_ANSWER_IS_LOOKED_AT);
            }
        }
    }
    false
}

/// Take a destination slot on the port's channel, once whatever publishes that
/// port has opened — or `None` when this egress never starts, said by name.
///
/// Everything an egress does before it declares its liveliness token, and
/// nothing that needs a Zenoh session: the ordering this holds — no token
/// until a helper-placed source says it opened its publisher — is then
/// provable without standing a session up.
fn take_a_destination_slot_once_the_port_publishes(
    how_to_read_the_port: &HowToReadAnOfferedOutputPort,
    addressed: &MeshPortAddress,
    iceoryx2_node: &Iceoryx2Node,
    stop: &AtomicBool,
) -> Option<ChannelDataServiceSubscriber> {
    if let Some(the_helpers_answer) = how_to_read_the_port
        .the_helpers_answer_that_it_opened_its_publisher
        .as_deref()
        && !a_helper_opened_its_publisher(the_helpers_answer, addressed, stop)
    {
        return None;
    }

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
                "the mesh cannot send {addressed}: its channel {} did not open: {open_failure}",
                how_to_read_the_port.channel_service_name
            );
            return None;
        }
    };
    match service.create_subscriber(
        how_to_read_the_port
            .channel_sizing
            .channel_service_creation_depth,
    ) {
        Ok(subscriber) => Some(subscriber),
        Err(subscribe_failure) => {
            tracing::warn!(
                "the mesh cannot send {addressed}: it could not take a destination slot on \
                 {}: {subscribe_failure}",
                how_to_read_the_port.channel_service_name
            );
            None
        }
    }
}

/// The body of one egress thread: take a destination slot on the port's
/// channel, say on the mesh that the port is being sent, and put every bag.
fn send_one_port_to_the_mesh(sending: WhatOneEgressSends, stop: Arc<AtomicBool>) {
    let WhatOneEgressSends {
        where_this_egress_says_it_gave_up,
        which_egress_of_this_port_this_is,
        session,
        key_space,
        addressed,
        how_to_read_the_port,
        iceoryx2_node,
    } = sending;
    let this_runtimes_name = addressed.runtime_name();
    let processor_display_name = addressed.processor_display_name();
    let port_name = addressed.port_name();

    let subscriber = take_a_destination_slot_once_the_port_publishes(
        &how_to_read_the_port,
        &addressed,
        &iceoryx2_node,
        &stop,
    );
    // Declared after the subscriber, so a reader that sees the token and starts
    // counting is never counting against a port nothing is draining yet.
    let egress_token = subscriber.as_ref().and_then(|_| {
        session
            .liveliness()
            .declare_token(key_space.egress_token_key(
                &this_runtimes_name,
                &processor_display_name,
                &port_name,
            ))
            .wait()
            .inspect_err(|declare_failure| {
                tracing::warn!(
                    "the mesh cannot say that {addressed} is being sent, so a reader would never \
                     learn it stopped: {declare_failure}"
                )
            })
            .ok()
    });
    let (Some(subscriber), Some(egress_token)) = (subscriber, egress_token) else {
        // Said back rather than only logged: the table holds this egress, and
        // one it goes on holding is one `graph` reports as a port being sent
        // and one no later reader can replace. Named with which egress of the
        // port this is, because a cancelled one reaches here too — its wait
        // ends on `stop` — and by then the table may already hold the egress
        // that replaced it.
        if let Some(where_this_egress_says_it_gave_up) = where_this_egress_says_it_gave_up.upgrade()
        {
            let _ = where_this_egress_says_it_gave_up.send(
                WhatTheReadersDid::AnEgressGaveUpOnItsPort {
                    port: OutputPortOfferedOnTheMesh {
                        processor_display_name: processor_display_name.to_string(),
                        port_name: port_name.to_string(),
                    },
                    which_egress_of_its_port_it_was: which_egress_of_this_port_this_is,
                },
            );
        }
        return;
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
                             resolve. Those bags are not sent, and each one reads on the reading \
                             runtime as a bag this hop lost, which from that side is what it is. \
                             The mesh carrying the pixels themselves is what ends both."
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
                    publisher_generation: PublisherGenerationOnTheMesh(publisher_generation),
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

    fn a_port_addressed_on_the_mesh() -> MeshPortAddress {
        MeshPortAddress::new("a-runtime", "AProcessor", "out1").expect("a legal mesh address")
    }

    /// A helper that opened its publisher lets the egress carry on, and one
    /// that refused stops it — so a reader never sees the port declared sent
    /// by a runtime whose producer never opened.
    #[test]
    fn an_egress_starts_on_a_helpers_yes_and_stops_on_its_no() {
        let opened = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        opened.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        let refused = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        refused.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: "its setup did not succeed".to_string(),
        });
        let addressed = a_port_addressed_on_the_mesh();
        let never_stopped = AtomicBool::new(false);

        assert!(a_helper_opened_its_publisher(
            &opened,
            &addressed,
            &never_stopped
        ));
        assert!(!a_helper_opened_its_publisher(
            &refused,
            &addressed,
            &never_stopped
        ));
    }

    /// An egress dropped while it is still waiting stops waiting, rather than
    /// holding its thread for the rest of the helper's budget.
    #[test]
    fn an_egress_stopped_while_it_waits_gives_up_at_once() {
        let never_answered = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        let stopped = AtomicBool::new(true);

        assert!(!a_helper_opened_its_publisher(
            &never_answered,
            &a_port_addressed_on_the_mesh(),
            &stopped
        ));
    }

    /// How to read a port whose publisher a helper was asked for and has not
    /// answered about, on a channel name of this test's own.
    fn a_port_published_by_a_helper(
        arm: &str,
        the_helpers_answer: &Arc<OutOfProcessLinkWireReply>,
    ) -> HowToReadAnOfferedOutputPort {
        HowToReadAnOfferedOutputPort {
            channel_service_name: format!("egress-waits-{arm}-{}", std::process::id()),
            channel_sizing: crate::iceoryx2::ChannelSizing {
                max_subscribers: 2,
                channel_service_creation_depth: 4,
            },
            the_helpers_answer_that_it_opened_its_publisher: Some(Arc::clone(the_helpers_answer)),
        }
    }

    /// A helper that refused leaves the egress holding nothing at all — no
    /// destination slot, and not even a channel service.
    ///
    /// Mental-revert: take the wait out of
    /// `take_a_destination_slot_once_the_port_publishes` and this goes red on
    /// the slot it then takes, which is the slot the liveliness token is
    /// declared behind. Nothing else in CI reaches that ordering — the
    /// two-process proof hand-rolls the offering trait and answers `None`, and
    /// the end-to-end arm is rig-only.
    #[test]
    fn a_refused_helper_leaves_the_egress_holding_no_slot_and_no_channel() {
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();
        let the_helpers_answer = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        the_helpers_answer.note_the_far_sides_answer(
            OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: "its setup did not succeed".to_string(),
            },
        );
        let how_to_read_the_port = a_port_published_by_a_helper("refused", &the_helpers_answer);

        let took_a_slot = take_a_destination_slot_once_the_port_publishes(
            &how_to_read_the_port,
            &a_port_addressed_on_the_mesh(),
            &iceoryx2_node,
            &AtomicBool::new(false),
        );

        assert!(
            took_a_slot.is_none(),
            "a port whose publisher was refused is a port this runtime cannot send"
        );
        assert!(
            iceoryx2_node
                .open_existing_channel_service(&how_to_read_the_port.channel_service_name)
                .expect("asking whether the service exists succeeds")
                .is_none(),
            "the channel is not even created: the wait comes before it, so a refusal costs \
             nothing"
        );
    }

    /// A helper that opened its publisher lets the egress take its slot, on the
    /// sizing it was told — the other half of the same ordering.
    #[test]
    fn an_opened_helper_publisher_lets_the_egress_take_its_slot() {
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();
        let the_helpers_answer = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        the_helpers_answer
            .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        let how_to_read_the_port = a_port_published_by_a_helper("opened", &the_helpers_answer);

        let took_a_slot = take_a_destination_slot_once_the_port_publishes(
            &how_to_read_the_port,
            &a_port_addressed_on_the_mesh(),
            &iceoryx2_node,
            &AtomicBool::new(false),
        );

        assert!(took_a_slot.is_some());
    }
}
