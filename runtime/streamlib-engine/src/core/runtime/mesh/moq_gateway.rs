// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MoQ gateway: every output port of this runtime as a MoQ track on a
//! relay, pulled on subscribe.
//!
//! One session to the relay announces `<prefix>/<runtime name>/<epoch>`, the
//! epoch being the unix second the session opened — a relay keeps a namespace
//! routed to a publisher that died without saying so for minutes, so no two
//! sessions of this gateway ever announce the same one. Nothing is read off a
//! port until the relay asks for its track: moq-transport hands the gateway a
//! fresh track writer for every track nobody is serving yet, and only then
//! does a port-serving thread take a subscriber slot on the port's channel.
//! A track whose groups stop being forwarded is dropped, which makes it stale
//! and sends the next SUBSCRIBE for it back through the same door.
//!
//! Each served bag is the Zenoh data path's message — attachment and payload
//! — framed as one plaintext object, sealed in the secure object envelope
//! whenever a content key is configured.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::core::json_schema::MoqGatewayTrackKind;
use crate::core::json_schema::{MoqGatewayOutput, MoqGatewayStateOutput, MoqGatewayTrackOutput};
use crate::core::runtime::mesh::a_bags_top_level_surface_id::{
    the_top_level_keys_the_moq_gateway_reads, the_top_level_surface_id_of_a_bag,
};
use crate::core::runtime::mesh::a_frames_pixels_read_out_for_the_mesh::ReadsAFramesPixelsOutForTheMesh;
use crate::core::runtime::mesh::gpu_context_the_mesh_copies_frames_with::GpuContextTheMeshCopiesFramesWith;
use crate::core::runtime::mesh::machine_clock_identity::MachineClockIdentity;
use crate::core::runtime::mesh::mesh_data_message_attachment::{
    MeshDataMessageAttachment, PublisherGenerationOnTheMesh,
};
use crate::core::runtime::mesh::mesh_port_egress::{
    PublisherGenerationsOnePortHasHad, WhyAnEgressNeverStarted,
    take_a_destination_slot_once_the_port_publishes,
};
use crate::core::runtime::mesh::moq_gateway_configuration::{
    HOW_OFTEN_THE_MOQ_GATEWAY_HANDOFF_IS_READ_AGAIN, MoqGatewayDoors, the_relay_host_of,
};
use crate::core::runtime::mesh::moq_gateway_group_cut_policy::WhenTheMoqGatewayCutsAGroup;
use crate::core::runtime::mesh::moq_gateway_object_framing::a_plaintext_moq_object_carrying;
use crate::core::runtime::mesh::moq_relay_client_connection::{
    open_a_moq_session_to_the_relay, the_moq_transport_runtime,
};
use crate::core::runtime::mesh::moq_secure_object_envelope::seal_a_moq_object_with_a_fresh_iv;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::{
    OutputPortOfferedOnTheMesh, WhatThisRuntimeOffersOnTheMeshRegistry,
};
use crate::iceoryx2::{ChannelIdlePollBackoff, FRAME_HEADER_SIZE, FrameHeader, Iceoryx2Node};

/// How long a served track may go with no forwarder on its open group before
/// the relay is taken to have stopped subscribing.
const HOW_LONG_A_TRACK_GOES_UNFORWARDED_BEFORE_IT_IS_DROPPED: Duration = Duration::from_secs(5);

/// How long the gateway waits before dialling a relay again after a failure.
const HOW_LONG_THE_GATEWAY_WAITS_BEFORE_DIALLING_AGAIN: Duration = Duration::from_secs(2);

/// The publisher priority every gateway group is opened at: moq-pub's media
/// literal, the rung the extension wheel's data tracks ride.
const MOQ_GATEWAY_GROUP_PRIORITY: u8 = 127;

/// Where this runtime's gateway publishes right now, for the mesh description
/// a peer's MoQ ingress reads and for the graph.
#[derive(Default)]
pub(crate) struct WhereTheMoqGatewayPublishes {
    right_now: Mutex<Option<MoqGatewayNamespaceOnARelay>>,
}

/// One announced namespace and the relay host it is announced on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoqGatewayNamespaceOnARelay {
    pub(crate) namespace: String,
    pub(crate) relay_host: String,
}

