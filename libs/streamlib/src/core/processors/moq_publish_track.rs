// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Publish Track
//
// Forwards raw bytes from a single graph input to a named MoQ track.
// Type-agnostic: uses read_raw() to receive any serialized data type
// and publishes the bytes as-is to the shared MoQ relay session.

use crate::core::streaming::MoqPublishSession;
use crate::core::{Result, RuntimeContext, StreamError};
use parking_lot::Mutex;
use std::sync::Arc;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_publish_track")]
pub struct MoqPublishTrackProcessor {
    /// Shared MoQ publish session (from RuntimeContext).
    shared_publish_session: Option<Arc<Mutex<MoqPublishSession>>>,

    /// Resolved track name (from config or auto-generated).
    track_name: String,

    /// Frames published counter.
    frames_published: u64,
}

impl crate::core::ReactiveProcessor for MoqPublishTrackProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        // Track name: use config value or auto-generate from processor ID
        self.track_name = self.config.track_name.clone().unwrap_or_else(|| {
            ctx.processor_id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "default".to_string())
        });

        // Get the shared publish session from the runtime (one QUIC connection, N tracks)
        let session = ctx.moq_sessions().get_publish_session().await?;

        // Register this track in the catalog
        ctx.moq_sessions().register_published_track(&self.track_name);

        tracing::info!(
            broadcast = %ctx.moq_sessions().broadcast_path(),
            track = %self.track_name,
            "[MoqPublishTrack] Using shared session"
        );

        self.shared_publish_session = Some(session);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_published = self.frames_published,
            "[MoqPublishTrack] Shutting down"
        );
        self.shared_publish_session.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
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
            .ok_or_else(|| StreamError::Runtime("MoQ session not connected".into()))?
            .lock();

        // Detect keyframe: scan for Annex B start code (0x00000001) followed by SPS NAL (type 7).
        // The bytes are msgpack-serialized Encodedvideoframe — the NAL data is embedded in
        // the binary field, so we search within the full payload.
        let is_keyframe = bytes.windows(5).any(|w| {
            w[0] == 0 && w[1] == 0 && w[2] == 0 && w[3] == 1 && (w[4] & 0x1F) == 7
        });

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
