// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The gateway's thread: it holds the one relay session, takes the track
//! declarations and receive requests its nodes send it, writes each declared
//! track's content to the broadcast and reads each requested track back.

use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iceoryx2::port::listener::Listener;
use iceoryx2::prelude::ipc_threadsafe;
use serde_json::Value;

use crate::error::{MoqModuleError, Result};
use crate::moq_catalog::the_catalog_merged_from_the_entries_of;
use crate::moq_gateway_receive_read::{
    MoqReceiveNodeContentSentByTheGateway, MoqTrackReceiveRequest, read_a_track_for_a_receive_node,
};
use crate::moq_gateway_relay_session::{MoqGatewayRelaySession, MoqGatewayTrackWriter};
use crate::moq_gateway_relay_settings::MoqGatewayRelaySettings;
use crate::moq_node_ipc::{
    MoqGatewayControlMessage, MoqNodeIpc, MoqNodeSliceSubscriber, MoqTrackContentSampleKind,
    MoqTrackContentSentToTheGatewayHeader,
};

/// How long a track or a receive request lives on after its node last sent it.
pub const A_DECLARATION_EXPIRES_AFTER: Duration = Duration::from_secs(5);

/// The longest the gateway sleeps between passes when nothing wakes it.
const THE_GATEWAY_WAKES_AT_LEAST_EVERY: Duration = Duration::from_millis(5);

/// How long after a read ends a fresh one starts for the same request.
const A_READ_THAT_ENDED_STARTS_AGAIN_AFTER: Duration = Duration::from_secs(1);

/// How long a session that never connects is held before it is dialed afresh.
const A_SESSION_THAT_NEVER_CONNECTS_IS_REDIALED_AFTER: Duration = Duration::from_secs(10);

/// What the gateway takes its relay settings from: each call hands back the
/// newest settings it was sent since the last call, if any.
pub type MoqGatewaySettingsSource = Box<dyn FnMut() -> Option<MoqGatewayRelaySettings> + Send>;

/// The gateway's running thread.
pub struct MoqGatewayThread {
    stop_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
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
        let stop_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_requested_on_the_thread = std::sync::Arc::clone(&stop_requested);
        let thread = std::thread::Builder::new()
            .name("moq-gateway".into())
            .spawn(move || gateway.run_until(&stop_requested_on_the_thread))
            .map_err(|refusal| MoqModuleError::Relay {
                what: format!("the gateway thread did not start: {refusal}"),
            })?;
        Ok(Self {
            stop_requested,
            thread: Some(thread),
        })
    }

    /// Stop the thread and wait up to `timeout` for it; whether it ended.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.stop_requested
            .store(true, std::sync::atomic::Ordering::Release);
        let Some(thread) = self.thread.take() else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
        true
    }
}

impl Drop for MoqGatewayThread {
    fn drop(&mut self) {
        self.stop_and_join(Duration::from_secs(2));
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

impl Drop for RequestedReceive {
    fn drop(&mut self) {
        if let Some(reading) = self.reading.take() {
            reading.abort();
        }
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
    receives: HashMap<String, RequestedReceive>,
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
            receives: HashMap::new(),
        })
    }

    fn run_until(mut self, stop_requested: &std::sync::atomic::AtomicBool) {
        while !stop_requested.load(std::sync::atomic::Ordering::Acquire) {
            let now = Instant::now();
            self.take_the_newest_settings_and_redial_when_due(now);
            self.take_every_control_message(now);
            self.write_every_tracks_content();
            self.expire_what_was_not_heard_from_since(now);
            self.publish_the_catalog(now);
            self.start_every_read_not_running(now);
            let _ = self
                .wake
                .timed_wait(|_| {}, THE_GATEWAY_WAKES_AT_LEAST_EVERY);
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
            if let Some(reading) = receive.reading.take() {
                reading.abort();
            }
        }
        for track in self.tracks.values_mut() {
            track.writer = None;
        }
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
                    declared.catalog_entry_declared = catalog_entry;
                    declared.last_declared_at = now;
                    return;
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
                    },
                );
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
                self.receives.insert(
                    receive_node,
                    RequestedReceive {
                        request,
                        last_requested_at: now,
                        reading: None,
                        next_read_may_start_at: now,
                    },
                );
            }
        }
    }

    fn write_every_tracks_content(&mut self) {
        let session = self.session.as_ref();
        for (track_node, declared) in &mut self.tracks {
            if declared.writer.is_none()
                && let Some(session) = session
            {
                match session.broadcast.create_track(&declared.track) {
                    Ok(track) => declared.writer = Some(MoqGatewayTrackWriter::new(track)),
                    Err(refusal) => tracing::warn!("MoqGateway: `{track_node}`: {refusal}"),
                }
            }
            loop {
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
                if header.kind == MoqTrackContentSampleKind::UpstreamCatalogEntry as u32 {
                    declared.catalog_entry_upstream = if sample.payload().is_empty() {
                        None
                    } else {
                        serde_json::from_slice(sample.payload()).ok()
                    };
                    continue;
                }
                let Some(writer) = &mut declared.writer else {
                    continue;
                };
                if a_sample_was_overwritten || header.lost_since_last_read > 0 {
                    writer.give_up_the_group_being_written();
                }
                writer.write_object(
                    sample.payload(),
                    header.timestamp_ns,
                    header.starts_group != 0,
                );
            }
        }
    }

    fn expire_what_was_not_heard_from_since(&mut self, now: Instant) {
        self.tracks
            .retain(|_, declared| now < declared.last_declared_at + A_DECLARATION_EXPIRES_AFTER);
        self.receives
            .retain(|_, requested| now < requested.last_requested_at + A_DECLARATION_EXPIRES_AFTER);
    }

    fn publish_the_catalog(&mut self, now: Instant) {
        let Some(session) = &mut self.session else {
            return;
        };
        let catalog = the_catalog_merged_from_the_entries_of(
            self.tracks
                .values()
                .filter_map(|declared| Some((declared.track.as_str(), declared.catalog_entry()?))),
        );
        session.broadcast.publish_the_catalog(&catalog);
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
            if requested
                .reading
                .as_ref()
                .is_some_and(|reading| !reading.is_finished())
                || now < requested.next_read_may_start_at
            {
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