impl WhereTheMoqGatewayPublishes {
    /// The namespace announced right now, or `None` while none is.
    pub(crate) fn right_now(&self) -> Option<MoqGatewayNamespaceOnARelay> {
        self.right_now.lock().clone()
    }

    fn record(&self, announced: Option<MoqGatewayNamespaceOnARelay>) {
        *self.right_now.lock() = announced;
    }
}

/// What the gateway knows about one track, whether or not it is served.
#[derive(Default)]
struct MoqGatewayTrackRecord {
    kind_shown_by_a_bag: Mutex<Option<MoqGatewayTrackKind>>,
    objects_published: AtomicU64,
    a_relay_subscription_is_live: AtomicBool,
}

/// Everything the gateway's threads share.
struct MoqGatewayShared {
    doors: Arc<MoqGatewayDoors>,
    this_runtimes_name: String,
    offered: Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
    iceoryx2_node: Iceoryx2Node,
    gpu_context_the_mesh_copies_frames_with: Arc<GpuContextTheMeshCopiesFramesWith>,
    where_it_publishes: Arc<WhereTheMoqGatewayPublishes>,
    serving_state: Mutex<MoqGatewayStateOutput>,
    tracks: Mutex<BTreeMap<OutputPortOfferedOnTheMesh, Arc<MoqGatewayTrackRecord>>>,
    stop: AtomicBool,
}

impl MoqGatewayShared {
    fn the_record_of(&self, port: &OutputPortOfferedOnTheMesh) -> Arc<MoqGatewayTrackRecord> {
        Arc::clone(self.tracks.lock().entry(port.clone()).or_default())
    }

    fn the_kind_of(&self, port: &OutputPortOfferedOnTheMesh) -> MoqGatewayTrackKind {
        self.tracks
            .lock()
            .get(port)
            .and_then(|record| *record.kind_shown_by_a_bag.lock())
            .unwrap_or_else(|| {
                MoqGatewayTrackKind::inferred_from_the_port_names(
                    &port.processor_display_name,
                    &port.port_name,
                )
            })
    }

    fn a_kind_is_servable(&self, kind: MoqGatewayTrackKind) -> bool {
        kind != MoqGatewayTrackKind::Surface || self.doors.serve_surface_ports
    }
}

/// This runtime's MoQ gateway. Dropping it closes its relay session and stops
/// every port it serves.
pub(crate) struct MoqGateway {
    shared: Arc<MoqGatewayShared>,
    control_task: Option<tokio::task::JoinHandle<()>>,
}

impl MoqGateway {
    /// Start the gateway `doors` configure, serving `offered`'s ports.
    pub(crate) fn start(
        doors: Arc<MoqGatewayDoors>,
        this_runtimes_name: &str,
        offered: &Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
        iceoryx2_node: &Iceoryx2Node,
        gpu_context_the_mesh_copies_frames_with: &Arc<GpuContextTheMeshCopiesFramesWith>,
        where_it_publishes: &Arc<WhereTheMoqGatewayPublishes>,
    ) -> Result<Self, String> {
        let transport_runtime = the_moq_transport_runtime()?;
        let shared = Arc::new(MoqGatewayShared {
            doors,
            this_runtimes_name: this_runtimes_name.to_string(),
            offered: Arc::clone(offered),
            iceoryx2_node: iceoryx2_node.clone(),
            gpu_context_the_mesh_copies_frames_with: Arc::clone(
                gpu_context_the_mesh_copies_frames_with,
            ),
            where_it_publishes: Arc::clone(where_it_publishes),
            serving_state: Mutex::new(MoqGatewayStateOutput::WaitingForRelay),
            tracks: Mutex::default(),
            stop: AtomicBool::new(false),
        });
        let control_task =
            transport_runtime.spawn(run_the_moq_gateway_until_stopped(Arc::clone(&shared)));
        tracing::info!("The MoQ gateway for runtime {this_runtimes_name} started");
        Ok(Self {
            shared,
            control_task: Some(control_task),
        })
    }

    /// The gateway as `graph` renders it. Reads the graph, so it must not be
    /// called under the compiler's scope.
    pub(crate) fn render_for_graph(&self) -> MoqGatewayOutput {
        render_the_gateway_for_graph(Some(&self.shared), &self.shared.offered)
    }
}

