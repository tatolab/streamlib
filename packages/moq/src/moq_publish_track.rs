// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Publish Track — forwards raw bytes from a graph input to a named MoQ track.

use crate::_generated_::EncodedVideoFrame;
use streamlib_moq::{sessions_for_runtime, MoqPublishSession, SharedMoqSessions};
use parking_lot::Mutex;
use std::sync::Arc;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};

#[streamlib::sdk::processor("MoqPublishTrack")]
pub struct MoqPublishTrackProcessor {
    shared_publish_session: Option<Arc<Mutex<MoqPublishSession>>>,
    sessions: Option<SharedMoqSessions>,
    track_name: String,
    frames_published: u64,
    /// Plugin-owned tokio runtime. Constructed in `setup()`; the host's
    /// runtime is not reachable across the plugin ABI per #885.
    /// Used to drive `get_publish_session().await` from sync `setup()`.
    tokio_runtime: Option<tokio::runtime::Runtime>,
}

impl streamlib::sdk::processors::ReactiveProcessor for MoqPublishTrackProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| {
                Error::Runtime(format!(
                    "MoqPublishTrack: failed to build tokio runtime: {e}"
                ))
            })?;

        // Track name: explicit config, or auto-generate from processor id.
        self.track_name = self.config.track_name.clone().unwrap_or_else(|| {
            ctx.processor_id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "default".to_string())
        });

        let sessions = sessions_for_runtime(&ctx.runtime_id().to_string());
        let session = runtime.block_on(sessions.get_publish_session())?;
        sessions.register_published_track(&self.track_name);

        tracing::info!(
            broadcast = %sessions.broadcast_path(),
            track = %self.track_name,
            "[MoqPublishTrack] Using shared session"
        );

        self.shared_publish_session = Some(session);
        self.sessions = Some(sessions);
        self.tokio_runtime = Some(runtime);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_published = self.frames_published,
            "[MoqPublishTrack] Shutting down"
        );
        self.shared_publish_session.take();
        self.sessions.take();
        self.tokio_runtime.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("data_in") {
            return Ok(());
        }

        let raw_data = match self.inputs.read_raw("data_in") {
            Ok(Some(data)) => data,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };

        let (bytes, _timestamp_ns) = raw_data;

        let mut session = self
            .shared_publish_session
            .as_ref()
            .ok_or_else(|| Error::Runtime("MoQ session not connected".into()))?
            .lock();

        // Detect keyframe on the "video" track by checking the
        // `is_keyframe` field in the msgpack-encoded EncodedVideoFrame.
        // Scanning for NAL patterns on raw bytes produces false positives
        // on the msgpack envelope; deserializing just the flag is reliable.
        let is_keyframe = if self.track_name == "video" {
            rmp_serde::from_slice::<EncodedVideoFrame>(&bytes)
                .map(|frame| frame.is_keyframe)
                .unwrap_or(false)
        } else {
            // Non-video tracks: all frames go into one subgroup.
            false
        };

        session.publish_frame(&self.track_name, &bytes, is_keyframe)?;

        self.frames_published += 1;
        if self.frames_published == 1 {
            tracing::info!(
                track = %self.track_name,
                "[MoqPublishTrack] First frame published"
            );
        } else if self.frames_published % 100 == 0 {
            tracing::info!(
                frames = self.frames_published,
                "[MoqPublishTrack] Publish progress"
            );
        }

        Ok(())
    }
}
