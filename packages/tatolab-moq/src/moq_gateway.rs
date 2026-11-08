// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The gateway's thread: it holds the one relay session, takes the track
//! declarations and receive requests its nodes send it, writes each declared
//! track's content to the broadcast and reads each requested track back.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iceoryx2::port::listener::Listener;
use iceoryx2::prelude::ipc_threadsafe;
use serde_json::Value;

use crate::error::{MoqModuleError, Result};
use crate::moq_catalog::{
    the_catalog_merged_from_the_entries_of,
    the_track_carries_its_timestamp_in_the_hang_legacy_container,
};
use crate::moq_gateway_receive_read::{
    MoqReceiveNodeContentSentByTheGateway, MoqTrackReceiveRequest, read_a_track_for_a_receive_node,
};
use crate::moq_gateway_relay_session::{MoqGatewayRelaySession, MoqGatewayTrackWriter};
use crate::moq_gateway_relay_settings::MoqGatewayRelaySettings;
use crate::moq_hang_legacy_container::write_hang_legacy_container_media_frame;
use crate::moq_node_ipc::{
    MoqGatewayControlMessage, MoqNodeIpc, MoqNodeSliceSubscriber, MoqTrackContentSampleKind,
    MoqTrackContentSentToTheGatewayHeader,
};
use crate::moq_node_thread::{MoqNodeThread, MoqNodeThreadStopSignal};

/// How long a track or a receive request lives on after its node last sent it.
pub const A_DECLARATION_EXPIRES_AFTER: Duration = Duration::from_secs(5);

/// The longest the gateway waits between passes when nothing wakes it: how
/// soon it sees new settings, a stop, and a declaration that expired.
const THE_GATEWAY_WAKES_AT_LEAST_EVERY: Duration = Duration::from_millis(20);

/// The most samples of one track's content a pass writes, so one busy track
/// never holds back the others, the control messages or a stop.
const CONTENT_SAMPLES_A_PASS_WRITES_PER_TRACK: usize = 64;

/// How long after a read ends a fresh one starts for the same request.
const A_READ_THAT_ENDED_STARTS_AGAIN_AFTER: Duration = Duration::from_secs(1);

/// How long a session that never connects is held before it is dialed afresh.
const A_SESSION_THAT_NEVER_CONNECTS_IS_REDIALED_AFTER: Duration = Duration::from_secs(10);

/// What the gateway takes its relay settings from: each call hands back the
/// newest settings it was sent since the last call, if any.
pub type MoqGatewaySettingsSource = Box<dyn FnMut() -> Option<MoqGatewayRelaySettings> + Send>;

/// The gateway's running thread.
pub struct MoqGatewayThread {
    thread: MoqNodeThread,
}

impl MoqGatewayThread {
    /// Start the gateway named `gateway_node` on `ipc`, dialing what
    /// `settings_source` hands it.
    pub fn spawn(
        ipc: MoqNodeIpc,
        gateway_node: String,
        settings_source: MoqGatewaySettingsSource,
    ) -> Result<Self> {
        let async_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moq-gateway-relay")
            .enable_all()
            .build()
            .map_err(|refusal| MoqModuleError::Relay {
                what: format!("no async runtime builds for the gateway: {refusal}"),
            })?;
        let gateway = MoqGatewayPasses::open(ipc, gateway_node, settings_source, async_runtime)?;
        let thread = MoqNodeThread::spawn("moq-gateway", move |stop_signal| {
            gateway.run_until(stop_signal)
        })?;
        Ok(Self { thread })
    }

    /// Stop the thread and wait up to `timeout` for it; whether it ended.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.thread.stop_and_join(timeout)
    }
}

/// One track node's declaration and the content its send node writes.
struct DeclaredTrack {
    track: String,
    catalog_entry_declared: Option<Value>,
    catalog_entry_upstream: Option<Value>,
    last_declared_at: Instant,
    content: MoqNodeSliceSubscriber<MoqTrackContentSentToTheGatewayHeader>,
    next_sequence_number: Option<u64>,
    writer: Option<MoqGatewayTrackWriter>,
    writer_refused_on_this_session: bool,
    /// The buffer each Packets unit is wrapped in hang's legacy container in,
    /// reused across the track's objects.
    hang_legacy_container_media_frame_bytes: Vec<u8>,
}

impl DeclaredTrack {
    fn catalog_entry(&self) -> Option<&Value> {
        self.catalog_entry_declared
            .as_ref()
            .or(self.catalog_entry_upstream.as_ref())
    }
}

/// One receive node's request and the read serving it.
struct RequestedReceive {
    request: MoqTrackReceiveRequest,
    last_requested_at: Instant,
    reading: Option<tokio::task::JoinHandle<()>>,
    next_read_may_start_at: Instant,
}

