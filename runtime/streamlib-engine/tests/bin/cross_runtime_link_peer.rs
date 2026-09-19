// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One end of a cross-runtime link, in its own OS process, for
//! `cross_runtime_links_two_processes`.
//!
//! A link between runtimes is between processes by construction, so its proof
//! needs a second one. CI has no GPU, and `Runner::start()` needs one — so this
//! stands up a runtime's mesh half without a `Runner`: the same
//! `RuntimeMeshMembership` a runtime joins with, told to serve its output ports
//! or to carry a link from another runtime, over a real iceoryx2 channel and a
//! real Zenoh session.
//!
//! `--source` publishes one port and offers it. `--reader` links from an
//! address and reports every bag that lands on the local channel, as one JSON
//! line each, beside the link's own state and what its ingress says the hop
//! lost.
//!
//! The reader also counts what *its own* local ring lost, off the same
//! sequence numbers a real destination's subscriber reads, because it polls a
//! subscriber directly instead of having a processor's counted mailbox. That
//! is what lets an arm state the whole conservation identity rather than
//! assume the last hop was lossless.
//!
//! Closing its stdin is how the test asks for a clean leave. A test that wants
//! an abrupt one kills it instead, which is how "SIGKILL of the source returns
//! the link to waiting" is proven.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use streamlib_engine::core::graph::{MeshPortAddress, RemoteLinkResolution};
use streamlib_engine::core::runtime::mesh::{
    HowToReadAnOfferedOutputPort, MeshLinkIngressTable, OutputPortOfferedOnTheMesh,
    OutputPortsOfferedOnTheMesh, ResolvedRuntimeMeshConfiguration, RuntimeMeshMembership,
    WhatThisRuntimeOffersOnTheMesh, WhatThisRuntimeOffersOnTheMeshRegistry,
};
use streamlib_engine::core::runtime::{RuntimeMeshConfiguration, RuntimeName};
use streamlib_engine::iceoryx2::{
    ChannelDataServicePublisher, ChannelSizing, FRAME_HEADER_SIZE, FrameHeader, Iceoryx2Node,
    MeshHopDroppedBagCountsByRemoteInboundLink, mesh_ingress_channel_name,
};

/// What the peer writes once its mesh half is up.
const READY_LINE: &str = "READY";

/// What the peer writes instead when it could not come up at all.
const REFUSED_LINE_PREFIX: &str = "REFUSED ";

/// The processor id the source's channel is keyed on. Not a mesh name: the
/// mesh addresses the port by its *display* name, and this is the local side
/// the display name resolves to.
const THE_SOURCES_PROCESSOR_ID: &str = "psource";

/// The port the source publishes and the reader links from.
const THE_PORT: &str = "video";

/// How often either peer reports.
const HOW_OFTEN_THE_PEER_REPORTS: Duration = Duration::from_millis(100);

/// How long the reader waits after finding its local channel empty. Short
/// enough that it is back before a 16-deep ring can fill.
const HOW_LONG_AN_EMPTY_POLL_WAITS: Duration = Duration::from_micros(200);

/// How deep the source's channel is, and the ring every reader of it takes.
const THE_CHANNELS_DEPTH: usize = 16;

/// How many destination slots the source's channel is created with. Fixed like
/// every channel's, so an egress joining later fits a slot that already exists.
const THE_CHANNELS_SUBSCRIBER_SLOTS: usize = 8;

fn main() {
    // No `Runner`, so no engine logging pathway: a peer says what it is doing
    // only when the test asks for it, and only to stderr, which the harness
    // keeps out of its report channel.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let report = ReportChannelTakenBeforeAnythingReplacesStdout::take();
    let how = HowToRunThisPeer::read_from_the_command_line();
    let asked_to_leave = Arc::new(AtomicBool::new(false));
    read_stdin_until_it_closes(Arc::clone(&asked_to_leave));

    let outcome = match how.role {
        WhatThisPeerIs::TheSourceOfTheLink => run_as_the_source(&report, &how, &asked_to_leave),
        WhatThisPeerIs::TheReaderOfTheLink => run_as_the_reader(&report, &how, &asked_to_leave),
    };
    if let Err(why) = outcome {
        report.write_line(&format!("{REFUSED_LINE_PREFIX}{why}"));
        std::process::exit(2);
    }
}