impl Drop for MoqGateway {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(control_task) = self.control_task.take() {
            // The control loop notices the flag within one poll and tears its
            // session down; an abort is the backstop.
            let deadline = Instant::now() + Duration::from_secs(3);
            while !control_task.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            control_task.abort();
        }
        self.shared.where_it_publishes.record(None);
    }
}

/// The listing `graph` renders for a runtime with no gateway configured.
pub(crate) fn render_an_unconfigured_moq_gateway_for_graph(
    offered: &WhatThisRuntimeOffersOnTheMeshRegistry,
) -> MoqGatewayOutput {
    render_the_gateway_for_graph(None, offered)
}

/// The listing `graph` renders, for a gateway or for none (`off`).
fn render_the_gateway_for_graph(
    shared: Option<&Arc<MoqGatewayShared>>,
    offered: &WhatThisRuntimeOffersOnTheMeshRegistry,
) -> MoqGatewayOutput {
    let announced = shared.and_then(|shared| shared.where_it_publishes.right_now());
    let relay_host = match (shared, &announced) {
        (_, Some(announced)) => announced.relay_host.clone(),
        (Some(shared), None) => shared
            .doors
            .with_what_it_is_told(|told| told.relay_publish_url.as_deref().map(the_relay_host_of))
            .unwrap_or_default(),
        (None, None) => String::new(),
    };
    let namespace = announced
        .as_ref()
        .map(|announced| announced.namespace.clone())
        .unwrap_or_default();
    let state = match shared {
        Some(shared) => *shared.serving_state.lock(),
        None => MoqGatewayStateOutput::Off,
    };
    let tracks = offered
        .output_ports_it_offers_right_now()
        .ports
        .into_iter()
        .map(|port| {
            let (kind, servable, subscribed, objects_published) = match shared {
                Some(shared) => {
                    let kind = shared.the_kind_of(&port);
                    let record = shared.tracks.lock().get(&port).cloned();
                    (
                        kind,
                        shared.a_kind_is_servable(kind),
                        record.as_ref().is_some_and(|record| {
                            record.a_relay_subscription_is_live.load(Ordering::Acquire)
                        }),
                        record
                            .as_ref()
                            .map_or(0, |record| record.objects_published.load(Ordering::Relaxed)),
                    )
                }
                None => {
                    let kind = MoqGatewayTrackKind::inferred_from_the_port_names(
                        &port.processor_display_name,
                        &port.port_name,
                    );
                    (kind, kind != MoqGatewayTrackKind::Surface, false, 0)
                }
            };
            MoqGatewayTrackOutput {
                relay_namespace: namespace.clone(),
                relay_track: port.to_string(),
                display_name: port.processor_display_name,
                port: port.port_name,
                kind,
                servable,
                subscribed,
                objects_published,
            }
        })
        .collect();
    MoqGatewayOutput {
        relay_host,
        namespace,
        state,
        tracks,
    }
}

/// The epoch a fresh session announces under: the unix second now, and never
/// one an earlier session of this gateway announced.
fn a_fresh_announcement_epoch(the_last_one: Option<u64>) -> u64 {
    // An identifier by the wire contract, not a timekeeping read: subscribers
    // only need it to differ between sessions.
    let unix_seconds_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    match the_last_one {
        Some(last) if unix_seconds_now <= last => last + 1,
        _ => unix_seconds_now,
    }
}