impl RequestedReceive {
    /// Whether a read still holds the receive node's content service, ended or
    /// aborted reads included until their task is gone.
    fn a_read_is_running(&self) -> bool {
        self.reading
            .as_ref()
            .is_some_and(|reading| !reading.is_finished())
    }

    fn abort_the_read(&self) {
        if let Some(reading) = &self.reading {
            reading.abort();
        }
    }
}

impl Drop for RequestedReceive {
    fn drop(&mut self) {
        self.abort_the_read();
    }
}

struct MoqGatewayPasses {
    ipc: MoqNodeIpc,
    gateway_node: String,
    settings_source: MoqGatewaySettingsSource,
    async_runtime: tokio::runtime::Runtime,
    wake: Listener<ipc_threadsafe::Service>,
    control: MoqNodeSliceSubscriber<()>,
    settings: Option<MoqGatewayRelaySettings>,
    session: Option<MoqGatewayRelaySession>,
    session_dialed_at: Instant,
    tracks: HashMap<String, DeclaredTrack>,
    declarations_refused_for_a_track_name_held: HashSet<(String, String)>,
    receives: HashMap<String, RequestedReceive>,
    the_catalog_may_have_changed: bool,
}

impl MoqGatewayPasses {
    fn open(
        ipc: MoqNodeIpc,
        gateway_node: String,
        settings_source: MoqGatewaySettingsSource,
        async_runtime: tokio::runtime::Runtime,
    ) -> Result<Self> {
        let wake = ipc.listener(&ipc.service_names.gateway_wake(&gateway_node))?;
        let control = ipc.gateway_control_subscriber(&gateway_node)?;
        Ok(Self {
            ipc,
            gateway_node,
            settings_source,
            async_runtime,
            wake,
            control,
            settings: None,
            session: None,
            session_dialed_at: Instant::now(),
            tracks: HashMap::new(),
            declarations_refused_for_a_track_name_held: HashSet::new(),
            receives: HashMap::new(),
            the_catalog_may_have_changed: false,
        })
    }

    fn run_until(mut self, stop_signal: &MoqNodeThreadStopSignal) {
        while !stop_signal.is_requested() {
            let now = Instant::now();
            self.take_the_newest_settings_and_redial_when_due(now);
            self.take_every_control_message(now);
            let content_is_left_to_write = self.write_every_tracks_content();
            self.expire_what_was_not_heard_from_since(now);
            self.publish_the_catalog(now);
            self.start_every_read_not_running(now);
            if !content_is_left_to_write {
                let _ = self
                    .wake
                    .timed_wait(|_| {}, THE_GATEWAY_WAKES_AT_LEAST_EVERY);
            }
        }
        self.receives.clear();
        self.tracks.clear();
        self.session = None;
        self.async_runtime.shutdown_timeout(Duration::from_secs(1));
    }

    fn take_the_newest_settings_and_redial_when_due(&mut self, now: Instant) {
        let mut redial = false;
        if let Some(settings) = (self.settings_source)()
            && self.settings.as_ref() != Some(&settings)
        {
            tracing::info!(
                "MoqGateway `{}`: dialing a {:?} relay",
                self.gateway_node,
                settings.relay
            );
            self.settings = Some(settings);
            redial = true;
        }
        if let Some(session) = &self.session {
            redial |= session.its_credential_is_due_at(now);
            redial |= !session.is_connected()
                && now >= self.session_dialed_at + A_SESSION_THAT_NEVER_CONNECTS_IS_REDIALED_AFTER;
        } else {
            redial |= self.settings.is_some()
                && now >= self.session_dialed_at + A_SESSION_THAT_NEVER_CONNECTS_IS_REDIALED_AFTER;
        }
        if redial {
            self.dial_afresh(now);
        }
    }

    fn dial_afresh(&mut self, now: Instant) {
        for receive in self.receives.values_mut() {
            receive.abort_the_read();
            receive.next_read_may_start_at = now;
        }
        for track in self.tracks.values_mut() {
            track.writer = None;
            track.writer_refused_on_this_session = false;
        }
        self.the_catalog_may_have_changed = true;
        self.session = None;
        self.session_dialed_at = now;
        let Some(settings) = &self.settings else {
            return;
        };
        let dialed = settings.dial_now().and_then(|dial| {
            let _inside_the_async_runtime = self.async_runtime.enter();
            MoqGatewayRelaySession::dial(dial)
        });
        match dialed {
            Ok(session) => self.session = Some(session),
            Err(refusal) => {
                tracing::error!(
                    "MoqGateway `{}`: the relay was not dialed: {refusal}",
                    self.gateway_node
                )
            }
        }
    }