/// Publish one port, offer it on the mesh, and let the egress do the rest.
fn run_as_the_source(
    report: &ReportChannelTakenBeforeAnythingReplacesStdout,
    how: &HowToRunThisPeer,
    asked_to_leave: &AtomicBool,
) -> Result<(), String> {
    let iceoryx2_node = how.open_an_iceoryx2_node()?;
    let channel_service_name =
        streamlib_engine::iceoryx2::source_channel_name(THE_SOURCES_PROCESSOR_ID, THE_PORT)
            .map_err(|why| why.to_string())?
            .into_string();
    let service = iceoryx2_node
        .open_or_create_service(
            &channel_service_name,
            THE_CHANNELS_SUBSCRIBER_SLOTS,
            THE_CHANNELS_DEPTH,
        )
        .map_err(|why| why.to_string())?;
    let publisher = service
        .create_publisher(1024)
        .map_err(|why| why.to_string())?;

    let offered = Arc::new(WhatThisRuntimeOffersOnTheMeshRegistry::default());
    offered.record_how_to_read_this_runtimes_graph(Arc::new(TheOnePortThisPeerOffers {
        processor_display_name: how.display_name.clone(),
        channel_service_name,
    }));

    let membership = how.join_the_mesh()?;
    membership.start_serving_this_runtimes_output_ports(&offered, &iceoryx2_node);
    report.write_line(READY_LINE);

    // One bag per report interval, each carrying its own index so the reader
    // can say which arrived, and stamped so the test can check the stamp
    // crossed unchanged.
    let mut publisher = publisher;
    let mut published: u64 = 0;
    let mut burst_still_owed = how.burst_once_a_reader_arrives;
    while !asked_to_leave.load(Ordering::Relaxed) {
        let how_many_to_publish_now = match burst_still_owed {
            // No burst asked for: one bag per report, which is what every
            // other arm reads.
            None => 1,
            // A burst asked for and not yet sent. It waits for a reader, so
            // every bag of it is one the link was already carrying — which is
            // what makes the conservation the reader states an identity rather
            // than a race against the wiring.
            Some(owed) if owed > 0 && !membership.render_for_graph().egress_ports.is_empty() => {
                burst_still_owed = Some(0);
                owed
            }
            // Waiting for a reader, or the burst is spent. A bursting source
            // publishes nothing else ever, so the reader's count settling is
            // the whole of the burst having arrived or been lost.
            Some(_) => 0,
        };
        for _ in 0..how_many_to_publish_now {
            if how.recreate_the_publisher_after == Some(published) {
                // The port's publisher replaced under a running egress, which
                // is what a processor's last link going and coming back does.
                // The new one numbers its own sends from zero, so without the
                // generation beside the number the reader would read the
                // change as loss.
                drop(publisher);
                publisher = service
                    .create_publisher(1024)
                    .map_err(|why| why.to_string())?;
            }
            publish_one_bag(&publisher, published)?;
            published += 1;
        }

        // The mesh half of `graph` rides every report, so the test can watch
        // the source's own view of who is reading it change as readers come
        // and go — which is the only place that view exists without a `Runner`.
        report.write_line(
            &serde_json::json!({
                "published": published.saturating_sub(1),
                "published_count": published,
                "timestamp_ns": a_stamp_for(published.saturating_sub(1)),
                "egress_ports": membership.render_for_graph().egress_ports,
            })
            .to_string(),
        );
        std::thread::sleep(HOW_OFTEN_THE_PEER_REPORTS);
    }

    membership.leave("this peer was asked to leave");
    Ok(())
}