/// The gateway's control loop: dial, announce, hand every requested track to
/// a port-serving thread, and start over whenever the session ends or the
/// handoff says to.
async fn run_the_moq_gateway_until_stopped(shared: Arc<MoqGatewayShared>) {
    let mut the_last_epoch = None;
    let mut said_why_it_is_not_serving: Option<String> = None;
    while !shared.stop.load(Ordering::Acquire) {
        shared.doors.read_the_handoff_again();
        let told = shared.doors.what_it_is_told();
        if told.paused {
            *shared.serving_state.lock() = MoqGatewayStateOutput::Paused;
            tokio::time::sleep(HOW_OFTEN_THE_MOQ_GATEWAY_HANDOFF_IS_READ_AGAIN).await;
            continue;
        }
        *shared.serving_state.lock() = MoqGatewayStateOutput::WaitingForRelay;
        let (Some(relay_publish_url), Some(namespace_prefix)) = (
            told.relay_publish_url.clone(),
            told.namespace_prefix.clone(),
        ) else {
            tokio::time::sleep(HOW_OFTEN_THE_MOQ_GATEWAY_HANDOFF_IS_READ_AGAIN).await;
            continue;
        };
        let relay_host = the_relay_host_of(&relay_publish_url);
        let relay_session = match open_a_moq_session_to_the_relay(
            &relay_publish_url,
            shared.doors.accept_any_relay_certificate,
        )
        .await
        {
            Ok(relay_session) => relay_session,
            Err(why_not) => {
                if said_why_it_is_not_serving.as_deref() != Some(why_not.as_str()) {
                    tracing::warn!("The MoQ gateway could not reach its relay: {why_not}");
                    said_why_it_is_not_serving = Some(why_not);
                }
                tokio::time::sleep(HOW_LONG_THE_GATEWAY_WAITS_BEFORE_DIALLING_AGAIN).await;
                continue;
            }
        };
        said_why_it_is_not_serving = None;
        let epoch = a_fresh_announcement_epoch(the_last_epoch);
        the_last_epoch = Some(epoch);
        let namespace = format!(
            "{}/{}/{epoch}",
            namespace_prefix.trim_matches('/'),
            shared.this_runtimes_name
        );
        serve_one_relay_session(
            &shared,
            relay_session,
            MoqGatewayNamespaceOnARelay {
                namespace,
                relay_host,
            },
            (relay_publish_url, namespace_prefix),
        )
        .await;
    }
    *shared.serving_state.lock() = MoqGatewayStateOutput::Off;
}

/// Serve one session until it ends, the handoff changes what it serves, or
/// the gateway stops.
async fn serve_one_relay_session(
    shared: &Arc<MoqGatewayShared>,
    relay_session: crate::core::runtime::mesh::moq_relay_client_connection::MoqRelaySession,
    announced: MoqGatewayNamespaceOnARelay,
    served_relay_and_prefix: (String, String),
) {
    let track_namespace =
        match moq_transport::coding::TrackNamespace::try_from(announced.namespace.as_str()) {
            Ok(track_namespace) => track_namespace,
            Err(not_a_namespace) => {
                tracing::warn!(
                    "The MoQ gateway cannot announce {}: {not_a_namespace}",
                    announced.namespace
                );
                tokio::time::sleep(HOW_LONG_THE_GATEWAY_WAITS_BEFORE_DIALLING_AGAIN).await;
                return;
            }
        };
    let moq_transport_session = relay_session.session;
    let mut publisher = relay_session.publisher;
    let mut session_task = tokio::spawn(async move { moq_transport_session.run().await });
    let (_tracks_writer, mut tracks_request, tracks_reader) =
        moq_transport::serve::Tracks::new(track_namespace).produce();
    let mut publish_namespace_task =
        tokio::spawn(async move { publisher.publish_namespace(tracks_reader).await });

    shared.where_it_publishes.record(Some(announced.clone()));
    *shared.serving_state.lock() = MoqGatewayStateOutput::Serving;
    tracing::info!(
        "The MoQ gateway announced {} on {}",
        announced.namespace,
        announced.relay_host
    );

    let mut serving: BTreeMap<OutputPortOfferedOnTheMesh, MoqGatewayPortServing> = BTreeMap::new();
    let mut handoff_check = tokio::time::interval(HOW_OFTEN_THE_MOQ_GATEWAY_HANDOFF_IS_READ_AGAIN);
    let mut stop_check = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            requested = tracks_request.next() => {
                let Some(track_writer) = requested else {
                    tracing::info!("The MoQ gateway's track requests ended");
                    break;
                };
                if let Some((port, port_serving)) =
                    start_serving_a_requested_track(shared, &announced.namespace, track_writer)
                {
                    serving.retain(|_, running| !running.has_ended());
                    serving.insert(port, port_serving);
                }
            }
            ended = &mut session_task => {
                match ended {
                    Ok(Err(session_failure)) => tracing::warn!(
                        "The MoQ gateway's relay session ended: {session_failure}"
                    ),
                    _ => tracing::info!("The MoQ gateway's relay session closed"),
                }
                break;
            }
            ended = &mut publish_namespace_task => {
                tracing::warn!(
                    "The MoQ gateway's namespace {} stopped being announced: {:?}",
                    announced.namespace,
                    ended.map(|announced| announced.map_err(|failure| failure.to_string()))
                );
                break;
            }
            _ = handoff_check.tick() => {
                shared.doors.read_the_handoff_again();
                let told = shared.doors.what_it_is_told();
                let still_the_same_relay_and_prefix = told.relay_publish_url.as_deref()
                    == Some(served_relay_and_prefix.0.as_str())
                    && told.namespace_prefix.as_deref() == Some(served_relay_and_prefix.1.as_str());
                if told.paused || !still_the_same_relay_and_prefix {
                    tracing::info!(
                        "The MoQ gateway is closing {} because its handoff {}",
                        announced.namespace,
                        if told.paused { "paused it" } else { "named another relay or prefix" }
                    );
                    break;
                }
                serving.retain(|_, running| !running.has_ended());
            }
            _ = stop_check.tick() => {
                if shared.stop.load(Ordering::Acquire) {
                    break;
                }
            }
        }
    }

    shared.where_it_publishes.record(None);
    *shared.serving_state.lock() = MoqGatewayStateOutput::WaitingForRelay;
    for port_serving in serving.values() {
        port_serving.stop.store(true, Ordering::Release);
    }
    let _ = tokio::task::spawn_blocking(move || drop(serving)).await;
    publish_namespace_task.abort();
    session_task.abort();
}