    fn take_every_control_message(&mut self, now: Instant) {
        loop {
            let sample = match self.control.receive() {
                Ok(Some(sample)) => sample,
                Ok(None) => return,
                Err(refusal) => {
                    tracing::debug!("MoqGateway: the control service did not read: {refusal:?}");
                    return;
                }
            };
            match serde_json::from_slice::<MoqGatewayControlMessage>(sample.payload()) {
                Ok(message) => self.take_the_control_message(message, now),
                Err(refusal) => {
                    tracing::warn!("MoqGateway: a control message did not parse: {refusal}")
                }
            }
        }
    }

    fn take_the_control_message(&mut self, message: MoqGatewayControlMessage, now: Instant) {
        match message {
            MoqGatewayControlMessage::TrackDeclared {
                track_node,
                track,
                catalog_entry,
            } => {
                if let Some(declared) = self.tracks.get_mut(&track_node)
                    && declared.track == track
                {
                    if declared.catalog_entry_declared != catalog_entry {
                        declared.catalog_entry_declared = catalog_entry;
                        self.the_catalog_may_have_changed = true;
                    }
                    declared.last_declared_at = now;
                    return;
                }
                if let Some((holder, _)) = self
                    .tracks
                    .iter()
                    .find(|(holder, declared)| **holder != track_node && declared.track == track)
                {
                    if self
                        .declarations_refused_for_a_track_name_held
                        .insert((track_node.clone(), track.clone()))
                    {
                        tracing::warn!(
                            "MoqGateway `{}`: track node `{track_node}` declares `{track}`, \
                             which track node `{holder}` holds; it is refused",
                            self.gateway_node
                        );
                    }
                    return;
                }
                if self.tracks.remove(&track_node).is_some() {
                    self.the_catalog_may_have_changed = true;
                }
                let content = match self.ipc.track_content_subscriber(&track_node) {
                    Ok(content) => content,
                    Err(refusal) => {
                        tracing::warn!(
                            "MoqGateway: track node `{track_node}` is not read: {refusal}"
                        );
                        return;
                    }
                };
                tracing::info!(
                    "MoqGateway `{}`: track node `{track_node}` declares `{track}`",
                    self.gateway_node
                );
                self.tracks.insert(
                    track_node,
                    DeclaredTrack {
                        track,
                        catalog_entry_declared: catalog_entry,
                        catalog_entry_upstream: None,
                        last_declared_at: now,
                        content,
                        next_sequence_number: None,
                        writer: None,
                        writer_refused_on_this_session: false,
                        hang_legacy_container_media_frame_bytes: Vec::new(),
                    },
                );
                self.the_catalog_may_have_changed = true;
            }
            MoqGatewayControlMessage::TrackReceiveRequested {
                receive_node,
                broadcast,
                track,
                max_age_milliseconds,
            } => {
                let request = MoqTrackReceiveRequest {
                    receive_node: receive_node.clone(),
                    broadcast,
                    track,
                    max_age: Duration::from_millis(max_age_milliseconds),
                };
                if let Some(requested) = self.receives.get_mut(&receive_node)
                    && requested.request == request
                {
                    requested.last_requested_at = now;
                    return;
                }
                let read_still_holding_the_content_service = self
                    .receives
                    .remove(&receive_node)
                    .and_then(|mut replaced| {
                        replaced.abort_the_read();
                        replaced.reading.take()
                    });
                self.receives.insert(
                    receive_node,
                    RequestedReceive {
                        request,
                        last_requested_at: now,
                        reading: read_still_holding_the_content_service,
                        next_read_may_start_at: now,
                    },
                );
            }
        }
    }