/// Publish bag `published`, framed and stamped the way a real output port
/// frames and stamps one.
fn publish_one_bag(publisher: &ChannelDataServicePublisher, published: u64) -> Result<(), String> {
    let bag = a_bag_carrying(published);
    let stamp = a_stamp_for(published);
    let framed_len = FRAME_HEADER_SIZE + bag.len();
    let mut framed = vec![0u8; framed_len];
    FrameHeader::new(THE_PORT, stamp, bag.len() as u32)
        .map_err(|why| why.to_string())?
        .write_to_slice(&mut framed[..FRAME_HEADER_SIZE]);
    framed[FRAME_HEADER_SIZE..].copy_from_slice(&bag);

    let mut sample = publisher
        .loan_slice_uninit(framed_len)
        .map_err(|why| format!("{why:?}"))?;
    sample.payload_mut().copy_from_slice(unsafe {
        // SAFETY: `MaybeUninit<u8>` has the same layout as `u8`, and every
        // byte of `framed` is initialized.
        std::slice::from_raw_parts(
            framed.as_ptr() as *const std::mem::MaybeUninit<u8>,
            framed.len(),
        )
    });
    // SAFETY: the copy above initialized every byte the loan was taken for.
    let mut sample = unsafe { sample.assume_init() };
    // The engine's own numbering, which the egress copies into the
    // attachment: this peer has no output writer to do it, so it numbers its
    // own sends exactly as one does.
    sample.user_header_mut().sequence_number = published;
    sample.send().map_err(|why| format!("{why:?}"))?;
    Ok(())
}

/// Link from one address and report what lands on the local channel.
fn run_as_the_reader(
    report: &ReportChannelTakenBeforeAnythingReplacesStdout,
    how: &HowToRunThisPeer,
    asked_to_leave: &AtomicBool,
) -> Result<(), String> {
    let address = MeshPortAddress::new(
        how.link_from.clone().ok_or("--link-from is required")?,
        how.display_name.clone(),
        THE_PORT,
    )
    .map_err(|why| why.to_string())?;

    let iceoryx2_node = how.open_an_iceoryx2_node()?;
    // The channel the ingress writes, derived from the address exactly as the
    // wiring op derives it — which is the whole point of the derivation being
    // a pure function of the address.
    let local_channel = mesh_ingress_channel_name(&address.to_string()).into_string();
    let local_service = iceoryx2_node
        .open_or_create_service(
            &local_channel,
            streamlib_ipc_types::MAX_DESTINATIONS_PER_CHANNEL
                + streamlib_engine::iceoryx2::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL
                + streamlib_engine::iceoryx2::RESERVED_MESH_EGRESS_SUBSCRIBER_SLOTS_PER_CHANNEL,
            streamlib_engine::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        )
        .map_err(|why| why.to_string())?;
    let subscriber = local_service
        .create_subscriber(streamlib_engine::iceoryx2::DeliveryProfile::ORDERED_DEPTH)
        .map_err(|why| why.to_string())?;

    let ingress_table = MeshLinkIngressTable::of_this_runtime(&iceoryx2_node);
    let membership = how.join_the_mesh()?;
    membership.start_carrying_links_from_other_runtimes(&ingress_table);

    let how_far_it_has_got = Arc::new(Mutex::new(RemoteLinkResolution::AwaitingRemote {
        reason: "this peer has only just asked for the link".to_string(),
    }));
    let link_id = streamlib_engine::core::graph::LinkUniqueId::new();
    membership.note_a_link_from_another_runtime(
        address,
        link_id.clone(),
        Arc::clone(&how_far_it_has_got),
    );
    // What the wiring op does in a real runtime, and what this peer does for
    // itself because it has no compiler: the destination's side of the local
    // channel is open, so the link is one the mesh may report as carrying. The
    // notify service is `None` because this peer polls its own subscriber
    // rather than waiting on a listener. The counts stand in for the ones a
    // real destination's node carries, and are read back the same way `graph`
    // reads those.
    let where_the_hop_loss_is_counted =
        Arc::new(MeshHopDroppedBagCountsByRemoteInboundLink::default());
    ingress_table.note_how_a_links_destination_is_woken(
        &link_id,
        None,
        Some(Arc::clone(&where_the_hop_loss_is_counted)),
    );
    report.write_line(READY_LINE);

    // This peer polls a subscriber where a real destination has a counted
    // mailbox, so it counts its own ring's overwrites itself — against the
    // ingress's numbering, which is a fresh one per wiring and unrelated to the
    // sending runtime's.
    //
    // Drained continuously rather than once per report: the local channel is
    // as shallow as any `ordered` port, and a poll cadence would make this
    // peer lose almost everything a burst sent — loss after the hop, which is
    // not what a hop-loss arm is measuring.
    let mut counted = WhatThisPeerHasSeenOnItsLocalChannel::default();
    let mut report_next_at = std::time::Instant::now();
    while !asked_to_leave.load(Ordering::Relaxed) {
        let mut drained_something = false;
        while let Ok(Some(sample)) = subscriber.receive() {
            drained_something = true;
            counted.note_one_sample_off_the_local_channel(sample.user_header().sequence_number);

            let framed = sample.payload();
            if framed.len() < FRAME_HEADER_SIZE {
                continue;
            }
            let header = FrameHeader::read_from_slice(&framed[..FRAME_HEADER_SIZE]);
            let bag = String::from_utf8_lossy(&framed[FRAME_HEADER_SIZE..]).into_owned();
            counted.note_the_bag_it_carried(&bag);
            report.write_line(
                &serde_json::json!({ "received": bag, "timestamp_ns": header.timestamp_ns })
                    .to_string(),
            );
        }

        let now = std::time::Instant::now();
        if now >= report_next_at {
            report_next_at = now + HOW_OFTEN_THE_PEER_REPORTS;
            let mut how_far = how_far_it_has_got_as_json(&how_far_it_has_got);
            if let Some(reported) = how_far.as_object_mut() {
                reported.insert(
                    "mesh_hop_dropped_bags_by_link".to_string(),
                    serde_json::json!(
                        where_the_hop_loss_is_counted
                            .mesh_hop_dropped_bag_count_snapshot_by_inbound_link()
                    ),
                );
                counted.write_what_it_has_seen_into(reported);
            }
            report.write_line(&how_far.to_string());
        }
        if !drained_something {
            std::thread::sleep(HOW_LONG_AN_EMPTY_POLL_WAITS);
        }
    }

    ingress_table.stop();
    membership.leave("this peer was asked to leave");
    Ok(())
}