/// One port being served to the relay on its own thread.
struct MoqGatewayPortServing {
    stop: Arc<AtomicBool>,
    serving_thread: Option<std::thread::JoinHandle<()>>,
}

impl MoqGatewayPortServing {
    fn has_ended(&self) -> bool {
        self.serving_thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
}

impl Drop for MoqGatewayPortServing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(serving_thread) = self.serving_thread.take()
            && serving_thread.join().is_err()
        {
            tracing::warn!("a MoQ gateway port-serving thread panicked");
        }
    }
}

/// Start serving the port a relay SUBSCRIBE asked for, or answer it
/// not-found.
fn start_serving_a_requested_track(
    shared: &Arc<MoqGatewayShared>,
    announced_namespace: &str,
    track_writer: moq_transport::serve::TrackWriter,
) -> Option<(OutputPortOfferedOnTheMesh, MoqGatewayPortServing)> {
    let track_name = track_writer.name.to_string_lossy().into_owned();
    let requested_namespace = track_writer.namespace.to_utf8_path();
    let Some((processor_display_name, port_name)) = track_name.rsplit_once('/') else {
        tracing::info!("The MoQ gateway was asked for {track_name}, which names no port");
        let _ = track_writer.close(moq_transport::serve::ServeError::NotFound);
        return None;
    };
    if requested_namespace.trim_matches('/') != announced_namespace {
        tracing::info!(
            "The MoQ gateway was asked for {track_name} under {requested_namespace}, which it \
             does not announce"
        );
        let _ = track_writer.close(moq_transport::serve::ServeError::NotFound);
        return None;
    }
    let port = OutputPortOfferedOnTheMesh {
        processor_display_name: processor_display_name.to_string(),
        port_name: port_name.to_string(),
    };
    let kind = shared.the_kind_of(&port);
    if !shared.a_kind_is_servable(kind) {
        tracing::info!(
            "The MoQ gateway refused {port}: it carries raw pixels, which never go to the relay \
             unless STREAMLIB_MESH_MOQ_SERVE_SURFACE_PORTS is set"
        );
        let _ = track_writer.close(moq_transport::serve::ServeError::NotFound);
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let serving = OnePortServedToTheRelay {
        shared: Arc::clone(shared),
        port: port.clone(),
        namespace_and_track: format!("{announced_namespace}/{track_name}"),
        track_writer,
        stop: Arc::clone(&stop),
    };
    match std::thread::Builder::new()
        .name("streamlib-moq-gateway-port".to_string())
        .spawn(move || serve_one_port_to_the_relay(serving))
    {
        Ok(serving_thread) => Some((
            port,
            MoqGatewayPortServing {
                stop,
                serving_thread: Some(serving_thread),
            },
        )),
        Err(cannot_spawn) => {
            tracing::warn!("The MoQ gateway had no thread to serve {port} on: {cannot_spawn}");
            None
        }
    }
}

/// Everything one port-serving thread owns.
struct OnePortServedToTheRelay {
    shared: Arc<MoqGatewayShared>,
    port: OutputPortOfferedOnTheMesh,
    /// `<namespace>/<track name>`, the key a content key is handed over under.
    namespace_and_track: String,
    track_writer: moq_transport::serve::TrackWriter,
    stop: Arc<AtomicBool>,
}

/// The body of one port-serving thread.
fn serve_one_port_to_the_relay(serving: OnePortServedToTheRelay) {
    let OnePortServedToTheRelay {
        shared,
        port,
        namespace_and_track,
        track_writer,
        stop,
    } = serving;
    let record = shared.the_record_of(&port);
    let Some(how_to_read_the_port) = shared
        .offered
        .how_to_read_an_offered_output_port(&port.processor_display_name, &port.port_name)
    else {
        tracing::info!("The MoQ gateway was asked for {port}, which this runtime does not have");
        let _ = track_writer.close(moq_transport::serve::ServeError::NotFound);
        return;
    };
    let subscriber = match take_a_destination_slot_once_the_port_publishes(
        &how_to_read_the_port,
        &shared.iceoryx2_node,
        &stop,
    ) {
        Ok(subscriber) => subscriber,
        Err(WhyAnEgressNeverStarted::ItWasCancelled) => return,
        Err(WhyAnEgressNeverStarted::ThePortCannotBeSent(why_not)) => {
            tracing::warn!("The MoQ gateway cannot serve {port}: {why_not}");
            let _ = track_writer.close(moq_transport::serve::ServeError::NotFound);
            return;
        }
    };
    let mut subgroups = match track_writer.subgroups() {
        Ok(subgroups) => subgroups,
        Err(failure) => {
            tracing::warn!("The MoQ gateway could not open {port} as subgroups: {failure}");
            return;
        }
    };
    tracing::info!("The MoQ gateway is serving {namespace_and_track}");
    record
        .a_relay_subscription_is_live
        .store(true, Ordering::Release);

    let mut open_group: Option<moq_transport::serve::SubgroupWriter> = None;
    let mut cut_policy = WhenTheMoqGatewayCutsAGroup::default();
    let mut publisher_generations = PublisherGenerationsOnePortHasHad::default();
    let mut reads_a_frames_pixels_out = ReadsAFramesPixelsOutForTheMesh::reading_through(
        &shared.gpu_context_the_mesh_copies_frames_with,
    );
    let mut idle_poll_backoff = ChannelIdlePollBackoff::starting_at_the_shortest_sleep();
    let clock_identity = MachineClockIdentity::of_this_machine();
    let mut a_forwarder_was_last_seen_at = Instant::now();
    let mut said_it_is_waiting_for_a_key = false;

    while !stop.load(Ordering::Acquire) {
        let sample = match subscriber.receive() {
            Ok(Some(sample)) => sample,
            Ok(None) => {
                std::thread::sleep(idle_poll_backoff.sleep_this_empty_poll_earns(Instant::now()));
                continue;
            }
            Err(receive_failure) => {
                tracing::warn!("The MoQ gateway stopped serving {port}: {receive_failure:?}");
                break;
            }
        };
        idle_poll_backoff.reset_after_a_bag_arrived();
        let framed = sample.payload();
        if framed.len() < FRAME_HEADER_SIZE {
            continue;
        }
        let timestamp_ns = FrameHeader::read_from_slice(&framed[..FRAME_HEADER_SIZE]).timestamp_ns;
        let sequence_number = sample.user_header().sequence_number;
        let publisher_generation =
            publisher_generations.generation_of_a_sample_numbered_by(sample.origin());
        let bag_bytes = &framed[FRAME_HEADER_SIZE..];

        let bag_keys = the_top_level_keys_the_moq_gateway_reads(bag_bytes);
        let kind = MoqGatewayTrackKind::shown_by_a_bag(&bag_keys);
        *record.kind_shown_by_a_bag.lock() = Some(kind);
        if !shared.a_kind_is_servable(kind) {
            tracing::info!(
                "The MoQ gateway stopped serving {port}: its bags name a surface, and raw pixels \
                 never go to the relay unless STREAMLIB_MESH_MOQ_SERVE_SURFACE_PORTS is set"
            );
            break;
        }

        let (payload, frame_pixel_description_bytes) =
            match the_top_level_surface_id_of_a_bag(bag_bytes) {
                None => (std::borrow::Cow::Borrowed(bag_bytes), 0),
                Some(named) => match reads_a_frames_pixels_out
                    .a_mesh_message_carrying_the_frame_this_bag_names(named.surface_id(), bag_bytes)
                {
                    Ok(carrying) => (
                        std::borrow::Cow::Owned(carrying.message_bytes),
                        carrying.description_bytes,
                    ),
                    Err(why_it_cannot_cross) => {
                        tracing::debug!(
                            "a frame on {port} is not going to the relay: {why_it_cannot_cross}"
                        );
                        continue;
                    }
                },
            };
        let plaintext_object = a_plaintext_moq_object_carrying(
            MeshDataMessageAttachment {
                timestamp_ns,
                sequence_number,
                publisher_generation: PublisherGenerationOnTheMesh(publisher_generation),
                clock_identity,
                frame_pixel_description_bytes,
            },
            &payload,
        );
        drop(sample);

        let object = match shared.doors.with_what_it_is_told(|told| {
            (
                told.objects_must_be_sealed,
                told.the_key_to_seal_a_track_with(&namespace_and_track)
                    .cloned(),
            )
        }) {
            (_, Some(content_key)) => {
                said_it_is_waiting_for_a_key = false;
                match seal_a_moq_object_with_a_fresh_iv(&content_key, &plaintext_object) {
                    Ok(sealed) => sealed,
                    Err(why_not) => {
                        tracing::warn!(
                            "The MoQ gateway could not seal an object on {port}: {why_not}"
                        );
                        continue;
                    }
                }
            }
            (true, None) => {
                if !said_it_is_waiting_for_a_key {
                    tracing::info!(
                        "The MoQ gateway publishes nothing on {namespace_and_track} until its \
                         handoff carries a content key for it"
                    );
                    said_it_is_waiting_for_a_key = true;
                }
                continue;
            }
            (false, None) => plaintext_object,
        };

        let now = Instant::now();
        let it_is_an_encoded_sync_point = kind.is_cut_at_sync_points()
            && bag_keys.carries_a_bitstream
            && bag_keys.is_a_sync_point;
        if cut_policy.this_object_opens_a_new_group(it_is_an_encoded_sync_point, now)
            || open_group.is_none()
        {
            // Dropped before the next opens: the finished group's forwarder
            // drains it and FINs its stream, and a lagging subscriber moves on
            // to the newest group rather than working through a backlog.
            open_group = None;
            match subgroups.append(MOQ_GATEWAY_GROUP_PRIORITY) {
                Ok(opened) => open_group = Some(opened),
                Err(failure) => {
                    tracing::warn!("The MoQ gateway could not open a group on {port}: {failure}");
                    break;
                }
            }
        }
        let Some(group) = open_group.as_mut() else {
            continue;
        };
        if let Err(failure) = group.write(bytes::Bytes::from(object)) {
            tracing::debug!("an object on {port} did not reach its group: {failure}");
            open_group = None;
            cut_policy.forget_the_open_group();
            continue;
        }
        record.objects_published.fetch_add(1, Ordering::Relaxed);

        if group.forwarded().is_some() {
            a_forwarder_was_last_seen_at = now;
        } else if now.saturating_duration_since(a_forwarder_was_last_seen_at)
            >= HOW_LONG_A_TRACK_GOES_UNFORWARDED_BEFORE_IT_IS_DROPPED
        {
            tracing::info!(
                "The MoQ gateway stopped serving {namespace_and_track}: nothing has forwarded it \
                 for {HOW_LONG_A_TRACK_GOES_UNFORWARDED_BEFORE_IT_IS_DROPPED:?}"
            );
            break;
        }
    }
    record
        .a_relay_subscription_is_live
        .store(false, Ordering::Release);
    drop(open_group);
    let _ = subgroups.close(moq_transport::serve::ServeError::Done);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_epoch_is_never_one_an_earlier_session_announced() {
        let first = a_fresh_announcement_epoch(None);
        let second = a_fresh_announcement_epoch(Some(first));
        let third = a_fresh_announcement_epoch(Some(second));
        assert!(second > first && third > second);
        assert!(first > 1_700_000_000, "the epoch is a unix second");
    }
}
