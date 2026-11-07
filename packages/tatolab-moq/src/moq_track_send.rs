// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A track node's declaration on its gateway, and a send node's writes to the
//! track it names.

use std::time::{Duration, Instant};

use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::ipc_threadsafe;

use crate::error::{MoqModuleError, Result};
use crate::moq_node_ipc::{
    MoqGatewayControlMessage, MoqNodeIpc, MoqNodeSlicePublisher, MoqNodeSliceSubscriber,
    MoqTrackContentSampleKind, MoqTrackContentSentToTheGatewayHeader, MoqTrackDescription,
    publish_slice,
};
use crate::moq_node_thread::MoqNodeThread;

/// How often a running track node declares its track again.
pub const A_TRACK_IS_DECLARED_AGAIN_EVERY: Duration = Duration::from_secs(1);

/// What a track node declares.
#[derive(Debug, Clone)]
pub struct MoqTrackDeclared {
    pub track_node: String,
    pub gateway_node: String,
    pub track: String,
    /// `{"section", "entry"}`, replacing the entry of the port feeding the send.
    pub catalog_entry: Option<serde_json::Value>,
}

/// The track node's running declaration thread.
pub struct MoqTrackDeclarationThread {
    thread: MoqNodeThread,
}

impl MoqTrackDeclarationThread {
    /// Publish `declared`'s description and declare it on its gateway every
    /// second until stopped.
    pub fn spawn(ipc: MoqNodeIpc, declared: MoqTrackDeclared) -> Result<Self> {
        let description = ipc.track_description_publisher(&declared.track_node)?;
        let description_payload = serde_json::to_vec(&MoqTrackDescription {
            gateway_node: declared.gateway_node.clone(),
        })
        .map_err(refused_serialization)?;
        publish_slice(&description, (), &description_payload)?;
        let control = ipc.gateway_control_publisher(&declared.gateway_node)?;
        let gateway_wake = ipc.notifier(&ipc.service_names.gateway_wake(&declared.gateway_node))?;
        let declaration = serde_json::to_vec(&MoqGatewayControlMessage::TrackDeclared {
            track_node: declared.track_node.clone(),
            track: declared.track.clone(),
            catalog_entry: declared.catalog_entry.clone(),
        })
        .map_err(refused_serialization)?;
        let thread = MoqNodeThread::spawn("moq-track-declaration", move |stop_signal| {
            let _held_while_the_track_is_declared = (ipc, description);
            loop {
                match publish_slice(&control, (), &declaration) {
                    Ok(()) => {
                        let _ = gateway_wake.notify();
                    }
                    Err(refusal) => {
                        tracing::debug!("MoqTrack: the declaration was not sent: {refusal}")
                    }
                }
                if stop_signal.wait_for_a_stop_up_to(A_TRACK_IS_DECLARED_AGAIN_EVERY) {
                    return;
                }
            }
        })?;
        Ok(Self { thread })
    }

    /// Stop declaring the track and wait for the thread.
    pub fn stop_and_join(&mut self) {
        self.thread.stop_and_join(Duration::from_secs(2));
    }
}

fn refused_serialization(refusal: serde_json::Error) -> MoqModuleError {
    MoqModuleError::Refused {
        what: format!("a control message does not serialize: {refusal}"),
    }
}

/// A send node's writes to its track's content service, reaching the gateway
/// the track names once its description arrives.
pub struct MoqTrackSendPublisher {
    ipc: MoqNodeIpc,
    description: MoqNodeSliceSubscriber<()>,
    content: MoqNodeSlicePublisher<MoqTrackContentSentToTheGatewayHeader>,
    gateway_wake: Option<Notifier<ipc_threadsafe::Service>>,
    sequence_number: u64,
    upstream_catalog_entry_sent: Option<(Option<serde_json::Value>, Instant)>,
}

impl MoqTrackSendPublisher {
    /// Write to the track `track_node` declares.
    pub fn open(ipc: MoqNodeIpc, track_node: &str) -> Result<Self> {
        Ok(Self {
            description: ipc.track_description_subscriber(track_node)?,
            content: ipc.track_content_publisher(track_node)?,
            ipc,
            gateway_wake: None,
            sequence_number: 0,
            upstream_catalog_entry_sent: None,
        })
    }

    /// Hand the gateway one message, waking it once the track names one.
    pub fn send_message(
        &mut self,
        payload: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
        lost_since_last_read: u64,
    ) -> Result<()> {
        self.send(
            MoqTrackContentSampleKind::Object,
            payload,
            timestamp_ns,
            starts_group,
            lost_since_last_read,
        )
    }

    /// Hand the gateway the entry of the port feeding the send, `{"section",
    /// "entry"}`, when it changed since the last one handed over, and again
    /// every second for a gateway that was not yet reading the track.
    pub fn send_the_upstream_catalog_entry_when_it_changed(
        &mut self,
        section_and_entry: Option<serde_json::Value>,
    ) -> Result<()> {
        let now = Instant::now();
        if let Some((sent, sent_at)) = &self.upstream_catalog_entry_sent
            && *sent == section_and_entry
            && now < *sent_at + A_TRACK_IS_DECLARED_AGAIN_EVERY
        {
            return Ok(());
        }
        let payload = section_and_entry
            .as_ref()
            .map(|entry| entry.to_string().into_bytes())
            .unwrap_or_default();
        self.send(
            MoqTrackContentSampleKind::UpstreamCatalogEntry,
            &payload,
            0,
            false,
            0,
        )?;
        self.upstream_catalog_entry_sent = Some((section_and_entry, now));
        Ok(())
    }

    fn send(
        &mut self,
        kind: MoqTrackContentSampleKind,
        payload: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
        lost_since_last_read: u64,
    ) -> Result<()> {
        let user_header = MoqTrackContentSentToTheGatewayHeader {
            sequence_number: self.sequence_number,
            lost_since_last_read,
            timestamp_ns,
            kind: kind as u32,
            starts_group: u32::from(starts_group),
        };
        self.sequence_number += 1;
        publish_slice(&self.content, user_header, payload)?;
        self.wake_the_gateway();
        Ok(())
    }

    fn wake_the_gateway(&mut self) {
        if self.gateway_wake.is_none()
            && let Ok(Some(sample)) = self.description.receive()
            && let Ok(description) = serde_json::from_slice::<MoqTrackDescription>(sample.payload())
        {
            self.gateway_wake = self
                .ipc
                .notifier(
                    &self
                        .ipc
                        .service_names
                        .gateway_wake(&description.gateway_node),
                )
                .ok();
        }
        if let Some(gateway_wake) = &self.gateway_wake {
            let _ = gateway_wake.notify();
        }
    }
}