/// What this peer has taken off its local channel, in the terms an arm states
/// conservation in.
///
/// Its own ring's losses are counted here because this peer polls a subscriber
/// where a real destination has a counted mailbox. They are split in two: what
/// went missing *before* its first poll of a wiring, and what went missing
/// between two samples it saw. The first is what says whether this peer was
/// there from the ingress's first bag — without which no arm can state the
/// span the hop count covers.
#[derive(Default)]
struct WhatThisPeerHasSeenOnItsLocalChannel {
    received_count: u64,
    first_bag_index: Option<u64>,
    last_bag_index: Option<u64>,
    bags_lost_before_this_peers_first_poll: u64,
    what_this_peers_own_ring_lost: u64,
    last_number_the_local_channel_carried: Option<u64>,
}

impl WhatThisPeerHasSeenOnItsLocalChannel {
    /// Note one sample by the number the ingress gave it on the local channel.
    ///
    /// The ingress numbers a wiring's sends from zero, so the number on the
    /// first sample of all is exactly how many it wrote that this peer never
    /// saw.
    fn note_one_sample_off_the_local_channel(&mut self, number_on_the_local_channel: u64) {
        match self.last_number_the_local_channel_carried {
            Some(last) => {
                self.what_this_peers_own_ring_lost += number_on_the_local_channel
                    .saturating_sub(last)
                    .saturating_sub(1)
            }
            None => self.bags_lost_before_this_peers_first_poll = number_on_the_local_channel,
        }
        self.last_number_the_local_channel_carried = Some(number_on_the_local_channel);
    }

    /// Note the bag one sample carried, by the index its producer wrote into it.
    fn note_the_bag_it_carried(&mut self, bag: &str) {
        self.received_count += 1;
        let Some(index) = bag
            .strip_prefix("bag-")
            .and_then(|index| index.parse::<u64>().ok())
        else {
            return;
        };
        self.first_bag_index.get_or_insert(index);
        self.last_bag_index = Some(index);
    }