    /// Write up to a buffer's worth of each track's content; whether any
    /// track's content was left unread for the next pass.
    fn write_every_tracks_content(&mut self) -> bool {
        let mut content_is_left_to_write = false;
        let session = self.session.as_ref();
        for (track_node, declared) in &mut self.tracks {
            if declared.writer.is_none()
                && !declared.writer_refused_on_this_session
                && let Some(session) = session
            {
                match session.broadcast.create_track(&declared.track) {
                    Ok(track) => declared.writer = Some(MoqGatewayTrackWriter::new(track)),
                    Err(refusal) => {
                        declared.writer_refused_on_this_session = true;
                        tracing::warn!("MoqGateway: `{track_node}`: {refusal}");
                    }
                }
            }
            let mut objects_ride_the_hang_legacy_container =
                the_track_carries_its_timestamp_in_the_hang_legacy_container(
                    declared.catalog_entry(),
                );
            let mut samples_left_this_pass = CONTENT_SAMPLES_A_PASS_WRITES_PER_TRACK;
            loop {
                if samples_left_this_pass == 0 {
                    content_is_left_to_write = true;
                    break;
                }
                samples_left_this_pass -= 1;
                let sample = match declared.content.receive() {
                    Ok(Some(sample)) => sample,
                    Ok(None) => break,
                    Err(refusal) => {
                        tracing::debug!(
                            "MoqGateway: `{track_node}`'s content did not read: {refusal:?}"
                        );
                        break;
                    }
                };
                let header = *sample.user_header();
                let a_sample_was_overwritten = declared
                    .next_sequence_number
                    .is_some_and(|expected| header.sequence_number != expected);
                declared.next_sequence_number = Some(header.sequence_number + 1);
                if a_sample_was_overwritten && let Some(writer) = &mut declared.writer {
                    writer.give_up_the_group_being_written();
                }
                if header.kind
                    == MoqTrackContentSampleKind::HangCatalogEntryBuiltFromUpstreamStreamFacts
                        as u32
                {
                    let catalog_entry_upstream = if sample.payload().is_empty() {
                        None
                    } else {
                        serde_json::from_slice(sample.payload()).ok()
                    };
                    if declared.catalog_entry_upstream != catalog_entry_upstream {
                        declared.catalog_entry_upstream = catalog_entry_upstream;
                        self.the_catalog_may_have_changed = true;
                        objects_ride_the_hang_legacy_container =
                            the_track_carries_its_timestamp_in_the_hang_legacy_container(
                                declared.catalog_entry(),
                            );
                    }
                    continue;
                }
                let Some(writer) = &mut declared.writer else {
                    continue;
                };
                if header.lost_since_last_read > 0 {
                    writer.give_up_the_group_being_written();
                }
                let object_payload: &[u8] = if objects_ride_the_hang_legacy_container {
                    if let Err(refusal) = write_hang_legacy_container_media_frame(
                        &mut declared.hang_legacy_container_media_frame_bytes,
                        header.timestamp_ns,
                        sample.payload(),
                    ) {
                        tracing::debug!("MoqGateway: `{track_node}`: {refusal}");
                        writer.give_up_the_group_being_written();
                        continue;
                    }
                    &declared.hang_legacy_container_media_frame_bytes
                } else {
                    sample.payload()
                };
                writer.write_object(
                    object_payload,
                    header.timestamp_ns,
                    header.starts_group != 0,
                );
            }
        }
        content_is_left_to_write
    }

    fn expire_what_was_not_heard_from_since(&mut self, now: Instant) {
        let tracks_before = self.tracks.len();
        self.tracks
            .retain(|_, declared| now < declared.last_declared_at + A_DECLARATION_EXPIRES_AFTER);
        if self.tracks.len() != tracks_before {
            self.the_catalog_may_have_changed = true;
            self.declarations_refused_for_a_track_name_held.clear();
        }
        self.receives
            .retain(|_, requested| now < requested.last_requested_at + A_DECLARATION_EXPIRES_AFTER);
    }

    fn publish_the_catalog(&mut self, now: Instant) {
        let Some(session) = &mut self.session else {
            return;
        };
        if std::mem::take(&mut self.the_catalog_may_have_changed) {
            let catalog =
                the_catalog_merged_from_the_entries_of(self.tracks.values().filter_map(
                    |declared| Some((declared.track.as_str(), declared.catalog_entry()?)),
                ));
            session.broadcast.publish_the_catalog(&catalog);
        }
        if let Some(every) = session.dialed.catalog_rewritten_whole_every {
            session
                .broadcast
                .rewrite_the_catalog_whole_when_due(every, now);
        }
    }

    fn start_every_read_not_running(&mut self, now: Instant) {
        let Some(session) = &self.session else {
            return;
        };
        for (receive_node, requested) in &mut self.receives {
            if requested.a_read_is_running() || now < requested.next_read_may_start_at {
                continue;
            }
            requested.next_read_may_start_at = now + A_READ_THAT_ENDED_STARTS_AGAIN_AFTER;
            let content = self
                .ipc
                .receive_content_publisher(receive_node)
                .and_then(|publisher| {
                    let notifier = self
                        .ipc
                        .notifier(&self.ipc.service_names.receive_wake(receive_node))?;
                    Ok(MoqReceiveNodeContentSentByTheGateway::new(
                        publisher, notifier,
                    ))
                });
            let content = match content {
                Ok(content) => content,
                Err(refusal) => {
                    tracing::warn!(
                        "MoqGateway: receive node `{receive_node}` is not served: {refusal}"
                    );
                    continue;
                }
            };
            let announced_broadcasts = session.announced_broadcasts.clone();
            let request = requested.request.clone();
            requested.reading = Some(self.async_runtime.spawn(async move {
                let receive_node = request.receive_node.clone();
                let why_it_ended =
                    read_a_track_for_a_receive_node(announced_broadcasts, request, content).await;
                tracing::info!("MoqGateway: the read for `{receive_node}` stopped: {why_it_ended}");
            }));
        }
    }
}