    fn write_what_it_has_seen_into(
        &self,
        reported: &mut serde_json::Map<String, serde_json::Value>,
    ) {
        reported.insert(
            "received_count".to_string(),
            serde_json::json!(self.received_count),
        );
        reported.insert(
            "first_bag_index".to_string(),
            serde_json::json!(self.first_bag_index),
        );
        reported.insert(
            "last_bag_index".to_string(),
            serde_json::json!(self.last_bag_index),
        );
        reported.insert(
            "bags_lost_before_this_peers_first_poll".to_string(),
            serde_json::json!(self.bags_lost_before_this_peers_first_poll),
        );
        reported.insert(
            "what_this_peers_own_ring_lost".to_string(),
            serde_json::json!(self.what_this_peers_own_ring_lost),
        );
    }
}

/// How far the link has got, in the shape the test reads.
fn how_far_it_has_got_as_json(
    how_far_it_has_got: &Mutex<RemoteLinkResolution>,
) -> serde_json::Value {
    match &*how_far_it_has_got.lock() {
        RemoteLinkResolution::AwaitingRemote { reason } => {
            serde_json::json!({ "state": "awaiting_remote", "reason": reason })
        }
        RemoteLinkResolution::Wired => serde_json::json!({ "state": "wired" }),
        RemoteLinkResolution::Refused { reason } => {
            serde_json::json!({ "state": "error", "reason": reason })
        }
    }
}

/// The bag the source publishes for index `published`.
///
/// Plain bytes rather than msgpack: what crosses must be byte-equal, and a
/// payload the engine cannot read is exactly as good a proof of that as one it
/// can — better, since it also shows the mesh inspects nothing but the one key.
pub fn a_bag_carrying(published: u64) -> Vec<u8> {
    format!("bag-{published}").into_bytes()
}

/// The stamp the source puts on bag `published`, far enough from zero that a
/// stamp invented on the way across could not pass for it.
pub fn a_stamp_for(published: u64) -> i64 {
    1_726_000_000_000_000_000 + published as i64
}

/// The one port this peer offers, and the channel it publishes to.
struct TheOnePortThisPeerOffers {
    processor_display_name: String,
    channel_service_name: String,
}

impl WhatThisRuntimeOffersOnTheMesh for TheOnePortThisPeerOffers {
    fn output_ports_it_offers_right_now(&self) -> OutputPortsOfferedOnTheMesh {
        OutputPortsOfferedOnTheMesh {
            ports: vec![OutputPortOfferedOnTheMesh {
                processor_display_name: self.processor_display_name.clone(),
                port_name: THE_PORT.to_string(),
            }],
        }
    }

    fn how_to_read_an_offered_output_port(
        &self,
        processor_display_name: &str,
        port_name: &str,
    ) -> Option<HowToReadAnOfferedOutputPort> {
        (processor_display_name == self.processor_display_name && port_name == THE_PORT).then(
            || HowToReadAnOfferedOutputPort {
                channel_service_name: self.channel_service_name.clone(),
                channel_sizing: ChannelSizing {
                    max_subscribers: THE_CHANNELS_SUBSCRIBER_SLOTS,
                    channel_service_creation_depth: THE_CHANNELS_DEPTH,
                },
            },
        )
    }
}

/// Which end of the link this process is.
enum WhatThisPeerIs {
    TheSourceOfTheLink,
    TheReaderOfTheLink,
}

/// Every flag the fixture drives.
struct HowToRunThisPeer {
    role: WhatThisPeerIs,
    mesh: RuntimeMeshConfiguration,
    display_name: String,
    link_from: Option<String>,
    iceoryx2_domain_root: std::path::PathBuf,
    /// Publish this many bags back to back once a reader is there, instead of
    /// one per report. Far more than the channel is deep, so the egress cannot
    /// drain them all and the loss is the rings' rather than the network's.
    burst_once_a_reader_arrives: Option<u64>,
    /// Replace the channel publisher just before this bag, so the numbering
    /// restarts under a running egress.
    recreate_the_publisher_after: Option<u64>,
}

impl HowToRunThisPeer {
    fn read_from_the_command_line() -> Self {
        let mut role = WhatThisPeerIs::TheSourceOfTheLink;
        let mut mesh = RuntimeMeshConfiguration::default();
        let mut display_name = "CameraSource".to_string();
        let mut link_from = None;
        let mut iceoryx2_domain_root = std::path::PathBuf::from("/tmp");
        let mut burst_once_a_reader_arrives = None;
        let mut recreate_the_publisher_after = None;
        let mut arguments = std::env::args().skip(1);
        while let Some(flag) = arguments.next() {
            let mut value = || arguments.next().expect("every flag takes a value");
            match flag.as_str() {
                "--source" => role = WhatThisPeerIs::TheSourceOfTheLink,
                "--reader" => role = WhatThisPeerIs::TheReaderOfTheLink,
                "--runtime-name" => mesh.runtime_name = Some(value()),
                "--mesh-name" => mesh.mesh_name = Some(value()),
                "--mesh-peer" => mesh
                    .mesh_peer_endpoints
                    .get_or_insert_with(Vec::new)
                    .push(value()),
                "--mesh-listen" => mesh
                    .mesh_listen_endpoints
                    .get_or_insert_with(Vec::new)
                    .push(value()),
                "--multicast-discovery" => mesh.mesh_multicast_discovery = Some(value() == "on"),
                "--display-name" => display_name = value(),
                "--link-from" => link_from = Some(value()),
                "--iceoryx2-domain-root" => iceoryx2_domain_root = value().into(),
                "--burst-once-a-reader-arrives" => {
                    burst_once_a_reader_arrives = Some(value().parse().expect("a bag count"))
                }
                "--recreate-the-publisher-after" => {
                    recreate_the_publisher_after = Some(value().parse().expect("a bag index"))
                }
                unknown => panic!("unknown flag {unknown:?}"),
            }
        }
        Self {
            role,
            mesh,
            display_name,
            link_from,
            iceoryx2_domain_root,
            burst_once_a_reader_arrives,
            recreate_the_publisher_after,
        }
    }

    fn join_the_mesh(&self) -> Result<RuntimeMeshMembership, String> {
        let mut mesh = RuntimeMeshConfiguration {
            runtime_name: self.mesh.runtime_name.clone(),
            mesh_name: self.mesh.mesh_name.clone(),
            mesh_peer_endpoints: self.mesh.mesh_peer_endpoints.clone(),
            mesh_listen_endpoints: self.mesh.mesh_listen_endpoints.clone(),
            mesh_multicast_discovery: self.mesh.mesh_multicast_discovery,
        };
        let runtime_name = Arc::new(
            RuntimeName::from_configuration_environment_or_default(mesh.runtime_name.take())
                .map_err(|why| why.to_string())?,
        );
        let resolved =
            ResolvedRuntimeMeshConfiguration::resolve(mesh).map_err(|why| why.to_string())?;
        RuntimeMeshMembership::join(
            &resolved,
            &runtime_name,
            "R0000000000",
            "a-test-host",
            &Arc::new(Default::default()),
        )
        .map_err(|why| why.to_string())
    }

    fn open_an_iceoryx2_node(&self) -> Result<Iceoryx2Node, String> {
        std::fs::create_dir_all(&self.iceoryx2_domain_root).map_err(|why| why.to_string())?;
        Iceoryx2Node::new(
            &self.iceoryx2_domain_root,
            &format!("cross-runtime-link-peer/{}", std::process::id()),
        )
        .map_err(|why| why.to_string())
    }
}

/// A duplicate of fd 1, taken before anything replaces fd 1 itself.
struct ReportChannelTakenBeforeAnythingReplacesStdout(std::fs::File);

impl ReportChannelTakenBeforeAnythingReplacesStdout {
    fn take() -> Self {
        // SAFETY: fd 1 is open at process start, and the duplicate is checked
        // before anything adopts it.
        let duplicated = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(duplicated >= 0, "this process has no stdout to report on");
        // SAFETY: `duplicated` is a fresh descriptor nothing else owns.
        Self(unsafe { std::os::fd::FromRawFd::from_raw_fd(duplicated) })
    }

    fn write_line(&self, line: &str) {
        use std::io::Write as _;
        let mut channel = &self.0;
        let _ = writeln!(channel, "{line}");
        let _ = channel.flush();
    }
}

/// Watch stdin on its own thread: the parent closing it is the ask to leave.
fn read_stdin_until_it_closes(asked_to_leave: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().lock().read_line(&mut line).unwrap_or(0) > 0 {
            line.clear();
        }
        asked_to_leave.store(true, Ordering::Relaxed);
    });
}
